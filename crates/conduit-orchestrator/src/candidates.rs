//! CandidateSelector — resolve request model/channel/APIFormat candidates
//! (RUST-P9-003, steps S04–S09; RUST-P9-006 S10 top-level flow).
//!
//! Ported from the Go orchestrator:
//! - `conduit/internal/server/orchestrator/candidates.go` (`DefaultSelector`:
//!   `Select`, `selectChannelCadidates` legacy path, `selectModelCandidates`
//!   model/association path, `resolveAssociations` caching, `aggregate...`,
//!   `modelAssociationSignature`; the `SelectedChannelsSelector`,
//!   `TagsFilterSelector` and `LoadBalancedSelector` decorators).
//! - `conduit/internal/server/orchestrator/select_candidates.go`
//!   (`selectCandidates` middleware body — decorator wiring order, empty-result
//!   error semantics, `areAllChannelsExhausted`).
//! - `conduit/internal/server/orchestrator/candidates_quota.go`
//!   (`ProviderQuotaSelector`) + `biz/provider_quota.go`
//!   (`QuotaChannelStatus.EffectiveStatus`, `quotaStatusRank`) +
//!   `biz/provider_quota/types.go` (`RequestModality`, `QuotaLimitType`).
//! - `conduit/internal/server/orchestrator/candidates_condition.go`
//!   (`filterResolvedCandidatesForRequest` — When/condition evaluation,
//!   `populateAPIFormat`, `estimatePromptTokens`, content-feature detection).
//! - `conduit/internal/server/orchestrator/select_endpoints.go`
//!   (`SelectAPIFormat`).
//! - `conduit/internal/server/biz/model_association_matcher.go`
//!   (`MatchAssociations` + the six branch matchers, dedup tracker).
//!
//! This module is **pure logic**: it takes borrowed snapshots of the enabled
//! channels (as `ChannelSnapshot`) plus the effective association list and a
//! [`CandidateRequest`] view of the inbound request, and produces the resolved
//! [`ChannelModelsCandidate`] list. No IO, no async — fully unit-testable with
//! in-memory fixtures. The cache key is computed here too, but the actual TTL
//! store is the caller's responsibility (S07 produces the key + a 5m constant).

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};

use conduit_core::objects::apikey::match_channel_tags;
use conduit_core::objects::channel_settings::{
    ChannelEndpoint, ChannelPolicies, ChannelSettings, capability_policy,
};
use conduit_core::objects::model_association::condition_field as cf;
use conduit_core::objects::{
    ModelAssociation, ModelAssociationWhen, SystemModelSettings, condition::evaluate,
};
use conduit_llm::RequestType;
use conduit_services::ProviderQuotaEnforcementMode;
use conduit_services::channel_service::{ChannelModelEntry, ChannelModelEntryMap};
use regex::Regex;
use serde_json::Value;

use crate::load_balancer::RetryPolicy;

// ---------------------------------------------------------------------------
// Inputs (borrowed snapshots)
// ---------------------------------------------------------------------------

/// Minimal, logic-facing view of a channel for candidate resolution. Built by
/// the caller from `ChannelRepo::list_enabled_channels` rows; intentionally
/// cheap to clone so the selector can hold snapshots for the cache.
///
/// `id` mirrors the Go `Channel.ID` (string in Rust). `model_entries` is the
/// precomputed [`ChannelModelEntryMap`] for the channel (Go `GetModelEntries`),
/// and `resolved_endpoints` is the merged endpoint list (Go `ResolveEndpoints`).
///
/// `credential_key_identity` and `policies` carry the S14 dedup key dimension
/// (credential-key identity) and the S13 stream-policy dimension respectively.
/// Both default empty so legacy fixtures that do not set them keep compiling.
#[derive(Debug, Clone)]
pub struct ChannelSnapshot {
    pub id: String,
    pub name: String,
    /// Go `Channel.OrderingWeight`: channel-level load-balancer weight.
    /// This is deliberately separate from [`ChannelModelsCandidate::priority`],
    /// which is the model-association priority (lower value wins before load
    /// balancing is applied within that priority group).
    pub ordering_weight: i64,
    pub tags: Vec<String>,
    pub updated_at: String,
    pub model_entries: ChannelModelEntryMap,
    pub resolved_endpoints: Vec<ChannelEndpoint>,
    /// Stable identity of the active credential key (Go: selected API key
    /// fingerprint). Part of the S14 dedup key so two keys on the same channel
    /// are treated as distinct real-provider targets. Empty when unknown, in
    /// which case dedup falls back to channel id only.
    pub credential_key_identity: String,
    /// Per-capability policies (Go `Channel.Policies`). Drives the S11
    /// `stream_policy` filter stage.
    pub policies: ChannelPolicies,
    /// Go `Channel.Type` (e.g. `"gemini"`, `"anthropic"`). Drives the S10
    /// native-tools capability gate. Empty when unset; treated as
    /// not-native-capable.
    pub channel_type: String,
    /// Go `Channel.BaseURL`. Carried so the resolved candidate can stamp the
    /// outbound URL (WIRE-06 path C). `None` when the channel has no base URL.
    pub base_url: Option<String>,
    /// The already-resolved active credential (plaintext API key) for the
    /// channel, mirroring Go's per-channel `APIKeyProvider.Get` result
    /// (`channel_llm.go` `getAPIKeyProvider`). `None` for OAuth/Azure/GCP
    /// channels (their auth materializes in the transformer layer) or when
    /// no enabled key exists.
    ///
    /// ⚠ Plaintext secret: must stay in-memory only — never log it and never
    /// embed it in error text.
    pub active_credential: Option<String>,
    /// P-17: the full enabled-key set for the channel (Go
    /// `Credentials.GetEnabledAPIKeys`), carried so credential selection can be
    /// **deferred to request-execution time** when the per-request trace exists.
    /// `active_credential` above is the no-trace deterministic pick used when no
    /// trace is available; when a trace *is* present, the orchestrator re-picks
    /// trace-sticky from this list (Go `TraceStickyKeyProvider`). Empty for
    /// OAuth/Azure/GCP channels and single-legacy-key channels (where the single
    /// `active_credential` is already correct).
    ///
    /// ⚠ Plaintext secrets: in-memory only — never log or embed in error text.
    pub enabled_credentials: Vec<String>,
    /// Full channel settings used by request middleware after selection.
    pub settings: Option<ChannelSettings>,
}

impl ChannelSnapshot {
    /// Convenience accessor for the stream capability policy, defaulting to
    /// `unlimited` when unset (mirrors Go `streamPolicyOf`).
    pub fn stream_policy(&self) -> &str {
        if self.policies.stream.is_empty() {
            capability_policy::UNLIMITED
        } else {
            self.policies.stream.as_str()
        }
    }
}

/// Request view consumed by the selector. Mirrors the Go `*llm.Request` fields
/// the selector actually reads (`Model`, `Stream`, `APIFormat`, `RequestType`,
/// `Messages`). Kept as plain data so tests can build it inline.
#[derive(Debug, Clone)]
pub struct CandidateRequest {
    pub model: String,
    pub stream: bool,
    pub api_format: String,
    pub request_type: RequestType,
    pub messages: Vec<RequestMessage>,
    pub tools: Vec<RequestTool>,
    /// Client-declared output ceiling used by wallet admission to reserve the
    /// worst-case retail charge before an upstream request is sent. `None`
    /// means the endpoint/model default is used by the admission adapter.
    pub max_output_tokens: Option<u32>,
    /// Whether the request carries an image payload (Go `req.Image != nil`).
    /// Drives the quota limit type via [`request_modality`]
    /// (`select_candidates.go` line 130 / `candidates_quota.go` line 54).
    pub is_image_request: bool,
    pub project_channel_ids: Vec<String>,
    /// Offer-derived channel -> concrete upstream model mapping for the
    /// requested canonical model. Empty keeps the legacy association path.
    pub project_upstream_models_by_channel: BTreeMap<String, String>,
    pub project_channel_tags: Vec<String>,
    pub project_channel_tags_match_mode: String,
    pub key_channel_ids: Vec<String>,
    pub key_channel_tags: Vec<String>,
    pub key_channel_tags_match_mode: String,
}

impl CandidateRequest {
    pub fn new(
        model: impl Into<String>,
        request_type: RequestType,
        api_format: impl Into<String>,
    ) -> Self {
        Self {
            model: model.into(),
            stream: false,
            api_format: api_format.into(),
            request_type,
            messages: Vec::new(),
            tools: Vec::new(),
            max_output_tokens: None,
            is_image_request: false,
            project_channel_ids: Vec::new(),
            project_upstream_models_by_channel: BTreeMap::new(),
            project_channel_tags: Vec::new(),
            project_channel_tags_match_mode: String::new(),
            key_channel_ids: Vec::new(),
            key_channel_tags: Vec::new(),
            key_channel_tags_match_mode: String::new(),
        }
    }

    pub const fn with_stream(mut self, stream: bool) -> Self {
        self.stream = stream;
        self
    }

    pub fn with_messages(mut self, messages: Vec<RequestMessage>) -> Self {
        self.messages = messages;
        self
    }

    /// Mark the request as an image request (Go `req.Image != nil`).
    pub const fn with_image_request(mut self, is_image_request: bool) -> Self {
        self.is_image_request = is_image_request;
        self
    }
}

/// Minimal message view for prompt-token estimation + content-feature
/// detection. Mirrors the subset of Go `llm.ChatMessage` the selector reads.
#[derive(Debug, Clone, Default)]
pub struct RequestMessage {
    pub role: String,
    pub name: Option<String>,
    pub tool_call_id: Option<String>,
    pub tool_call_name: Option<String>,
    pub reasoning_content: Option<String>,
    pub content: RequestMessageContent,
    pub tool_calls: Vec<RequestToolCall>,
}

impl RequestMessage {
    pub fn text(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: RequestMessageContent::Text(content.into()),
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RequestMessageContent {
    #[default]
    Empty,
    Text(String),
    Parts(Vec<RequestContentPart>),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RequestContentPart {
    pub part_type: String,
    pub text: Option<String>,
    pub image_url: Option<String>,
    pub video_url: Option<String>,
    pub document: Option<String>,
    pub input_audio: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct RequestToolCall {
    pub call_type: String,
    pub function_name: String,
    pub function_arguments: String,
}

#[derive(Debug, Clone, Default)]
pub struct RequestTool {
    pub tool_type: String,
    pub function_name: String,
    pub function_description: String,
    pub function_parameters: String,
}

// ---------------------------------------------------------------------------
// Outputs
// ---------------------------------------------------------------------------

/// A resolved channel candidate and its matched model entries. Mirrors Go
/// `ChannelModelsCandidate`. `api_format` is the selected endpoint format
/// (Go `SelectAPIFormat`); empty when none resolved.
///
/// `channel_type`, `policies`, and `credential_key_identity` carry the S10
/// native-tools-capability, S11 stream-policy, and S14 dedup dimensions
/// respectively. They mirror fields on the Go `biz.Channel` that the Go
/// `ChannelModelsCandidate` exposes via its embedded `*biz.Channel` pointer.
/// Both `policies` and `credential_key_identity` default empty so legacy
/// fixtures that do not set them keep compiling.
#[derive(Debug, Clone, PartialEq)]
pub struct ChannelModelsCandidate {
    pub channel_id: String,
    pub channel_name: String,
    /// Channel-level ordering weight consumed by the load balancer. This must
    /// not be conflated with `priority`, which comes from a model association.
    pub ordering_weight: i64,
    pub priority: i64,
    pub models: Vec<ChannelModelEntry>,
    /// Complete endpoint selected for this candidate. Unlike the legacy
    /// `api_format` mirror below, this preserves endpoint-level path, base URL
    /// and transport overrides for the outbound attempt.
    pub endpoint: ChannelEndpoint,
    /// Compatibility mirror of [`Self::endpoint.api_format`]. New outbound
    /// wiring should consume `endpoint` so the rest of its metadata is not
    /// discarded.
    pub api_format: String,
    /// Go `Channel.Type`. Drives the S10 native-tools capability gate (Go
    /// `ChannelType.SupportsGoogleNativeTools` / `SupportsAnthropicNativeTools`).
    /// Empty when unset (treated as not-native-capable).
    pub channel_type: String,
    /// Go `Channel.Policies`. Drives the S11 stream-policy filter stage.
    pub policies: ChannelPolicies,
    /// Stable identity of the active credential key (Go: selected API key
    /// fingerprint). Carried for parity with Go's `ChannelModelsCandidate`
    /// (which reaches it through `Channel`); the S14 dedup key already uses
    /// the snapshot's value at aggregation time, so this field is informational
    /// on the public candidate.
    pub credential_key_identity: String,
    /// Go `Channel.Tags` (reached through the embedded `*biz.Channel`).
    /// Drives the S10 profile channel-tags filter (Go `TagsFilterSelector`,
    /// `candidates.go` lines 723-742). Empty when the channel has no tags.
    pub tags: Vec<String>,
    /// Go `Channel.BaseURL`, projected from the [`ChannelSnapshot`] so the
    /// pipeline can stamp the outbound URL for this candidate (WIRE-06).
    pub base_url: Option<String>,
    /// The resolved active credential for the channel (see
    /// [`ChannelSnapshot::active_credential`]). Plaintext, in-memory only —
    /// never log or embed in error text.
    pub active_credential: Option<String>,
    /// P-17: full enabled-key set (see [`ChannelSnapshot::enabled_credentials`]),
    /// carried so the orchestrator can defer trace-sticky selection to request
    /// time. Plaintext secrets — in-memory only.
    pub enabled_credentials: Vec<String>,
    /// Channel settings propagated to the pipeline's per-attempt metadata.
    pub settings: Option<ChannelSettings>,
    /// Request-scoped theoretical procurement cost in accounting currency.
    /// This is an estimate for routing only; settlement always uses provider
    /// usage after the request completes.
    pub theoretical_cost_accounting: Option<String>,
    /// Cost score normalized within this request's admitted candidates.
    /// Higher is cheaper, in the inclusive range 0..=1000.
    pub cost_efficiency_score: i64,
}

impl ChannelModelsCandidate {
    /// Per-candidate stream capability policy, defaulting to `unlimited` when
    /// unset. Mirrors Go `streamPolicyOf(candidate)`.
    pub fn stream_policy(&self) -> &str {
        if self.policies.stream.is_empty() {
            capability_policy::UNLIMITED
        } else {
            self.policies.stream.as_str()
        }
    }

    /// Mirrors Go `ChannelType.SupportsGoogleNativeTools`. Google native tools
    /// (`google_*`) are only supported by native Gemini API format channels
    /// (`gemini`, `gemini_vertex`); OpenAI-compatible endpoints
    /// (`gemini_openai`) do NOT support them.
    pub fn supports_google_native_tools(&self) -> bool {
        matches!(self.channel_type.as_str(), "gemini" | "gemini_vertex")
    }

    /// Mirrors Go `ChannelType.SupportsAnthropicNativeTools`. Anthropic native
    /// tools (`web_search_20250305`) are only supported by direct Anthropic
    /// API channels; Bedrock/Vertex (`anthropic_aws`/`anthropic_gcp`) are
    /// included, but format-suffixed variants (`deepseek_anthropic`,
    /// `moonshot_anthropic`) are NOT.
    pub fn supports_anthropic_native_tools(&self) -> bool {
        matches!(
            self.channel_type.as_str(),
            "anthropic" | "anthropic_aws" | "anthropic_gcp" | "claudecode"
        )
    }
}

/// Intermediate association-resolution form retaining the originating `When`
/// condition, mirroring Go `resolvedAssociationCandidate`. The
/// request-dependent `When` filtering runs in a dedicated pass after structural
/// association matching, exactly like Go.
#[derive(Debug, Clone)]
struct ResolvedAssociationCandidate {
    channel: ChannelSnapshot,
    priority: i64,
    models: Vec<ChannelModelEntry>,
    when: Option<ModelAssociationWhen>,
}

/// Reason the selector returned no model-based candidates.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CandidateSelectionError {
    /// The requested model has no Conduit API model entry, and the system setting
    /// `fallback_to_channels_on_model_not_found` is `false`. Mirrors Go
    /// `ErrInvalidModel`.
    #[error("model not found and fallback disabled: {model:?}")]
    ModelNotFound { model: String },
}

// ---------------------------------------------------------------------------
// Association matching (port of biz.MatchAssociations + branch matchers)
// ---------------------------------------------------------------------------

/// A single association's resolved connections (mirrors Go `AssociationMatch`).
#[derive(Debug, Clone)]
struct AssociationMatch {
    when: Option<ModelAssociationWhen>,
    connections: Vec<MatchedConnection>,
}

#[derive(Debug, Clone)]
struct MatchedConnection {
    /// Index into the channels slice (avoids cloning the snapshot here; the
    /// caller resolves it into a `ChannelSnapshot` after the structural match).
    channel_idx: usize,
    models: Vec<ChannelModelEntry>,
    priority: i64,
}

/// Dedup tracker over `(channel_id, request_model)` pairs. Mirrors Go
/// `DuplicateKeyTracker` (which keys on the request/association model id, not
/// the resolved actual model). Shared across the whole association list so the
/// same channel/request-model combination is only produced once during the
/// structural match pass.
#[derive(Debug, Default)]
struct DuplicateTracker {
    seen: BTreeSet<(String, String)>,
}

impl DuplicateTracker {
    fn add(&mut self, channel_id: &str, request_model: &str) -> bool {
        self.seen
            .insert((channel_id.to_string(), request_model.to_string()))
    }
}

/// Match effective associations against the enabled-channel snapshots. Mirrors
/// Go `MatchAssociations`: disabled associations are skipped, dedup is global,
/// and the originating association's `When` is retained per match group.
fn match_associations(
    associations: &[ModelAssociation],
    channels: &[ChannelSnapshot],
) -> Vec<AssociationMatch> {
    let mut tracker = DuplicateTracker::default();
    let mut matches = Vec::with_capacity(associations.len());
    for assoc in associations {
        if assoc.disabled {
            continue;
        }
        let connections = match_single_association(assoc, channels, &mut tracker);
        if connections.is_empty() {
            continue;
        }
        matches.push(AssociationMatch {
            when: assoc.when.clone(),
            connections,
        });
    }
    matches
}

fn match_single_association(
    assoc: &ModelAssociation,
    channels: &[ChannelSnapshot],
    tracker: &mut DuplicateTracker,
) -> Vec<MatchedConnection> {
    match assoc.kind.as_str() {
        "channel_model" => match_channel_model(assoc, channels, tracker),
        "channel_regex" => match_channel_regex(assoc, channels, tracker),
        "regex" => match_regex(assoc, channels, tracker),
        "model" => match_model(assoc, channels, tracker),
        "channel_tags_model" => match_channel_tags_model(assoc, channels, tracker),
        "channel_tags_regex" => match_channel_tags_regex(assoc, channels, tracker),
        _ => Vec::new(),
    }
}

fn find_channel(channels: &[ChannelSnapshot], id: i64) -> Option<(usize, &ChannelSnapshot)> {
    let want = id.to_string();
    channels.iter().enumerate().find(|(_, ch)| ch.id == want)
}

fn match_channel_model(
    assoc: &ModelAssociation,
    channels: &[ChannelSnapshot],
    tracker: &mut DuplicateTracker,
) -> Vec<MatchedConnection> {
    let Some(branch) = &assoc.channel_model else {
        return Vec::new();
    };
    let Some((idx, ch)) = find_channel(channels, branch.channel_id) else {
        return Vec::new();
    };
    let Some(entry) = ch.model_entries.get(&branch.model_id) else {
        return Vec::new();
    };
    if !tracker.add(&ch.id, &branch.model_id) {
        return Vec::new();
    }
    vec![MatchedConnection {
        channel_idx: idx,
        models: vec![entry.clone()],
        priority: assoc.priority,
    }]
}

fn match_channel_regex(
    assoc: &ModelAssociation,
    channels: &[ChannelSnapshot],
    tracker: &mut DuplicateTracker,
) -> Vec<MatchedConnection> {
    let Some(branch) = &assoc.channel_regex else {
        return Vec::new();
    };
    let Some((idx, ch)) = find_channel(channels, branch.channel_id) else {
        return Vec::new();
    };
    let Ok(re) = Regex::new(&branch.pattern) else {
        return Vec::new();
    };
    let mut models = Vec::new();
    for (model_id, entry) in ch.model_entries.iter() {
        if re.is_match(model_id) && tracker.add(&ch.id, model_id) {
            models.push(entry.clone());
        }
    }
    if models.is_empty() {
        return Vec::new();
    }
    vec![MatchedConnection {
        channel_idx: idx,
        models,
        priority: assoc.priority,
    }]
}

fn match_regex(
    assoc: &ModelAssociation,
    channels: &[ChannelSnapshot],
    tracker: &mut DuplicateTracker,
) -> Vec<MatchedConnection> {
    let Some(branch) = &assoc.regex else {
        return Vec::new();
    };
    let Ok(re) = Regex::new(&branch.pattern) else {
        return Vec::new();
    };
    let mut connections = Vec::new();
    for (idx, ch) in channels.iter().enumerate() {
        if should_exclude_channel(ch, &branch.exclude) {
            continue;
        }
        let mut models = Vec::new();
        for (model_id, entry) in ch.model_entries.iter() {
            if re.is_match(model_id) && tracker.add(&ch.id, model_id) {
                models.push(entry.clone());
            }
        }
        if !models.is_empty() {
            connections.push(MatchedConnection {
                channel_idx: idx,
                models,
                priority: assoc.priority,
            });
        }
    }
    connections
}

fn match_model(
    assoc: &ModelAssociation,
    channels: &[ChannelSnapshot],
    tracker: &mut DuplicateTracker,
) -> Vec<MatchedConnection> {
    let Some(branch) = &assoc.model_id else {
        return Vec::new();
    };
    let mut connections = Vec::new();
    for (idx, ch) in channels.iter().enumerate() {
        if should_exclude_channel(ch, &branch.exclude) {
            continue;
        }
        let Some(entry) = ch.model_entries.get(&branch.model_id) else {
            continue;
        };
        if !tracker.add(&ch.id, &branch.model_id) {
            continue;
        }
        connections.push(MatchedConnection {
            channel_idx: idx,
            models: vec![entry.clone()],
            priority: assoc.priority,
        });
    }
    connections
}

fn match_channel_tags_model(
    assoc: &ModelAssociation,
    channels: &[ChannelSnapshot],
    tracker: &mut DuplicateTracker,
) -> Vec<MatchedConnection> {
    let Some(branch) = &assoc.channel_tags_model else {
        return Vec::new();
    };
    if branch.channel_tags.is_empty() {
        return Vec::new();
    }
    let mut connections = Vec::new();
    for (idx, ch) in channels.iter().enumerate() {
        if !channel_has_any_tag(ch, &branch.channel_tags) {
            continue;
        }
        let Some(entry) = ch.model_entries.get(&branch.model_id) else {
            continue;
        };
        if !tracker.add(&ch.id, &branch.model_id) {
            continue;
        }
        connections.push(MatchedConnection {
            channel_idx: idx,
            models: vec![entry.clone()],
            priority: assoc.priority,
        });
    }
    connections
}

fn match_channel_tags_regex(
    assoc: &ModelAssociation,
    channels: &[ChannelSnapshot],
    tracker: &mut DuplicateTracker,
) -> Vec<MatchedConnection> {
    let Some(branch) = &assoc.channel_tags_regex else {
        return Vec::new();
    };
    if branch.channel_tags.is_empty() {
        return Vec::new();
    }
    let Ok(re) = Regex::new(&branch.pattern) else {
        return Vec::new();
    };
    let mut connections = Vec::new();
    for (idx, ch) in channels.iter().enumerate() {
        if !channel_has_any_tag(ch, &branch.channel_tags) {
            continue;
        }
        let mut models = Vec::new();
        for (model_id, entry) in ch.model_entries.iter() {
            if re.is_match(model_id) && tracker.add(&ch.id, model_id) {
                models.push(entry.clone());
            }
        }
        if !models.is_empty() {
            connections.push(MatchedConnection {
                channel_idx: idx,
                models,
                priority: assoc.priority,
            });
        }
    }
    connections
}

fn channel_has_any_tag(ch: &ChannelSnapshot, tags: &[String]) -> bool {
    tags.iter().any(|t| ch.tags.iter().any(|ct| ct == t))
}

/// Port of Go `shouldExcludeChannel`. A channel is excluded if any exclude rule
/// matches its name pattern, id, or tags.
fn should_exclude_channel(
    ch: &ChannelSnapshot,
    excludes: &[conduit_core::objects::ExcludeAssociation],
) -> bool {
    if excludes.is_empty() {
        return false;
    }
    let id_i64 = ch.id.parse::<i64>().ok();
    for exclude in excludes {
        if !exclude.channel_name_pattern.is_empty()
            && let Ok(re) = Regex::new(&exclude.channel_name_pattern)
            && re.is_match(&ch.name)
        {
            return true;
        }
        if !exclude.channel_ids.is_empty()
            && let Some(id) = id_i64
            && exclude.channel_ids.contains(&id)
        {
            return true;
        }
        if !exclude.channel_tags.is_empty()
            && exclude.channel_tags.iter().any(|t| ch.tags.contains(t))
        {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// APIFormat selection (port of orchestrator.SelectAPIFormat)
// ---------------------------------------------------------------------------

/// API formats capable of serving each request type. Mirrors the Go
/// `*CapableAPIFormats` maps.
fn capable_formats(rt: RequestType) -> &'static [&'static str] {
    match rt {
        RequestType::Chat => &[
            "openai/chat_completions",
            "openai/responses",
            "anthropic/messages",
            "gemini/contents",
            "ollama/chat",
        ],
        RequestType::Compact => &["openai/responses_compact"],
        RequestType::Completion => &["openai/completions"],
        RequestType::Embedding => &["openai/embeddings", "jina/embeddings", "gemini/embeddings"],
        RequestType::Image => &[
            "openai/image_generation",
            "openai/image_edit",
            "openai/image_variation",
        ],
        RequestType::Rerank => &["jina/rerank"],
        RequestType::Video => &["openai/video", "seedance/video"],
        RequestType::Speech => &["openai/audio_speech"],
        RequestType::Transcription => &["openai/audio_transcriptions"],
        RequestType::Translation => &["openai/audio_translations"],
    }
}

/// Select the complete endpoint best matching the request. Mirrors Go
/// `SelectAPIFormat`: prefer a capable endpoint that equals the inbound format
/// (pass-through), then any capable endpoint, then the first endpoint.
///
/// Returning the endpoint itself is important: selecting only its format loses
/// endpoint-specific path, base URL and transport overrides before execution.
pub fn select_endpoint<'a>(
    endpoints: &'a [ChannelEndpoint],
    req: &CandidateRequest,
) -> Option<&'a ChannelEndpoint> {
    let allowed = capable_formats(req.request_type);
    if !req.api_format.is_empty() {
        for ep in endpoints {
            if allowed.contains(&ep.api_format.as_str()) && ep.api_format == req.api_format {
                return Some(ep);
            }
        }
    }
    for ep in endpoints {
        if allowed.contains(&ep.api_format.as_str()) {
            return Some(ep);
        }
    }
    endpoints.first()
}

/// Compatibility wrapper for callers that only need the selected format.
pub fn select_api_format(endpoints: &[ChannelEndpoint], req: &CandidateRequest) -> String {
    select_endpoint(endpoints, req)
        .map(|endpoint| endpoint.api_format.clone())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Condition filtering (port of candidates_condition.go)
// ---------------------------------------------------------------------------

/// Detected request content features, mirroring Go `requestContentFeatures`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ContentFeatures {
    has_image: bool,
    has_video: bool,
    has_document: bool,
    has_audio: bool,
}

impl ContentFeatures {
    fn detect(req: &CandidateRequest) -> Self {
        let mut f = Self::default();
        for msg in &req.messages {
            if let RequestMessageContent::Parts(parts) = &msg.content {
                for part in parts {
                    if part.image_url.is_some() {
                        f.has_image = true;
                    }
                    if part.video_url.is_some() {
                        f.has_video = true;
                    }
                    if part.document.is_some() {
                        f.has_document = true;
                    }
                    if part.input_audio.is_some() {
                        f.has_audio = true;
                    }
                    if f.has_image && f.has_video && f.has_document && f.has_audio {
                        return f;
                    }
                }
            }
        }
        f
    }
}

/// Evaluate an association's `When` clause against the request-derived data.
/// Mirrors Go `matchesAssociationWhen`: `None` or disabled `When` matches;
/// otherwise the embedded condition is evaluated.
fn matches_when(
    when: Option<&ModelAssociationWhen>,
    prompt_tokens: i64,
    stream: bool,
    request_format: &str,
    features: ContentFeatures,
    now_rfc3339: &str,
) -> bool {
    let Some(when) = when else {
        return true;
    };
    if !when.enabled {
        return true;
    }
    let Some(condition) = &when.condition else {
        return true;
    };
    let data = serde_json::json!({
        cf::PROMPT_TOKENS: prompt_tokens,
        cf::STREAM: stream,
        cf::REQUEST_FORMAT: request_format,
        cf::HAS_IMAGE: features.has_image,
        cf::HAS_VIDEO: features.has_video,
        cf::HAS_DOCUMENT: features.has_document,
        cf::HAS_AUDIO: features.has_audio,
        "now": now_rfc3339,
    });
    evaluate(condition, &data)
}

/// Estimate prompt tokens for condition evaluation. Mirrors Go
/// `estimatePromptTokens` (simplified heuristic, not the provider tokenizer).
pub fn estimate_prompt_tokens(req: &CandidateRequest) -> i64 {
    let mut total = 0i64;
    for msg in &req.messages {
        total += estimate_tokens(&msg.role);
        total += msg.name.as_deref().map_or(0, estimate_tokens);
        total += count_message_content(&msg.content);
        total += msg.tool_call_id.as_deref().map_or(0, estimate_tokens);
        total += msg.tool_call_name.as_deref().map_or(0, estimate_tokens);
        total += msg.reasoning_content.as_deref().map_or(0, estimate_tokens);
        for call in &msg.tool_calls {
            total += estimate_tokens(&call.call_type);
            total += estimate_tokens(&call.function_name);
            total += estimate_tokens(&call.function_arguments);
        }
    }
    for tool in &req.tools {
        total += estimate_tokens(&tool.tool_type);
        total += estimate_tokens(&tool.function_name);
        total += estimate_tokens(&tool.function_description);
        total += estimate_tokens(&tool.function_parameters);
    }
    total
}

fn count_message_content(content: &RequestMessageContent) -> i64 {
    match content {
        RequestMessageContent::Empty => 0,
        RequestMessageContent::Text(t) => estimate_tokens(t),
        RequestMessageContent::Parts(parts) => {
            let mut tokens = 0i64;
            for part in parts {
                tokens += estimate_tokens(&part.part_type);
                if let Some(t) = &part.text {
                    tokens += estimate_tokens(t);
                }
                if part.image_url.is_some() {
                    tokens += 128;
                }
                if part.video_url.is_some() {
                    tokens += 128;
                }
                if part.document.is_some() {
                    tokens += 128;
                }
                if part.input_audio.is_some() {
                    tokens += 128;
                }
            }
            tokens
        }
    }
}

/// Rough token estimate: CJK chars count ~1.5/char, other non-space chars ~4/char.
/// Ported from Go `estimateTokens`.
fn estimate_tokens(value: &str) -> i64 {
    if value.is_empty() {
        return 0;
    }
    let mut cjk = 0f64;
    let mut other = 0f64;
    for r in value.chars() {
        if is_cjk(r) {
            cjk += 1.0;
        } else if r.is_whitespace() {
            continue;
        } else {
            other += 1.0;
        }
    }
    let total = (cjk / 1.5) + (other / 4.0);
    if total == 0.0 { 1 } else { total as i64 }
}

fn is_cjk(r: char) -> bool {
    // Mirrors Go `isCJK` ranges.
    matches!(r as u32,
        0x3400..=0x9FFF   // CJK + Han extension A (covers Han, Hiragana, Katakana within unified ranges)
        | 0x3040..=0x30FF // Hiragana + Katakana
        | 0xAC00..=0xD7A3 // Hangul syllables
        | 0x1100..=0x11FF // Hangul Jamo
    )
}

/// Aggregate resolved association candidates into the public candidate list,
/// deduplicating per `(channel_id, endpoint/api_format, credential_key_identity,
/// actual_model)`. Mirrors Go `aggregateChannelModelCandidates`, extended with
/// the S14 dedup dimensions: the selected endpoint (`api_format`) and the active
/// credential-key identity. When `credential_key_identity` is empty the dedup
/// collapses to the Go `(channel_id, actual_model)` behavior, so legacy
/// fixtures without a credential key still behave like Go.
///
/// Candidate grouping is by `(channel_id, priority, api_format)`: two resolved
/// entries that map to the same `(channel, actual_model)` but a different
/// `api_format` stay distinct candidates (they target different endpoints).
/// Within a single candidate the model list is deduped on `actual_model` only
/// (Go parity). The `seen` set additionally carries `credential_key_identity`
/// so two keys on the same channel are not collapsed — but because the public
/// `ChannelModelsCandidate` does not expose the credential key, the two keys
/// collapse into the same candidate row when their `api_format` matches. That
/// is acceptable for selection; the outbound key picker re-resolves the real
/// key. `[Euclid-the-2nd ?]` TODO: surface `credential_key_identity` on
/// `ChannelModelsCandidate` once the credential snapshot type lands.
fn aggregate_candidates(
    resolved: &[ResolvedAssociationCandidate],
    req: &CandidateRequest,
) -> Vec<ChannelModelsCandidate> {
    let mut candidates: Vec<ChannelModelsCandidate> = Vec::new();
    // Per-(channel_id, api_format, credential_key, actual_model) dedup across
    // associations. S14 requires the dedup key to cover at least
    // `channel_id + endpoint + credential-key-identity`.
    let mut seen: BTreeSet<(String, String, String, String)> = BTreeSet::new();

    for resolved_cand in resolved {
        let endpoint = select_endpoint(&resolved_cand.channel.resolved_endpoints, req)
            .cloned()
            .unwrap_or_default();
        let api_format = endpoint.api_format.clone();
        for entry in &resolved_cand.models {
            let key = (
                resolved_cand.channel.id.clone(),
                api_format.clone(),
                resolved_cand.channel.credential_key_identity.clone(),
                entry.actual_model.clone(),
            );
            if seen.contains(&key) {
                continue;
            }
            seen.insert(key);
            // Group by (channel_id, priority, api_format); within the group,
            // dedup models by actual_model.
            let pos = candidates.iter().position(|c| {
                c.channel_id == resolved_cand.channel.id
                    && c.priority == resolved_cand.priority
                    && c.api_format == api_format
            });
            match pos {
                Some(i) => {
                    if !candidates[i]
                        .models
                        .iter()
                        .any(|m| m.actual_model == entry.actual_model)
                    {
                        candidates[i].models.push(entry.clone());
                    }
                }
                None => {
                    candidates.push(ChannelModelsCandidate {
                        channel_id: resolved_cand.channel.id.clone(),
                        channel_name: resolved_cand.channel.name.clone(),
                        ordering_weight: resolved_cand.channel.ordering_weight,
                        priority: resolved_cand.priority,
                        models: vec![entry.clone()],
                        endpoint: endpoint.clone(),
                        api_format: api_format.clone(),
                        channel_type: resolved_cand.channel.channel_type.clone(),
                        policies: resolved_cand.channel.policies.clone(),
                        credential_key_identity: resolved_cand
                            .channel
                            .credential_key_identity
                            .clone(),
                        tags: resolved_cand.channel.tags.clone(),
                        base_url: resolved_cand.channel.base_url.clone(),
                        active_credential: resolved_cand.channel.active_credential.clone(),
                        // P-17: carry the full enabled-key set so credential
                        // selection can be deferred to request-execution time
                        // (when the trace id exists) instead of the snapshot's
                        // no-trace `enabled[0]` pick.
                        enabled_credentials: resolved_cand.channel.enabled_credentials.clone(),
                        settings: resolved_cand.channel.settings.clone(),
                        theoretical_cost_accounting: None,
                        cost_efficiency_score: 0,
                    });
                }
            }
        }
    }
    candidates
}

/// Port of Go `filterResolvedCandidatesForRequest`. When-clauses are evaluated
/// only after structural association resolution; associations without a `When`
/// always pass.
fn filter_resolved_for_request(
    resolved: &[ResolvedAssociationCandidate],
    req: &CandidateRequest,
    now_rfc3339: &str,
) -> Vec<ChannelModelsCandidate> {
    if resolved.is_empty() {
        return Vec::new();
    }
    let has_conditional = resolved.iter().any(|c| c.when.is_some());
    if !has_conditional {
        return aggregate_candidates(resolved, req);
    }
    let prompt_tokens = estimate_prompt_tokens(req);
    let features = ContentFeatures::detect(req);
    let filtered: Vec<ResolvedAssociationCandidate> = resolved
        .iter()
        .filter(|c| {
            matches_when(
                c.when.as_ref(),
                prompt_tokens,
                req.stream,
                &req.api_format,
                features,
                now_rfc3339,
            )
        })
        .cloned()
        .collect();
    aggregate_candidates(&filtered, req)
}

// ---------------------------------------------------------------------------
// Association signature + cache key (S07)
// ---------------------------------------------------------------------------

/// TTL for the association-resolution cache. Mirrors Go `associationCacheTTL`.
pub const ASSOCIATION_CACHE_TTL_SECS: u64 = 5 * 60;

/// Stable signature of an effective association list, mirroring Go
/// `modelAssociationSignature` (FNV-64a over the deterministic serialization of
/// each association's branch + when/condition). The signature is part of the
/// cache key so any change to the association graph invalidates the cache.
pub fn model_association_signature(associations: &[ModelAssociation]) -> u64 {
    let mut h = FnvHasher::new();
    h.write_usize(associations.len());
    for assoc in associations {
        write_association_signature(&mut h, assoc);
    }
    h.finish()
}

fn write_association_signature(h: &mut FnvHasher, assoc: &ModelAssociation) {
    h.write_str(&assoc.kind);
    h.write_i64(assoc.priority);
    h.write_bool(assoc.disabled);
    write_when_signature(h, assoc.when.as_ref());
    if let Some(b) = &assoc.channel_model {
        h.write_str("channelModel");
        h.write_str(&b.channel_id.to_string());
        h.write_str(&b.model_id);
    }
    if let Some(b) = &assoc.channel_regex {
        h.write_str("channelRegex");
        h.write_str(&b.channel_id.to_string());
        h.write_str(&b.pattern);
    }
    if let Some(b) = &assoc.regex {
        h.write_str("regex");
        h.write_str(&b.pattern);
        write_exclude_signature(h, &b.exclude);
    }
    if let Some(b) = &assoc.model_id {
        h.write_str("modelId");
        h.write_str(&b.model_id);
        write_exclude_signature(h, &b.exclude);
    }
    if let Some(b) = &assoc.channel_tags_model {
        h.write_str("channelTagsModel");
        h.write_str_slice(&b.channel_tags);
        h.write_str(&b.model_id);
    }
    if let Some(b) = &assoc.channel_tags_regex {
        h.write_str("channelTagsRegex");
        h.write_str_slice(&b.channel_tags);
        h.write_str(&b.pattern);
    }
}

fn write_when_signature(h: &mut FnvHasher, when: Option<&ModelAssociationWhen>) {
    match when {
        None => h.write_str("when:nil"),
        Some(w) => {
            h.write_str("when");
            h.write_bool(w.enabled);
            write_condition_signature(h, w.condition.as_ref());
        }
    }
}

fn write_condition_signature(h: &mut FnvHasher, cond: Option<&conduit_core::objects::Condition>) {
    match cond {
        None => {
            h.write_str("condition:nil");
        }
        Some(c) => {
            h.write_str(format!("{:?}", c.r#type).as_str());
            h.write_str(&c.logic);
            h.write_str(&c.field);
            h.write_str(&c.operator);
            h.write_value(&c.value);
            h.write_usize(c.conditions.len());
            for child in &c.conditions {
                write_condition_signature(h, Some(child));
            }
        }
    }
}

fn write_exclude_signature(
    h: &mut FnvHasher,
    excludes: &[conduit_core::objects::ExcludeAssociation],
) {
    h.write_usize(excludes.len());
    for ex in excludes {
        h.write_str(&ex.channel_name_pattern);
        h.write_str_slice(
            &ex.channel_ids
                .iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>(),
        );
        h.write_str_slice(&ex.channel_tags);
    }
}

/// Minimal FNV-64a hasher matching Go `hash/fnv`. Ported inline so the
/// signature is reproducible without pulling in a `hash` crate.
struct FnvHasher {
    state: u64,
}

impl FnvHasher {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;

    fn new() -> Self {
        Self {
            state: Self::OFFSET,
        }
    }

    fn write_bytes(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.state ^= u64::from(b);
            self.state = self.state.wrapping_mul(Self::PRIME);
        }
        // Separator byte, mirroring Go `writeSignatureString`'s trailing NUL.
        self.state ^= 0;
        self.state = self.state.wrapping_mul(Self::PRIME);
    }

    fn write_str(&mut self, s: &str) {
        self.write_bytes(s.as_bytes());
    }

    fn write_bool(&mut self, b: bool) {
        self.write_str(if b { "1" } else { "0" });
    }

    fn write_i64(&mut self, v: i64) {
        self.write_str(&v.to_string());
    }

    fn write_usize(&mut self, v: usize) {
        self.write_str(&v.to_string());
    }

    fn write_str_slice(&mut self, values: &[String]) {
        self.write_usize(values.len());
        for v in values {
            self.write_str(v);
        }
    }

    fn write_value(&mut self, value: &Option<Value>) {
        let s = match value {
            None => "<nil>".to_string(),
            Some(v) => v.to_string(),
        };
        self.write_str(&s);
    }

    fn finish(self) -> u64 {
        self.state
    }
}

/// Cache key for association resolution. Mirrors the validity tuple checked in
/// Go `resolveAssociations` (model id + association signature + channel count +
/// latest channel/model update + channel cache version). The TTL
/// ([`ASSOCIATION_CACHE_TTL_SECS`]) is checked separately by the caller.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AssociationCacheKey {
    pub model_id: String,
    pub association_signature: u64,
    pub channel_count: usize,
    pub latest_channel_update: String,
    pub latest_model_update: String,
    pub channel_cache_version: u64,
}

impl AssociationCacheKey {
    pub fn new(
        model_id: impl Into<String>,
        association_signature: u64,
        channel_count: usize,
        latest_channel_update: impl Into<String>,
        latest_model_update: impl Into<String>,
        channel_cache_version: u64,
    ) -> Self {
        Self {
            model_id: model_id.into(),
            association_signature,
            channel_count,
            latest_channel_update: latest_channel_update.into(),
            latest_model_update: latest_model_update.into(),
            channel_cache_version,
        }
    }
}

/// Latest `updated_at` among channels, mirroring Go
/// `getLatestChannelUpdateTime`. Empty string when there are none.
pub fn latest_channel_update(channels: &[ChannelSnapshot]) -> String {
    channels
        .iter()
        .map(|c| c.updated_at.as_str())
        .max()
        .unwrap_or_default()
        .to_string()
}

// ---------------------------------------------------------------------------
// Selector
// ---------------------------------------------------------------------------

/// Effective-association provider. Implementations resolve the requested Conduit API
/// model id into its developer-inherited association list (see
/// `conduit_services::model_service::effective_model_associations`). Kept as a
/// trait so the selector stays pure-logic and testable with fixtures.
pub trait AssociationSource {
    /// Resolve the effective associations for `requested_model_id`. Returns
    /// `None` when the model has no Conduit API entry (Go `GetModelByModelID`
    /// not-found), signalling the legacy fallback path.
    fn resolve(&self, requested_model_id: &str) -> Option<EffectiveModel>;

    /// System-wide model settings, used to decide the fallback path when a
    /// model is not found. Mirrors Go `SystemService.ModelSettingsOrDefault`.
    fn system_settings(&self) -> SystemModelSettings;
}

/// Result of association resolution: the effective association list plus the
/// metadata needed to build a stable cache key.
#[derive(Debug, Clone)]
pub struct EffectiveModel {
    pub model_id: String,
    pub developer: String,
    pub updated_at: String,
    pub associations: Vec<ModelAssociation>,
    pub system_settings: SystemModelSettings,
}

// ---------------------------------------------------------------------------
// S12: unified selection inputs (Go `DefaultSelector.Select` implicit deps)
// ---------------------------------------------------------------------------

/// Unified input bag for candidate selection. Mirrors the implicit inputs the
/// Go `DefaultSelector.Select(ctx, req)` reads through its receiver fields
/// (`ChannelService`, `ModelService`, `SystemService`) plus the request-scoped
/// profile filtering wired in `select_candidates.go`. Bundling them here keeps
/// the selector entry point self-describing and lets callers build the bag once
/// per inbound request (project/api-key profile are resolved upstream by the
/// HTTP handler and passed in via [`FilterContext`]).
///
/// Fields (Go mapping in parens):
/// - `request` — the inbound `*llm.Request` view (`Model`, `Stream`,
///   `APIFormat`, `RequestType`, `Messages`, `Tools`).
/// - `channels` — all enabled channels (`biz.ChannelService.GetEnabledChannels`).
/// - `associations` — effective association + system-settings provider
///   (`biz.ModelService` + `biz.SystemService.ModelSettingsOrDefault`).
/// - `profile` — project/key profile + request flags consumed by the
///   request-scoped [`FilterPipeline`] (`select_candidates.go` decorator chain).
/// - `now_rfc3339` — evaluation timestamp for `daily_time` conditions.
///
/// `system_settings` is intentionally NOT a separate field: it is already
/// exposed through [`AssociationSource::system_settings`], mirroring Go where
/// `SystemService` is a peer of `ModelService` on the selector receiver. Keeping
/// a single source of truth avoids drift between the two representations.
#[derive(Clone)]
pub struct SelectionInputs<'a> {
    pub request: &'a CandidateRequest,
    pub channels: &'a [ChannelSnapshot],
    pub associations: &'a dyn AssociationSource,
    pub profile: FilterContext,
    pub now_rfc3339: &'a str,
}

impl<'a> std::fmt::Debug for SelectionInputs<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SelectionInputs")
            .field("request", &self.request)
            .field("channels_len", &self.channels.len())
            .field("profile", &self.profile)
            .field("now_rfc3339", &self.now_rfc3339)
            .finish_non_exhaustive()
    }
}

impl<'a> SelectionInputs<'a> {
    /// Build the inputs with a default (no-op) profile. Callers that need
    /// project/key filtering set the `profile` field afterwards or use
    /// [`Self::with_profile`].
    pub fn new(
        request: &'a CandidateRequest,
        channels: &'a [ChannelSnapshot],
        associations: &'a dyn AssociationSource,
        now_rfc3339: &'a str,
    ) -> Self {
        Self {
            request,
            channels,
            associations,
            profile: FilterContext::from_request(request),
            now_rfc3339,
        }
    }

    /// Replace the profile context. Used by callers that resolve the
    /// project/api-key profile upstream and want to override the
    /// request-derived defaults (e.g. add `project_channel_ids`).
    pub fn with_profile(mut self, profile: FilterContext) -> Self {
        self.profile = profile;
        self
    }
}

/// Pure-logic candidate selector. Mirrors Go `DefaultSelector.Select`:
/// 1. Resolve the Conduit API model; if not found and fallback is enabled, run the
///    legacy channel-selection path (S05). If not found and fallback disabled,
///    return [`CandidateSelectionError::ModelNotFound`] (S04).
/// 2. Otherwise resolve the effective associations against the enabled channels
///    (S06), evaluate request-dependent `When` conditions (condition filter),
///    and aggregate/dedup (S09).
#[derive(Debug, Clone, Default)]
pub struct CandidateSelector;

impl CandidateSelector {
    /// Run selection. `now_rfc3339` is the evaluation timestamp for
    /// `daily_time` conditions (callers pass the current time; tests pass a
    /// fixed value).
    pub fn select(
        &self,
        req: &CandidateRequest,
        channels: &[ChannelSnapshot],
        associations: &dyn AssociationSource,
        now_rfc3339: &str,
    ) -> Result<Vec<ChannelModelsCandidate>, CandidateSelectionError> {
        match associations.resolve(&req.model) {
            Some(effective) => {
                let candidates =
                    self.select_model_candidates(req, channels, &effective, now_rfc3339);
                Ok(candidates)
            }
            None => {
                // No Conduit API model: consult the system settings to decide
                // whether legacy channel selection is allowed (S04).
                if associations
                    .system_settings()
                    .fallback_to_channels_on_model_not_found
                {
                    Ok(self.select_legacy(req, channels))
                } else {
                    Err(CandidateSelectionError::ModelNotFound {
                        model: req.model.clone(),
                    })
                }
            }
        }
    }

    /// Legacy channel selection (S05): every enabled channel whose model-entry
    /// map contains the requested model produces one candidate, priority 0.
    /// Mirrors Go `selectChannelCadidates`.
    pub fn select_legacy(
        &self,
        req: &CandidateRequest,
        channels: &[ChannelSnapshot],
    ) -> Vec<ChannelModelsCandidate> {
        let mut candidates = Vec::new();
        for ch in channels {
            let Some(entry) = ch.model_entries.get(&req.model) else {
                continue;
            };
            let endpoint = select_endpoint(&ch.resolved_endpoints, req)
                .cloned()
                .unwrap_or_default();
            candidates.push(ChannelModelsCandidate {
                channel_id: ch.id.clone(),
                channel_name: ch.name.clone(),
                ordering_weight: ch.ordering_weight,
                priority: 0,
                models: vec![entry.clone()],
                api_format: endpoint.api_format.clone(),
                endpoint,
                channel_type: ch.channel_type.clone(),
                policies: ch.policies.clone(),
                credential_key_identity: ch.credential_key_identity.clone(),
                tags: ch.tags.clone(),
                base_url: ch.base_url.clone(),
                active_credential: ch.active_credential.clone(),
                // P-17: carry the enabled-key set alongside the (legacy) active
                // credential so downstream selection can defer to trace time.
                enabled_credentials: ch.enabled_credentials.clone(),
                settings: ch.settings.clone(),
                theoretical_cost_accounting: None,
                cost_efficiency_score: 0,
            });
        }
        candidates
    }

    /// Model-based selection (S06): resolve associations, evaluate conditions,
    /// aggregate + dedup. Returns an empty list (not an error) when the model
    /// has no associations or none match — matching Go's behavior.
    fn select_model_candidates(
        &self,
        req: &CandidateRequest,
        channels: &[ChannelSnapshot],
        effective: &EffectiveModel,
        now_rfc3339: &str,
    ) -> Vec<ChannelModelsCandidate> {
        if effective.associations.is_empty() {
            return Vec::new();
        }
        let matches = match_associations(&effective.associations, channels);
        // Flatten matches into resolved candidates (resolving channel indices).
        let mut resolved: Vec<ResolvedAssociationCandidate> = Vec::new();
        for m in &matches {
            for conn in &m.connections {
                let Some(ch) = channels.get(conn.channel_idx) else {
                    continue;
                };
                resolved.push(ResolvedAssociationCandidate {
                    channel: ch.clone(),
                    priority: conn.priority,
                    models: conn.models.clone(),
                    when: m.when.clone(),
                });
            }
        }
        filter_resolved_for_request(&resolved, req, now_rfc3339)
    }

    /// Compute the association-resolution cache key for a model (S07). Returns
    /// `None` when the model is unknown (no caching on the legacy path).
    pub fn cache_key(
        &self,
        requested_model_id: &str,
        channels: &[ChannelSnapshot],
        associations: &dyn AssociationSource,
        channel_cache_version: u64,
    ) -> Option<AssociationCacheKey> {
        let effective = associations.resolve(requested_model_id)?;
        let signature = model_association_signature(&effective.associations);
        Some(AssociationCacheKey::new(
            effective.model_id.clone(),
            signature,
            channels.len(),
            latest_channel_update(channels),
            effective.updated_at.clone(),
            channel_cache_version,
        ))
    }

    /// S12 entry point: run selection from a unified [`SelectionInputs`] bag.
    /// This is the Go-parity surface — Go's `DefaultSelector.Select(ctx, req)`
    /// implicitly reads the same inputs through its receiver fields. The
    /// method resolves candidates via [`Self::select`] and then applies the
    /// request-scoped [`FilterPipeline`] (project/key profile, native-tools
    /// capability, stream policy) using the bag's `profile`, returning the
    /// survivors plus diagnostics. Quota admission and load balancing belong
    /// to the top-level [`select_candidates`] step (Go `selectCandidates`
    /// middleware), which composes this method's stages with the runtime
    /// quota/LB inputs.
    pub fn select_with_inputs(
        &self,
        inputs: &SelectionInputs<'_>,
    ) -> (Vec<ChannelModelsCandidate>, SelectionDiagnostics) {
        let resolved = match self.select(
            inputs.request,
            inputs.channels,
            inputs.associations,
            inputs.now_rfc3339,
        ) {
            Ok(cands) => cands,
            Err(_) => {
                // Model-not-found surfaces as an empty result here; callers
                // that need the typed error use [`Self::select`] directly.
                // (Go's `selectCandidates` middleware translates the same
                // error into `ErrInvalidModel` at the pipeline layer.)
                return (Vec::new(), SelectionDiagnostics::default());
            }
        };
        FilterPipeline::run(resolved, &inputs.profile)
    }

    /// S12 + S16: build the request-scoped cache key from the unified inputs.
    /// Combines the S07 association-resolution dimensions with the S16
    /// request-scoped dimensions (project id, api key id, request type, stream,
    /// tags/profile signature). Returns `None` when the model is unknown (no
    /// caching on the legacy path), mirroring [`Self::cache_key`].
    pub fn cache_key_with_inputs(
        &self,
        inputs: &SelectionInputs<'_>,
        project_id: &str,
        api_key_id: &str,
        channel_cache_version: u64,
    ) -> Option<RequestScopedCacheKey> {
        let association = self.cache_key(
            &inputs.request.model,
            inputs.channels,
            inputs.associations,
            channel_cache_version,
        )?;
        // Tags signature from the profile (sorted-unique). The profile
        // signature string is derived from the channel-id allow-lists so two
        // profiles with the same tags+allow-list collapse to one key.
        let mut all_tags: Vec<String> = inputs
            .profile
            .project_channel_tags
            .iter()
            .chain(inputs.profile.key_channel_tags.iter())
            .cloned()
            .collect();
        let profile_sig = format!(
            "p:{}|k:{}",
            join_sorted(&inputs.profile.project_channel_ids),
            join_sorted(&inputs.profile.key_channel_ids),
        );
        let tags_sig = tags_profile_sig(&all_tags, &profile_sig);
        all_tags.clear();
        let _ = all_tags;
        Some(RequestScopedCacheKey::new(
            association,
            project_id,
            api_key_id,
            inputs.request.model.clone(),
            inputs.request.request_type,
            inputs.request.stream,
            tags_sig,
        ))
    }
}

/// Comma-joined sorted-unique view of a string slice, for stable profile
/// signatures. Internal helper for [`CandidateSelector::cache_key_with_inputs`].
fn join_sorted(values: &[String]) -> String {
    let mut v: Vec<&str> = values.iter().map(String::as_str).collect();
    v.sort();
    v.dedup();
    v.join(",")
}

// ---------------------------------------------------------------------------
// S15: selection diagnostics (Go-parity, for ChannelModelsCandidate)
// ---------------------------------------------------------------------------

/// Stable identifier of a pipeline filter stage. The order mirrors the Go
/// decorator chain in `select_candidates.go` (see [`FilterPipeline`] docs).
/// Variants are ordered least-specific first so diagnostics read top-down.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum FilterStage {
    /// Project-level profile (channel-id allowlist + channel-tags). Go
    /// `WithSelectedChannelsSelector`/`WithChannelTagsFilterSelector` applied
    /// with the project's active profile.
    ProjectProfile,
    /// API-key-level profile (narrows further within the project scope).
    KeyProfile,
    /// Native-tools capability gate for Gemini/Anthropic native API formats.
    /// Go `WithGoogleNativeToolsSelector`/`WithAnthropicNativeToolsSelector`.
    NativeToolsCapability,
    /// Stream capability policy. Go `WithStreamPolicySelector`.
    StreamPolicy,
    /// Provider quota / rate-limit admission. Go `WithProviderQuotaSelector`.
    /// Implemented by [`apply_provider_quota_selector`], which the top-level
    /// [`select_candidates`] step invokes after the pure-logic stages (the Go
    /// quota decorator likewise wraps outside the profile/native/stream trio).
    /// [`FilterPipeline::run`] itself does not invoke this stage because the
    /// quota-status snapshot is a runtime dependency the caller injects via
    /// [`ProviderQuotaStatusProvider`].
    QuotaAdmission,
    /// Procurement price, price-shape, and currency-conversion admission.
    /// Runs after access/quota selection and before any upstream attempt.
    PricingAdmission,
    /// Recent execution health for the concrete channel/model/credential
    /// target. This is applied after credential selection and before retries.
    RouteHealth,
}

impl FilterStage {
    /// Human-readable label matching Go decorator names.
    pub fn label(self) -> &'static str {
        match self {
            FilterStage::ProjectProfile => "project_profile",
            FilterStage::KeyProfile => "key_profile",
            FilterStage::NativeToolsCapability => "native_tools_capability",
            FilterStage::StreamPolicy => "stream_policy",
            FilterStage::QuotaAdmission => "quota_admission",
            FilterStage::PricingAdmission => "pricing_admission",
            FilterStage::RouteHealth => "route_health",
        }
    }
}

/// One recorded rejection. Mirrors the per-candidate "filtered-out reason"
/// produced by each Go decorator. The `detail` string is free-form because each
/// stage has its own reason vocabulary (channel-id-not-allowed,
/// missing-required-tag, stream-policy-forbid, quota-exhausted, ...).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionRejection {
    pub stage: FilterStage,
    pub channel_id: String,
    pub channel_name: String,
    pub detail: String,
}

/// Diagnostics accumulated by the [`FilterPipeline`]. Selected candidates are
/// recorded once (final survivors); rejected candidates carry the stage +
/// reason. This is the Go-parity diagnostics for `ChannelModelsCandidate` and
/// is distinct from the generic [`crate::candidate::CandidateFilterDiagnostics`]
/// which operates on the abstract [`crate::candidate::Candidate`] type.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionDiagnostics {
    pub selected: Vec<SelectedCandidateRef>,
    pub rejected: Vec<SelectionRejection>,
}

/// Lightweight reference to a surviving candidate in diagnostics (avoids
/// cloning the full model list).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectedCandidateRef {
    pub channel_id: String,
    pub channel_name: String,
    pub priority: i64,
    pub api_format: String,
}

impl SelectionDiagnostics {
    /// Number of distinct candidates that survived all stages.
    pub fn selected_count(&self) -> usize {
        self.selected.len()
    }

    /// Number of distinct candidates rejected by at least one stage.
    pub fn rejected_candidate_count(&self) -> usize {
        let mut ids: BTreeSet<&str> = BTreeSet::new();
        for r in &self.rejected {
            ids.insert(r.channel_id.as_str());
        }
        ids.len()
    }

    /// Rejection count grouped by stage label, sorted by stage order. Used by
    /// tests to assert which stage filtered a candidate.
    pub fn rejections_by_stage(&self) -> Vec<(FilterStage, usize)> {
        let mut counts: Vec<(FilterStage, usize)> = Vec::new();
        for stage in [
            FilterStage::ProjectProfile,
            FilterStage::KeyProfile,
            FilterStage::NativeToolsCapability,
            FilterStage::StreamPolicy,
            FilterStage::QuotaAdmission,
            FilterStage::PricingAdmission,
            FilterStage::RouteHealth,
        ] {
            let n = self.rejected.iter().filter(|r| r.stage == stage).count();
            if n > 0 {
                counts.push((stage, n));
            }
        }
        counts
    }
}

// ---------------------------------------------------------------------------
// S13: ordered filter pipeline (pure-logic, mirrors Go decorator chain)
// ---------------------------------------------------------------------------

/// Inputs to the request-scoped filter pipeline. Built by the caller from the
/// inbound request context (project/api-key profile + request flags). Fields
/// left empty/`None` disable the corresponding stage, matching Go's "empty
/// allow-list = no filtering" behavior.
///
/// Manual `Default` because `RequestType` (conduit-llm) does not implement
/// `Default`; the stream-policy stage treats a default-constructed context as
/// a chat request, mirroring Go's empty-`RequestType` handling
/// (`supportsAutoAggregateRequest` falls through to the chat branches when
/// `req.RequestType == ""`).
#[derive(Debug, Clone)]
pub struct FilterContext {
    /// Allowed channel IDs from the project-level profile. Empty = all allowed.
    pub project_channel_ids: Vec<String>,
    /// Required channel tags from the project-level profile.
    pub project_channel_tags: Vec<String>,
    /// Tag match mode for the project-level profile (Go
    /// `projectProfile.ChannelTagsMatchMode`; empty = `any`).
    pub project_channel_tags_match_mode: String,
    /// Allowed channel IDs from the api-key-level profile (narrows project).
    pub key_channel_ids: Vec<String>,
    /// Required channel tags from the api-key-level profile.
    pub key_channel_tags: Vec<String>,
    /// Tag match mode for the api-key-level profile (Go
    /// `profile.ChannelTagsMatchMode`; empty = `any`).
    pub key_channel_tags_match_mode: String,
    /// Inbound API format (drives the native-tools capability gate). Go
    /// `req.APIFormat`; the pipeline only applies the Google native-tools
    /// filter when this equals `gemini/contents`, and the Anthropic filter
    /// when it equals `anthropic/messages` (mirrors `select_candidates.go`).
    pub api_format: String,
    /// Whether the inbound request is a streaming request. Go `req.Stream`.
    pub stream: bool,
    /// Request tools (Go `req.Tools`). The native-tools gate inspects these for
    /// `google_*` / `web_search` / `web_search_20250305` tool types.
    pub tools: Vec<RequestTool>,
    /// Request type (Go `req.RequestType`). Drives the auto-aggregate fallback
    /// decision in the stream-policy stage.
    pub request_type: RequestType,
}

impl FilterContext {
    /// Build a [`FilterContext`] from the inbound [`CandidateRequest`]. Carries
    /// the request-scoped fields the pipeline needs; the caller still supplies
    /// the project/key profile allow-lists separately.
    pub fn from_request(req: &CandidateRequest) -> Self {
        Self {
            project_channel_ids: req.project_channel_ids.clone(),
            project_channel_tags: req.project_channel_tags.clone(),
            project_channel_tags_match_mode: req.project_channel_tags_match_mode.clone(),
            key_channel_ids: req.key_channel_ids.clone(),
            key_channel_tags: req.key_channel_tags.clone(),
            key_channel_tags_match_mode: req.key_channel_tags_match_mode.clone(),
            api_format: req.api_format.clone(),
            stream: req.stream,
            tools: req.tools.clone(),
            request_type: req.request_type,
            ..Default::default()
        }
    }
}

impl Default for FilterContext {
    fn default() -> Self {
        Self {
            project_channel_ids: Vec::new(),
            project_channel_tags: Vec::new(),
            project_channel_tags_match_mode: String::new(),
            key_channel_ids: Vec::new(),
            key_channel_tags: Vec::new(),
            key_channel_tags_match_mode: String::new(),
            api_format: String::new(),
            stream: false,
            tools: Vec::new(),
            // Default to Chat so the stream-policy auto-aggregate branch
            // matches Go's empty-`RequestType` fallthrough.
            request_type: RequestType::Chat,
        }
    }
}

/// The fixed-order filter pipeline applied to the resolved
/// [`ChannelModelsCandidate`] list. Mirrors the Go decorator chain composed in
/// `select_candidates.go`:
///
/// 1. **Project profile** ([`FilterStage::ProjectProfile`]) — channel-id
///    allowlist (Go `WithSelectedChannelsSelector`) then channel-tags filter
///    (Go `WithChannelTagsFilterSelector` with the project profile's match
///    mode), from `select_candidates.go` lines 29-42.
/// 2. **Key profile** ([`FilterStage::KeyProfile`]) — the api-key profile's
///    channel-id allowlist then channel-tags filter (lines 44-53); key
///    narrows project because the passes run sequentially, exactly like the
///    Go decorators.
/// 3. **Native-tools capability** ([`FilterStage::NativeToolsCapability`]) —
///    Go `WithGoogleNativeToolsSelector` (only for `gemini/contents`) and
///    `WithAnthropicNativeToolsSelector` (only for `anthropic/messages`).
///    Active only when the inbound API format is one of those two and the
///    request tools contain a native tool type (`google_*` or
///    `web_search`/`web_search_20250305`). Falls back to all candidates when
///    no compatible channel exists (Go parity).
/// 4. **Stream policy** ([`FilterStage::StreamPolicy`]) — full port of Go
///    `StreamPolicySelector`. For a streaming request, drops `forbid`-policy
///    candidates; for a non-streaming request, prefers non-`require`
///    candidates and only falls back to `require` ones when the request
///    supports auto-aggregate.
/// 5. **Quota admission** ([`FilterStage::QuotaAdmission`]) — Go
///    `WithProviderQuotaSelector`. Not part of [`Self::run`]: the quota
///    snapshot is a runtime input, so the stage lives in
///    [`apply_provider_quota_selector`] and is invoked by the top-level
///    [`select_candidates`] step, in the same position as the Go decorator
///    (after stream policy, before load balancing).
///
/// The earlier stages (model access, channel status, model support/mapping,
/// request-type capability for the inbound API format, and association
/// `When`/conditions) are performed inside [`CandidateSelector::select`] before
/// the pipeline runs; this pipeline only covers the request-scoped decorators
/// that operate on the already-resolved candidate list. Each stage records its
/// rejections into [`SelectionDiagnostics`] (S15).
#[derive(Debug, Clone, Default)]
pub struct FilterPipeline;

impl FilterPipeline {
    /// Run all pure-logic stages against the resolved candidates and return the
    /// survivors plus accumulated diagnostics. Stages run in the documented
    /// fixed order; a candidate rejected by an earlier stage does not reach the
    /// later stages (Go decorator parity — the wrapped selector never sees
    /// filtered candidates).
    pub fn run(
        candidates: Vec<ChannelModelsCandidate>,
        ctx: &FilterContext,
    ) -> (Vec<ChannelModelsCandidate>, SelectionDiagnostics) {
        let mut diags = SelectionDiagnostics::default();
        let survivors = Self::run_stages(candidates, ctx, &mut diags);
        for c in &survivors {
            diags.selected.push(SelectedCandidateRef {
                channel_id: c.channel_id.clone(),
                channel_name: c.channel_name.clone(),
                priority: c.priority,
                api_format: c.api_format.clone(),
            });
        }
        (survivors, diags)
    }

    /// Profile → native-tools → stream-policy, recording rejections but not
    /// the final `selected` refs. Used by [`select_candidates`], which appends
    /// the survivors to `diags.selected` only after the quota and load-balance
    /// stages have run.
    fn run_stages(
        candidates: Vec<ChannelModelsCandidate>,
        ctx: &FilterContext,
        diags: &mut SelectionDiagnostics,
    ) -> Vec<ChannelModelsCandidate> {
        let survivors = Self::profile_stages(candidates, ctx, diags);
        let survivors = Self::native_tools_capability(survivors, ctx, diags);
        Self::stream_policy(survivors, ctx, diags)
    }

    /// Stages 1+2: project profile then key profile, in the Go decorator order
    /// (`select_candidates.go` lines 29-53). Each level applies its channel-id
    /// allowlist (Go `SelectedChannelsSelector.Select`, `candidates.go` lines
    /// 597-620) and then its channel-tags filter (Go `TagsFilterSelector.Select`
    /// and `matchChannelTagsFilter`, `candidates.go` lines 723-742). An empty
    /// allow-list or empty tags list is a pass-through, matching the Go
    /// decorators.
    fn profile_stages(
        candidates: Vec<ChannelModelsCandidate>,
        ctx: &FilterContext,
        diags: &mut SelectionDiagnostics,
    ) -> Vec<ChannelModelsCandidate> {
        let survivors = Self::allowlist_pass(
            candidates,
            &ctx.project_channel_ids,
            FilterStage::ProjectProfile,
            "project",
            diags,
        );
        let survivors = Self::tags_pass(
            survivors,
            &ctx.project_channel_tags,
            &ctx.project_channel_tags_match_mode,
            FilterStage::ProjectProfile,
            "project",
            diags,
        );
        let survivors = Self::allowlist_pass(
            survivors,
            &ctx.key_channel_ids,
            FilterStage::KeyProfile,
            "key",
            diags,
        );
        Self::tags_pass(
            survivors,
            &ctx.key_channel_tags,
            &ctx.key_channel_tags_match_mode,
            FilterStage::KeyProfile,
            "key",
            diags,
        )
    }

    /// One channel-id allowlist pass. Mirrors Go `SelectedChannelsSelector`
    /// (`candidates.go` lines 597-620): empty allow-list returns all
    /// candidates; otherwise only candidates whose channel id is allowed
    /// survive.
    fn allowlist_pass(
        candidates: Vec<ChannelModelsCandidate>,
        allowed_ids: &[String],
        stage: FilterStage,
        level: &str,
        diags: &mut SelectionDiagnostics,
    ) -> Vec<ChannelModelsCandidate> {
        if allowed_ids.is_empty() {
            return candidates;
        }
        candidates
            .into_iter()
            .filter(|c| {
                if allowed_ids.contains(&c.channel_id) {
                    true
                } else {
                    diags.rejected.push(SelectionRejection {
                        stage,
                        channel_id: c.channel_id.clone(),
                        channel_name: c.channel_name.clone(),
                        detail: format!(
                            "channel_id {} not in {} profile allow-list ({} ids)",
                            c.channel_id,
                            level,
                            allowed_ids.len()
                        ),
                    });
                    false
                }
            })
            .collect()
    }

    /// One channel-tags pass. Mirrors Go `TagsFilterSelector.Select`
    /// (`candidates.go` lines 723-742): empty tags list returns all
    /// candidates; otherwise each candidate's channel tags are matched via
    /// `objects.MatchChannelTags(allowedTags, matchMode, channelTags)`.
    fn tags_pass(
        candidates: Vec<ChannelModelsCandidate>,
        allowed_tags: &[String],
        match_mode: &str,
        stage: FilterStage,
        level: &str,
        diags: &mut SelectionDiagnostics,
    ) -> Vec<ChannelModelsCandidate> {
        if allowed_tags.is_empty() {
            return candidates;
        }
        candidates
            .into_iter()
            .filter(|c| {
                if match_channel_tags(allowed_tags, match_mode, &c.tags) {
                    true
                } else {
                    diags.rejected.push(SelectionRejection {
                        stage,
                        channel_id: c.channel_id.clone(),
                        channel_name: c.channel_name.clone(),
                        detail: format!(
                            "channel tags {:?} do not match {} profile tags {:?} (mode {:?})",
                            c.tags, level, allowed_tags, match_mode
                        ),
                    });
                    false
                }
            })
            .collect()
    }

    /// Stage: native-tools capability gate. Mirrors Go
    /// `WithGoogleNativeToolsSelector` (applied only when the inbound API
    /// format is `gemini/contents`) and `WithAnthropicNativeToolsSelector`
    /// (applied only when the inbound API format is `anthropic/messages`).
    /// Both decorators short-circuit when the request carries no native tools;
    /// when native tools are present, candidates whose channels do not support
    /// the corresponding native tool family are filtered out, falling back to
    /// the full candidate list when no compatible channel exists.
    ///
    /// See `conduit/internal/server/orchestrator/candidates_google.go` and
    /// `candidates_anthropic.go`. The gating on `api_format` mirrors
    /// `select_candidates.go` lines 56-63.
    fn native_tools_capability(
        candidates: Vec<ChannelModelsCandidate>,
        ctx: &FilterContext,
        diags: &mut SelectionDiagnostics,
    ) -> Vec<ChannelModelsCandidate> {
        // Determine which native-tools family (if any) is active for this
        // request. The Go decorator chain wires exactly one selector based on
        // the inbound API format; here we replicate that dispatch.
        let family = match ctx.api_format.as_str() {
            "gemini/contents" => {
                if !contains_google_native_tools(&ctx.tools) {
                    return candidates;
                }
                NativeToolFamily::Google
            }
            "anthropic/messages" => {
                if !contains_anthropic_native_tools(&ctx.tools) {
                    return candidates;
                }
                NativeToolFamily::Anthropic
            }
            _ => return candidates,
        };

        let (kept, filtered_out): (Vec<_>, Vec<_>) =
            candidates.into_iter().partition(|c| match family {
                NativeToolFamily::Google => c.supports_google_native_tools(),
                NativeToolFamily::Anthropic => c.supports_anthropic_native_tools(),
            });

        // Go parity: when at least one compatible candidate survives, record
        // the rest as rejected. When nothing is compatible, fall back to all
        // candidates and record NO rejections (the Go decorator returns the
        // original list in that case — nothing was actually filtered out).
        if kept.is_empty() {
            return filtered_out;
        }

        for c in &filtered_out {
            diags.rejected.push(SelectionRejection {
                stage: FilterStage::NativeToolsCapability,
                channel_id: c.channel_id.clone(),
                channel_name: c.channel_name.clone(),
                detail: format!(
                    "channel type {:?} does not support {} native tools",
                    c.channel_type, family
                ),
            });
        }
        kept
    }

    /// Stage: stream policy. Full port of Go `StreamPolicySelector.Select`
    /// (`candidates_stream_policy.go`). For a streaming request, drop
    /// candidates whose stream policy is `forbid`. For a non-streaming
    /// request, prefer candidates whose stream policy is not `require`; if
    /// any non-`require` candidates survive, drop the `require` ones. If only
    /// `require` candidates remain, keep them only when the request supports
    /// auto-aggregate (otherwise drop all — Go returns nil).
    fn stream_policy(
        candidates: Vec<ChannelModelsCandidate>,
        ctx: &FilterContext,
        diags: &mut SelectionDiagnostics,
    ) -> Vec<ChannelModelsCandidate> {
        if candidates.is_empty() {
            return candidates;
        }

        if ctx.stream {
            // Streaming request: drop `forbid`-policy candidates.
            let (kept, filtered_out): (Vec<_>, Vec<_>) = candidates
                .into_iter()
                .partition(|c| c.stream_policy() != capability_policy::FORBID);
            for c in &filtered_out {
                diags.rejected.push(SelectionRejection {
                    stage: FilterStage::StreamPolicy,
                    channel_id: c.channel_id.clone(),
                    channel_name: c.channel_name.clone(),
                    detail: "stream policy is 'forbid' for a streaming request".to_string(),
                });
            }
            return kept;
        }

        // Non-streaming request: prefer non-`require` candidates.
        let (native, require_only): (Vec<_>, Vec<_>) = candidates
            .into_iter()
            .partition(|c| c.stream_policy() != capability_policy::REQUIRE);
        if !native.is_empty() {
            for c in &require_only {
                diags.rejected.push(SelectionRejection {
                    stage: FilterStage::StreamPolicy,
                    channel_id: c.channel_id.clone(),
                    channel_name: c.channel_name.clone(),
                    detail: "stream policy is 'require' for a non-streaming request; native candidates preferred".to_string(),
                });
            }
            return native;
        }

        // Only `require`-policy candidates remain. Keep them only when the
        // request supports auto-aggregate; otherwise drop all (Go returns nil).
        if supports_auto_aggregate(&ctx.api_format, ctx.request_type) {
            require_only
        } else {
            for c in &require_only {
                diags.rejected.push(SelectionRejection {
                    stage: FilterStage::StreamPolicy,
                    channel_id: c.channel_id.clone(),
                    channel_name: c.channel_name.clone(),
                    detail: "stream policy is 'require' for a non-streaming request without auto-aggregate".to_string(),
                });
            }
            Vec::new()
        }
    }
}

/// Native-tool family discriminator for the S10 stage. Mirrors which Go
/// selector decorator is active (`WithGoogleNativeToolsSelector` vs.
/// `WithAnthropicNativeToolsSelector`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NativeToolFamily {
    Google,
    Anthropic,
}

impl std::fmt::Display for NativeToolFamily {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            NativeToolFamily::Google => "google",
            NativeToolFamily::Anthropic => "anthropic",
        })
    }
}

/// Mirrors Go `llm.ContainsGoogleNativeTools` (`llm/tools.go`): a tool is a
/// Google native tool when its `Type` starts with `google_`.
pub fn contains_google_native_tools(tools: &[RequestTool]) -> bool {
    tools.iter().any(|t| t.tool_type.starts_with("google_"))
}

/// Mirrors Go `anthropic.ContainsAnthropicNativeTools`
/// (`llm/transformer/anthropic/tools.go`): a tool is an Anthropic native tool
/// when its `Type` is `web_search` (OpenAI-format input that maps to the
/// Anthropic native tool) or `web_search_20250305` (already-transformed
/// Anthropic native format).
pub fn contains_anthropic_native_tools(tools: &[RequestTool]) -> bool {
    tools
        .iter()
        .any(|t| t.tool_type == "web_search" || t.tool_type == "web_search_20250305")
}

/// Predicate form of the stream-policy filter, operating on a single candidate
/// plus its stream policy string. Pure, fully unit-testable, and an exact port
/// of Go `StreamPolicySelector.Select` for one candidate. Exposed so a future
/// caller that carries the real policy can reuse the decision logic.
///
/// Returns `Ok(())` when the candidate survives, or `Err(reason)` with the
/// filter-out reason for diagnostics.
pub fn stream_policy_allows(
    policy: &str,
    stream: bool,
    supports_auto_aggregate: bool,
) -> Result<(), &'static str> {
    if stream {
        if policy == capability_policy::FORBID {
            return Err("stream policy is 'forbid' for a streaming request");
        }
        return Ok(());
    }
    // Non-streaming request.
    if policy == capability_policy::REQUIRE {
        // Require-stream candidates only survive for non-stream requests when
        // the request supports auto-aggregate (Go falls back to keep them).
        if !supports_auto_aggregate {
            return Err(
                "stream policy is 'require' for a non-streaming request without auto-aggregate",
            );
        }
    }
    Ok(())
}

/// Auto-aggregate support predicate, mirroring Go
/// `supportsAutoAggregateRequest`. True for chat requests on the common chat
/// API formats and for completion requests on the openai completion format.
pub fn supports_auto_aggregate(api_format: &str, request_type: RequestType) -> bool {
    match request_type {
        RequestType::Chat => matches!(
            api_format,
            "" | "openai/chat_completions"
                | "openai/responses"
                | "anthropic/messages"
                | "gemini/contents"
                | "ollama/chat"
        ),
        RequestType::Completion => api_format.is_empty() || api_format == "openai/completions",
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// RUST-P9-006 S10: top-level selection flow — port of Go
// `select_candidates.go` (`selectCandidates` middleware body +
// `areAllChannelsExhausted`), `candidates_quota.go` (`ProviderQuotaSelector`),
// `biz/provider_quota.go` (`QuotaChannelStatus.EffectiveStatus`,
// `quotaStatusRank`), `biz/provider_quota/types.go` (`RequestModality`),
// and the `LoadBalancedSelector` decorator (`candidates.go` lines 622-704).
// ---------------------------------------------------------------------------

/// Provider-quota status values. Mirrors the Go ent enum
/// `providerquotastatus.Status` (`internal/ent/providerquotastatus/`, lines
/// 133-136): `available`, `warning`, `exhausted`, `unknown`.
pub mod provider_quota_status {
    pub const AVAILABLE: &str = "available";
    pub const WARNING: &str = "warning";
    pub const EXHAUSTED: &str = "exhausted";
    pub const UNKNOWN: &str = "unknown";
}

/// Quota limit types. Mirrors Go `provider_quota.QuotaLimitType`
/// (`internal/server/biz/provider_quota/types.go` lines 18-24).
pub mod quota_limit_type {
    pub const IMAGE: &str = "image";
    pub const TOKEN: &str = "token";
    pub const SUBSCRIPTION_CYCLE: &str = "subscription_cycle";
}

/// Mirrors Go `provider_quota.RequestModality` (`types.go` lines 50-55): an
/// image request consumes the `image` limit, everything else the `token`
/// limit.
pub fn request_modality(is_image_request: bool) -> &'static str {
    if is_image_request {
        quota_limit_type::IMAGE
    } else {
        quota_limit_type::TOKEN
    }
}

/// One per-limit quota status. Mirrors the Go `provider_quota.QuotaLimitStatus`
/// fields the selector reads (`Type`, `Status`, `Ready`; `types.go` lines
/// 29-35 — `UsageRatio`/`NextResetAt` are diagnostics-only and omitted here).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct QuotaLimitStatusView {
    pub limit_type: String,
    pub status: String,
    pub ready: bool,
}

/// Channel-level quota status snapshot. Mirrors Go `biz.QuotaChannelStatus`
/// (`internal/server/biz/provider_quota.go` lines 26-30).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct QuotaChannelStatusView {
    pub status: String,
    pub ready: bool,
    pub limits: Vec<QuotaLimitStatusView>,
}

impl QuotaChannelStatusView {
    /// Effective quota status for `limit_type`. Port of Go
    /// `QuotaChannelStatus.EffectiveStatus` (`provider_quota.go` lines 39-81):
    /// - channel-level `exhausted` short-circuits regardless of per-limit data;
    /// - no limits → the channel-level status/ready;
    /// - otherwise the worst-ranked matching limit wins (equal rank ANDs the
    ///   `ready` flags);
    /// - no matching limit type → `unknown` with `ready=true` so missing data
    ///   does not block routing.
    pub fn effective_status(&self, limit_type: &str) -> (String, bool) {
        if self.status == provider_quota_status::EXHAUSTED {
            return (provider_quota_status::EXHAUSTED.to_string(), false);
        }
        if self.limits.is_empty() {
            return (self.status.clone(), self.ready);
        }
        let mut worst_status = String::new();
        let mut worst_ready = true;
        let mut found = false;
        for l in &self.limits {
            if l.limit_type != limit_type {
                continue;
            }
            if !found {
                worst_status = l.status.clone();
                worst_ready = l.ready;
                found = true;
                continue;
            }
            if quota_status_rank(&l.status) > quota_status_rank(&worst_status) {
                worst_status = l.status.clone();
                worst_ready = l.ready;
            } else if quota_status_rank(&l.status) == quota_status_rank(&worst_status) {
                worst_ready = worst_ready && l.ready;
            }
        }
        if !found {
            // Go lines 73-79: missing data must not block routing.
            return (provider_quota_status::UNKNOWN.to_string(), true);
        }
        (worst_status, worst_ready)
    }
}

/// Port of Go `quotaStatusRank` (`provider_quota.go` lines 83-96): available 0,
/// warning 1, exhausted 2, unknown/default -1.
fn quota_status_rank(status: &str) -> i32 {
    match status {
        provider_quota_status::AVAILABLE => 0,
        provider_quota_status::WARNING => 1,
        provider_quota_status::EXHAUSTED => 2,
        _ => -1,
    }
}

/// Quota-status source for channels. Mirrors Go `ProviderQuotaStatusProvider`
/// (`provider_quota_provider.go`): `GetQuotaStatus(channelID)` returns nil
/// (Rust `None`) when there is no quota data for the channel. The production
/// impl wraps `conduit_services`' provider-quota service; tests use in-memory
/// fixtures.
pub trait ProviderQuotaStatusProvider {
    fn get_quota_status(&self, channel_id: &str) -> Option<QuotaChannelStatusView>;
}

/// Quota enforcement settings. Mirrors Go `biz.QuotaEnforcementSettings`
/// (`internal/server/biz/system.go` lines 203-209). The `Default` matches Go
/// `defaultQuotaEnforcementSettings` (`system_default.go` lines 76-79):
/// disabled, `exhausted_only`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuotaEnforcementSettings {
    pub enabled: bool,
    pub mode: ProviderQuotaEnforcementMode,
}

impl Default for QuotaEnforcementSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: ProviderQuotaEnforcementMode::ExhaustedOnly,
        }
    }
}

/// Port of Go `ProviderQuotaSelector.Select` (`candidates_quota.go` lines
/// 34-90) in data form: `candidates` is what the wrapped selector produced.
/// Returns the surviving candidates plus the number filtered out (Go
/// `ProviderQuotaSelector.FilteredCount` — only populated in ExhaustedOnly
/// mode; the DePrioritize/disabled/nil-provider paths return early with 0).
/// Rejections are recorded under [`FilterStage::QuotaAdmission`].
pub fn apply_provider_quota_selector(
    candidates: Vec<ChannelModelsCandidate>,
    provider: Option<&dyn ProviderQuotaStatusProvider>,
    settings: &QuotaEnforcementSettings,
    is_image_request: bool,
    diags: &mut SelectionDiagnostics,
) -> (Vec<ChannelModelsCandidate>, usize) {
    // Go lines 40-42: empty pass-through.
    if candidates.is_empty() {
        return (candidates, 0);
    }
    // Go lines 44-46: nil provider passes everything through.
    let Some(provider) = provider else {
        return (candidates, 0);
    };
    // Go lines 48-52: disabled or DePrioritize mode returns early.
    if !settings.enabled || settings.mode == ProviderQuotaEnforcementMode::DePrioritize {
        return (candidates, 0);
    }
    // Go line 54: limit type from the request modality.
    let limit_type = request_modality(is_image_request);
    let before = candidates.len();
    let mut filtered = Vec::with_capacity(before);
    for c in candidates {
        let keep = match provider.get_quota_status(&c.channel_id) {
            // Go lines 59-61: channels without quota data are kept.
            None => true,
            Some(status) => {
                let (effective, _) = status.effective_status(limit_type);
                // Go lines 65-74: only `exhausted` filters; available /
                // warning / unknown / anything else keeps the candidate.
                effective != provider_quota_status::EXHAUSTED
            }
        };
        if keep {
            filtered.push(c);
        } else {
            diags.rejected.push(SelectionRejection {
                stage: FilterStage::QuotaAdmission,
                channel_id: c.channel_id.clone(),
                channel_name: c.channel_name.clone(),
                detail: format!("provider quota exhausted for limit type {limit_type:?}"),
            });
        }
    }
    // Go line 77: FilteredCount = before - after.
    let filtered_count = before - filtered.len();
    (filtered, filtered_count)
}

/// Port of Go `areAllChannelsExhausted` (`select_candidates.go` lines
/// 125-145): `false` when there are no candidates or no provider, and as soon
/// as any channel has no quota data or a non-exhausted effective status.
pub fn are_all_channels_exhausted(
    candidates: &[ChannelModelsCandidate],
    provider: Option<&dyn ProviderQuotaStatusProvider>,
    is_image_request: bool,
) -> bool {
    let Some(provider) = provider else {
        return false;
    };
    if candidates.is_empty() {
        return false;
    }
    let limit_type = request_modality(is_image_request);
    for c in candidates {
        let Some(status) = provider.get_quota_status(&c.channel_id) else {
            return false;
        };
        let (effective, _) = status.effective_status(limit_type);
        if effective != provider_quota_status::EXHAUSTED {
            return false;
        }
    }
    true
}

/// Sorts one priority group of candidates. Stub boundary for Go
/// `LoadBalancer.Sort(ctx, group, model, useStream)` (`load_balancer.go`) —
/// the strategy-composite LB in this crate operates on the abstract
/// [`crate::candidate::Candidate`] (see `load_balancer.rs::select_channels`);
/// the wiring layer bridges `ChannelModelsCandidate` ↔ `Candidate` and
/// implements this trait on top. `quota_limit_type` carries what Go passes via
/// `contextWithQuotaLimitType(ctx, RequestModality(req.Image != nil))`
/// (`candidates.go` line 678) for the quota-aware strategy.
pub trait CandidateGroupSorter {
    fn sort_group(
        &mut self,
        group: Vec<ChannelModelsCandidate>,
        model: &str,
        use_stream: bool,
        quota_limit_type: &str,
    ) -> Vec<ChannelModelsCandidate>;
}

/// Load-balance stage inputs. Mirrors Go `WithLoadBalancedSelector(wrapped,
/// loadBalancer, policy)` (`candidates.go` lines 629-637): the sorter is the
/// per-priority-group LB and `retry_policy` is the resolved policy (Go
/// `policy.RetryPolicyOrDefault(ctx)`) providing the early-stop budget.
pub struct LoadBalanceStage<'a> {
    pub sorter: &'a mut dyn CandidateGroupSorter,
    pub retry_policy: RetryPolicy,
}

impl std::fmt::Debug for LoadBalanceStage<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoadBalanceStage")
            .field("retry_policy", &self.retry_policy)
            .finish_non_exhaustive()
    }
}

/// Port of Go `LoadBalancedSelector.Select` (`candidates.go` lines 639-704):
/// - 0/1 candidates pass through untouched (line 645);
/// - `requiredCount` = 1, or `1 + MaxChannelRetries` when retry is enabled
///   (lines 652-655 — identical to [`RetryPolicy::top_k`]);
/// - candidates are grouped by `priority` and groups are processed in
///   ascending priority value (lower value = higher priority, lines 657-667);
/// - each group is LB-sorted, then appended until `requiredCount` is reached,
///   truncating the last group if needed (lines 671-693).
pub fn load_balanced_order(
    candidates: Vec<ChannelModelsCandidate>,
    stage: &mut LoadBalanceStage<'_>,
    model: &str,
    use_stream: bool,
    quota_limit_type: &str,
) -> Vec<ChannelModelsCandidate> {
    if candidates.len() <= 1 {
        return candidates;
    }
    // Go lines 652-655: requiredCount = 1 (+ MaxChannelRetries when enabled).
    let required_count = stage.retry_policy.top_k();
    // Go lines 658-667: group by priority; BTreeMap iterates keys ascending,
    // matching Go's `slices.Sort(priorities)`.
    let mut groups: BTreeMap<i64, Vec<ChannelModelsCandidate>> = BTreeMap::new();
    for c in candidates {
        groups.entry(c.priority).or_default().push(c);
    }
    let mut result: Vec<ChannelModelsCandidate> = Vec::new();
    for (_, group) in groups {
        // Go lines 676-679: the group is sorted before the remaining-budget
        // check (the sorter also receives model/stream/limit-type context).
        let sorted = stage
            .sorter
            .sort_group(group, model, use_stream, quota_limit_type);
        // Go lines 682-685.
        if result.len() >= required_count {
            break;
        }
        let remaining = required_count - result.len();
        // Go lines 687-692.
        if sorted.len() <= remaining {
            result.extend(sorted);
        } else {
            result.extend(sorted.into_iter().take(remaining));
            break;
        }
    }
    result
}

/// Error surface of the top-level selection step. Mirrors the two error paths
/// of Go `selectCandidates` (`select_candidates.go` lines 100-116).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SelectCandidatesError {
    /// Go `NewQuotaExhaustedError(model)` (`orchestrator/errors.go` lines
    /// 5-16): "all channels quota exhausted for model %s".
    #[error("all channels quota exhausted for model {model}")]
    QuotaExhausted { model: String },
    /// Go `fmt.Errorf("%w: %s", biz.ErrInvalidModel, model)` where
    /// `biz.ErrInvalidModel` is `errors.New("model not found")`
    /// (`llm/transformer/errors.go` line 9).
    #[error("model not found: {model}")]
    InvalidModel { model: String },
}

/// Top-level candidate selection step. Port of the Go `selectCandidates`
/// pipeline-middleware body (`select_candidates.go` lines 20-123), composing
/// the decorators in the exact Go wiring order:
///
/// 1. base selection ([`CandidateSelector::select`] — Go
///    `inbound.state.CandidateSelector`, the `DefaultSelector`);
/// 2. project profile → key profile → native-tools gate → stream policy
///    ([`FilterPipeline`] stages — Go lines 29-65);
/// 3. provider quota admission ([`apply_provider_quota_selector`] — Go lines
///    67-68);
/// 4. load balancing when a balancer is wired ([`load_balanced_order`] — Go
///    lines 70-72, `if inbound.state.LoadBalancer != nil`);
/// 5. empty-result semantics (Go lines 100-107): quota-exhausted when quota
///    enforcement is enabled and the quota stage filtered candidates,
///    otherwise `ErrInvalidModel`;
/// 6. DePrioritize re-check (Go lines 109-116): the quota selector does not
///    filter in that mode, so the final candidates are re-checked via
///    [`are_all_channels_exhausted`].
///
/// The Go middleware's "only select once" guard (lines 23-25) and the state
/// store (`inbound.state.ChannelModelsCandidates`, line 119) are pipeline
/// state owned by the wiring layer; this function is the pure selection body.
/// On success the returned diagnostics carry the final survivors in
/// `selected` plus every stage rejection recorded along the way.
pub fn select_candidates(
    inputs: &SelectionInputs<'_>,
    quota_provider: Option<&dyn ProviderQuotaStatusProvider>,
    quota_settings: &QuotaEnforcementSettings,
    load_balance: Option<&mut LoadBalanceStage<'_>>,
) -> Result<(Vec<ChannelModelsCandidate>, SelectionDiagnostics), SelectCandidatesError> {
    let selector = CandidateSelector;
    // Base selection. A fallback-disabled unknown model surfaces as Go's
    // wrapped `ErrInvalidModel` (`candidates.go` lines 86-103); the middleware
    // propagates it unchanged (`select_candidates.go` lines 74-77).
    let resolved = match selector.select(
        inputs.request,
        inputs.channels,
        inputs.associations,
        inputs.now_rfc3339,
    ) {
        Ok(candidates) => candidates,
        Err(CandidateSelectionError::ModelNotFound { model }) => {
            return Err(SelectCandidatesError::InvalidModel { model });
        }
    };

    // Request-scoped decorators (Go lines 29-65).
    let mut diags = SelectionDiagnostics::default();
    let survivors = FilterPipeline::run_stages(resolved, &inputs.profile, &mut diags);

    // Provider quota admission (Go lines 67-68). The filtered count feeds the
    // empty-result decision below (Go `quotaSelector.FilteredCount`).
    let is_image = inputs.request.is_image_request;
    let (survivors, quota_filtered_count) = apply_provider_quota_selector(
        survivors,
        quota_provider,
        quota_settings,
        is_image,
        &mut diags,
    );

    // Load balancing (Go lines 70-72): only when a load balancer is wired.
    let survivors = match load_balance {
        Some(stage) => load_balanced_order(
            survivors,
            stage,
            &inputs.request.model,
            inputs.request.stream,
            request_modality(is_image),
        ),
        None => survivors,
    };

    // Empty-result semantics (Go lines 100-107).
    if survivors.is_empty() {
        if quota_settings.enabled && quota_filtered_count > 0 {
            return Err(SelectCandidatesError::QuotaExhausted {
                model: inputs.request.model.clone(),
            });
        }
        return Err(SelectCandidatesError::InvalidModel {
            model: inputs.request.model.clone(),
        });
    }

    // DePrioritize re-check (Go lines 109-116).
    if quota_settings.enabled
        && quota_settings.mode == ProviderQuotaEnforcementMode::DePrioritize
        && are_all_channels_exhausted(&survivors, quota_provider, is_image)
    {
        return Err(SelectCandidatesError::QuotaExhausted {
            model: inputs.request.model.clone(),
        });
    }

    // Final survivors into diagnostics (post quota + load balance).
    for c in &survivors {
        diags.selected.push(SelectedCandidateRef {
            channel_id: c.channel_id.clone(),
            channel_name: c.channel_name.clone(),
            priority: c.priority,
            api_format: c.api_format.clone(),
        });
    }
    Ok((survivors, diags))
}

// ---------------------------------------------------------------------------
// S16: request-scoped cache key (extends S07 association key)
// ---------------------------------------------------------------------------

/// Request-scoped candidate cache key. Combines the S07 association-resolution
/// dimensions (model id, association signature, channel count, latest channel
/// update, latest model update, channel cache version) with the S16
/// request-scoped dimensions (project id, api key id, model, request type,
/// stream, tags/profile signature). Two requests with the same S07 inputs but
/// different S16 inputs must not share candidate sets because the profile
/// allow-list and stream policy change which candidates survive.
///
/// Deterministic and hashable so callers can use it as a `HashMap`/`BTreeMap`
/// key. The TTL is the caller's responsibility (S07 constant
/// [`ASSOCIATION_CACHE_TTL_SECS`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RequestScopedCacheKey {
    // S07 dimensions.
    pub model_id: String,
    pub association_signature: u64,
    pub channel_count: usize,
    pub latest_channel_update: String,
    pub latest_model_update: String,
    pub channel_cache_version: u64,
    // S16 dimensions.
    pub project_id: String,
    pub api_key_id: String,
    pub model: String,
    pub request_type: RequestType,
    pub stream: bool,
    pub tags_profile_signature: u64,
}

impl RequestScopedCacheKey {
    /// Build the request-scoped key from the S07 association key and the
    /// request-scoped inputs. The `tags_profile_signature` is computed by
    /// [`tags_profile_sig`] over the (sorted) tag list and the profile
    /// signature string — pass `0`/empty when the request has no tags/profile.
    pub fn new(
        association: AssociationCacheKey,
        project_id: impl Into<String>,
        api_key_id: impl Into<String>,
        model: impl Into<String>,
        request_type: RequestType,
        stream: bool,
        tags_profile_signature: u64,
    ) -> Self {
        Self {
            model_id: association.model_id,
            association_signature: association.association_signature,
            channel_count: association.channel_count,
            latest_channel_update: association.latest_channel_update,
            latest_model_update: association.latest_model_update,
            channel_cache_version: association.channel_cache_version,
            project_id: project_id.into(),
            api_key_id: api_key_id.into(),
            model: model.into(),
            request_type,
            stream,
            tags_profile_signature,
        }
    }
}

/// Stable FNV-64a signature over a sorted-unique tag list + an opaque profile
/// signature string. Mirrors the Go profile-signature construction (sorted
/// tags plus profile fingerprint) so identical tag sets produce identical
/// signatures regardless of input order.
pub fn tags_profile_sig(tags: &[String], profile_signature: &str) -> u64 {
    let mut sorted: Vec<String> = tags.to_vec();
    sorted.sort();
    sorted.dedup();
    let mut h = FnvHasher::new();
    h.write_usize(sorted.len());
    for t in &sorted {
        h.write_str(t);
    }
    h.write_str("profile");
    h.write_str(profile_signature);
    h.finish()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
