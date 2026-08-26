//! RUST-P7-008 — declarative provider wrapper outbound registry.
//!
//! Pure-logic mirror of Go `internal/server/biz/channel_llm.go`
//! `build_channel_with_transformer`'s giant `switch c.Type` block
//! (lines 481-1059). The Go side hand-writes one `case` per channel type that
//! all do the same three things:
//!
//! 1. validate credentials (lines 450-473)
//! 2. build a primary outbound transformer keyed off the provider family
//!    (lines 481-1059)
//! 3. pick a `StaticKeyProvider` vs `TraceStickyKeyProvider` based on the
//!    number of enabled API keys (Go `getAPIKeyProvider`, lines 155-170)
//!
//! This module captures ONLY the declarative, input-only pieces of that
//! logic — the three "S" sub-items assigned to Planck-the-3rd in
//! `TODO_SMALL.md`:
//!
//! * **S10/S11** — provider family lookup via [`provider_descriptor`],
//!   which encodes the channel-type → `{ base_path, auth_strategy,
//!   request_transformer_kind, response_transformer_kind }` mapping as a
//!   static table (the Rust analogue of Go's per-case transformer
//!   constructor dispatch).
//! * **S05** — credential-kind validation dispatch via
//!   [`required_credential_kind`], mirroring the Go `switch c.Type` at lines
//!   450-473 (Codex/ClaudeCode accept OAuth OR API key, github_copilot is
//!   OAuth-only, antigravity takes a legacy API key, anthropic_gcp requires
//!   GCP JSON, the *_fake types require nothing, Ollama's API key is
//!   optional, everyone else needs at least one enabled API key).
//! * **S06** — multi-key → `TraceSticky` vs `Static` decision via
//!   [`key_provider_kind`], mirroring Go `getAPIKeyProvider` (lines 155-170).
//!
//! The transformer *construction* itself (Go `doubao.NewOutboundTransformer…`,
//! `anthropic.NewOutboundTransformerWithConfig`, …) is intentionally NOT done
//! here — that wiring belongs to `conduit-services::ChannelService` (RUST-P7-008
//! S04/S08/S09, owned by another handle). This module only exposes the pure
//! lookup/validation helpers those future services will compose against.
//!
//! Every channel-type string here is the literal Go `channel.Type` constant
//! value (see Go `internal/ent/channel/channel.go:200-257`). Lookups are
//! case-insensitive to mirror Go's lowercase normalization done elsewhere in
//! the transformers crate (see `TransformerKey::new` in `traits.rs`).

// ---------------------------------------------------------------------------
// Provider-family tag (S10/S11)
// ---------------------------------------------------------------------------

/// Identifies which outbound-transformer implementation family a channel type
/// maps to. This is the Rust analogue of the Go `switch c.Type { case A, B, C:
/// openai.NewOutboundTransformerWithConfig(...) }` dispatch in
/// `buildChannelWithTransformer` (channel_llm.go:481-1059).
///
/// The tag carries no behavior — it is a pure descriptor that downstream
/// `ChannelService` code will `match` on to construct the real transformer
/// (which needs I/O / providers and is therefore out of scope for this
/// pure-logic module).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderFamily {
    /// Standard OpenAI-compatible chat/completions outbound (Go
    /// `openai.NewOutboundTransformerWithConfig` with
    /// `PlatformType: PlatformOpenAI`). Covers openai, atlascloud,
    /// deepinfra, qiniu, minimax, ppio, siliconflow, vercel, aihubmix,
    /// burncloud, github, opencode_go, evolink.
    OpenAiCompatible,
    /// OpenAI `/responses` outbound (Go `responses.NewOutboundTransformer…`).
    /// Covers openai_responses and nanogpt_responses. The `codex` channel type
    /// is a special case: it routes its `/responses` endpoint through the
    /// `codex` outbound (which itself wraps `responses`), but its *family
    /// tag* is [`ProviderFamily::Responses`] because the credential logic is
    /// identical.
    Responses,
    /// `codex`-flavored `/responses` outbound (Go `buildCodexOutbound`).
    Codex,
    /// `claudecode` outbound (Go `claudecode.NewOutboundTransformer`). Uses
    /// either real OAuth or an API-key-backed fake token provider depending
    /// on credential kind.
    ClaudeCode,
    /// GitHub Copilot outbound (Go `copilot.NewOutboundTransformer`). Strict
    /// OAuth-only (device flow).
    GithubCopilot,
    /// Google Gemini native `/v1beta/.../generateContent` outbound (Go
    /// `gemini.NewOutboundTransformerWithConfig`).
    Gemini,
    /// Google Gemini via Vertex AI (Go `gemini.PlatformVertex`).
    GeminiVertex,
    /// Gemini-as-OpenAI-compatible (Go `geminioai.NewOutboundTransformer…`).
    GeminiOpenAi,
    /// Anthropic native outbound — direct / bedrock / vertex / provider
    /// variant (Go `anthropic.NewOutboundTransformerWithConfig`). The
    /// specific sub-platform (PlatformDirect, PlatformDeepSeek,
    /// PlatformDoubao, PlatformLongCat, PlatformBedrock, PlatformVertex, …)
    /// is recorded in [`ProviderDescriptor::anthropic_platform`].
    Anthropic,
    /// Antigravity outbound (Go `antigravity.NewTransformer`). Carries its
    /// own token refresh; credentials live in the legacy `APIKey` field as
    /// `<refreshToken>|<projectID>`.
    Antigravity,
    /// Jina rerank/embedding outbound (Go `jina.NewOutboundTransformer…`).
    Jina,
    /// Provider-specific OpenAI-compatible outbounds that Go still wraps in
    /// dedicated constructors because of platform-specific quirks (doubao/
    /// volcengine, fireworks, openrouter, cerebras, nanogpt, zai/zhipu,
    /// xiaomi, deepseek, moonshot, xai, modelscope, longcat, bailian, ollama).
    /// The specific provider is recorded in
    /// [`ProviderDescriptor::direct_provider`].
    Direct,
    /// Anthropic fake transformer (Go `anthropic.NewFakeTransformer`). Test
    /// fixture only.
    AnthropicFake,
    /// OpenAI fake transformer (Go `openai.NewFakeTransformer`). Test fixture
    /// only.
    OpenAiFake,
}

impl ProviderFamily {
    /// String tag matching the Go transformer-kind family for diagnostic use.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiCompatible => "openai_compatible",
            Self::Responses => "responses",
            Self::Codex => "codex",
            Self::ClaudeCode => "claudecode",
            Self::GithubCopilot => "github_copilot",
            Self::Gemini => "gemini",
            Self::GeminiVertex => "gemini_vertex",
            Self::GeminiOpenAi => "gemini_openai",
            Self::Anthropic => "anthropic",
            Self::Antigravity => "antigravity",
            Self::Jina => "jina",
            Self::Direct => "direct",
            Self::AnthropicFake => "anthropic_fake",
            Self::OpenAiFake => "openai_fake",
        }
    }
}

// ---------------------------------------------------------------------------
// Auth strategy (S10/S11)
// ---------------------------------------------------------------------------

/// Identifies which authentication scheme the outbound uses. Captures the
/// `auth_strategy` column required by RUST-P7-008 S11 and matches the Go
/// `httpclient.AuthConfig.Type` value sent to the transport layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AuthStrategy {
    /// `Authorization: Bearer <api_key>` — the OpenAI default (Go
    /// `AuthConfig{Type:"bearer"}`).
    Bearer,
    /// Anthropic `x-api-key: <api_key>` header.
    ApiKeyHeader,
    /// Vertex AI / GCP — uses a Google service-account JWT exchange (no
    /// static API key).
    GcpServiceAccount,
    /// OAuth bearer (Codex / ClaudeCode / github_copilot / antigravity) —
    /// the access token comes from a refresh-capable token provider.
    OAuth,
    /// Test fixture — fake transformers carry no auth.
    None,
}

impl AuthStrategy {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bearer => "bearer",
            Self::ApiKeyHeader => "api_key_header",
            Self::GcpServiceAccount => "gcp_service_account",
            Self::OAuth => "oauth",
            Self::None => "none",
        }
    }
}

// ---------------------------------------------------------------------------
// ProviderDescriptor (S10/S11) — one table row per channel type
// ---------------------------------------------------------------------------

/// A single row in the declarative provider table. Mirrors the S11 contract:
/// `channel_type` (table key) → `{ base_path, auth_strategy,
/// request_transformer_kind, response_transformer_kind }`. The
/// `stream_filter_kind` is folded into `response_transformer_kind` since the
/// Rust crate does not yet split stream/response handling per provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderDescriptor {
    /// Family tag for the primary outbound transformer.
    pub family: ProviderFamily,
    /// Authentication scheme used by the outbound transport.
    pub auth_strategy: AuthStrategy,
    /// Default request endpoint path appended to the channel's `base_url` when
    /// no per-endpoint override is set. Mirrors the Go
    /// `openai.Config.EndpointPath` / `responses.Config.EndpointPath` / …
    /// defaults implied by each transformer's `buildFullRequestURL`.
    pub base_path: &'static str,
    /// Request-transformer-kind label (the Go constructor name, e.g.
    /// `"openai.OutboundTransformer"`, `"responses.OutboundTransformer"`,
    /// `"doubao.OutboundTransformer"`). Diagnostic only — the Rust side keys
    /// off [`ProviderDescriptor::family`].
    pub request_transformer_kind: &'static str,
    /// Response-transformer-kind label (same constructor family as the
    /// request side for every current provider). Diagnostic only.
    pub response_transformer_kind: &'static str,
    /// Stream-filter-kind label. Same family as the response side for every
    /// current provider; recorded separately per S11 for forward-compat.
    pub stream_filter_kind: &'static str,
    /// Sub-platform discriminator for the Anthropic family (Go
    /// `anthropic.PlatformDirect|PlatformBedrock|PlatformVertex|
    /// PlatformDeepSeek|PlatformDoubao|PlatformLongCat|PlatformMoonshot|
    /// PlatformZhipu|PlatformZai`). `None` for non-Anthropic families.
    pub anthropic_platform: Option<AnthropicPlatform>,
    /// Sub-provider discriminator for the Direct family. `None` for
    /// non-Direct families.
    pub direct_provider: Option<DirectProvider>,
}

/// Anthropic sub-platform tag — mirrors Go `anthropic.PlatformType` values
/// (Go `llm/transformer/anthropic`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AnthropicPlatform {
    /// `anthropic.PlatformDirect` — anthropic, minimax_anthropic,
    /// volcengine_anthropic, aihubmix_anthropic, xiaomi_anthropic,
    /// evolink_anthropic, bailian_anthropic, moonshot_coding,
    /// opencode_go_anthropic.
    Direct,
    /// `anthropic.PlatformLongCat` — longcat_anthropic.
    LongCat,
    /// `anthropic.PlatformDeepSeek` — deepseek_anthropic.
    DeepSeek,
    /// `anthropic.PlatformDoubao` — doubao_anthropic.
    Doubao,
    /// `anthropic.PlatformMoonshot` — moonshot_anthropic.
    Moonshot,
    /// `anthropic.PlatformZhipu` — zhipu_anthropic.
    Zhipu,
    /// `anthropic.PlatformZai` — zai_anthropic.
    Zai,
    /// `anthropic.PlatformBedrock` — anthropic_aws.
    Bedrock,
    /// `anthropic.PlatformVertex` — anthropic_gcp.
    Vertex,
}

/// Direct-family sub-provider tag — mirrors Go `modelscope|longcat|bailian|
/// openrouter|cerebras|nanogpt|zai|deepseek|moonshot|xai|fireworks|ollama`
/// dedicated constructors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DirectProvider {
    Fireworks,
    Openrouter,
    Cerebras,
    Nanogpt,
    /// `doubao` constructor — used by both `doubao` and `volcengine` (Go
    /// `case TypeDoubao, TypeVolcengine` branch, channel_llm.go:482-493).
    Doubao,
    /// `zai` constructor (used by both `zai` and `zhipu`).
    Zai,
    /// `zai` constructor with `Version: "v1"` (Go xiaomi branch).
    Xiaomi,
    Deepseek,
    Moonshot,
    Xai,
    Modelscope,
    Longcat,
    Bailian,
    /// Ollama — API key optional (see [`CredentialRequirement::OptionalApiKey`]).
    Ollama,
}

// ---------------------------------------------------------------------------
// The declarative table (S10/S11)
// ---------------------------------------------------------------------------

/// One row per Go channel-type constant value. The order loosely follows Go
/// `internal/ent/channel/channel.go:200-257` so the parity audit can diff the
/// two lists side-by-side.
const PROVIDER_TABLE: &[(&str, ProviderDescriptor)] = &[
    // --- openai-compatible (Go channel_llm.go:956-971) ----------------------
    ("openai", openai_compat()),
    ("atlascloud", openai_compat()),
    ("deepinfra", openai_compat()),
    ("qiniu", openai_compat()),
    ("minimax", openai_compat()),
    ("ppio", openai_compat()),
    ("siliconflow", openai_compat()),
    ("vercel", openai_compat()),
    ("aihubmix", openai_compat()),
    ("burncloud", openai_compat()),
    ("github", openai_compat()),
    ("opencode_go", openai_compat()),
    ("evolink", openai_compat()),
    // --- responses (Go channel_llm.go:972-984) ------------------------------
    ("openai_responses", responses()),
    ("nanogpt_responses", responses()),
    // --- codex (Go channel_llm.go:887-896) ----------------------------------
    ("codex", codex()),
    // --- claudecode (Go channel_llm.go:641-706) -----------------------------
    ("claudecode", claudecode()),
    // --- github_copilot (Go channel_llm.go:897-955) -------------------------
    ("github_copilot", github_copilot()),
    // --- anthropic variants (Go channel_llm.go:628-805, 707-785) ------------
    ("anthropic", anthropic_direct(AnthropicPlatform::Direct)),
    (
        "anthropic_aws",
        anthropic_direct(AnthropicPlatform::Bedrock),
    ),
    (
        "anthropic_gcp",
        ProviderDescriptor {
            family: ProviderFamily::Anthropic,
            auth_strategy: AuthStrategy::GcpServiceAccount,
            base_path: "",
            request_transformer_kind: "anthropic.OutboundTransformer",
            response_transformer_kind: "anthropic.OutboundTransformer",
            stream_filter_kind: "anthropic.OutboundTransformer",
            anthropic_platform: Some(AnthropicPlatform::Vertex),
            direct_provider: None,
        },
    ),
    (
        "minimax_anthropic",
        anthropic_direct(AnthropicPlatform::Direct),
    ),
    (
        "volcengine_anthropic",
        anthropic_direct(AnthropicPlatform::Direct),
    ),
    (
        "aihubmix_anthropic",
        anthropic_direct(AnthropicPlatform::Direct),
    ),
    (
        "xiaomi_anthropic",
        anthropic_direct(AnthropicPlatform::Direct),
    ),
    (
        "evolink_anthropic",
        anthropic_direct(AnthropicPlatform::Direct),
    ),
    (
        "bailian_anthropic",
        anthropic_direct(AnthropicPlatform::Direct),
    ),
    (
        "moonshot_coding",
        anthropic_direct(AnthropicPlatform::Direct),
    ),
    (
        "opencode_go_anthropic",
        anthropic_direct(AnthropicPlatform::Direct),
    ),
    (
        "longcat_anthropic",
        anthropic_direct(AnthropicPlatform::LongCat),
    ),
    (
        "deepseek_anthropic",
        anthropic_direct(AnthropicPlatform::DeepSeek),
    ),
    (
        "doubao_anthropic",
        anthropic_direct(AnthropicPlatform::Doubao),
    ),
    (
        "moonshot_anthropic",
        anthropic_direct(AnthropicPlatform::Moonshot),
    ),
    (
        "zhipu_anthropic",
        anthropic_direct(AnthropicPlatform::Zhipu),
    ),
    ("zai_anthropic", anthropic_direct(AnthropicPlatform::Zai)),
    // --- fake transformers (Go channel_llm.go:806-812) ----------------------
    (
        "anthropic_fake",
        ProviderDescriptor {
            family: ProviderFamily::AnthropicFake,
            auth_strategy: AuthStrategy::None,
            base_path: "",
            request_transformer_kind: "anthropic.FakeTransformer",
            response_transformer_kind: "anthropic.FakeTransformer",
            stream_filter_kind: "anthropic.FakeTransformer",
            anthropic_platform: None,
            direct_provider: None,
        },
    ),
    (
        "openai_fake",
        ProviderDescriptor {
            family: ProviderFamily::OpenAiFake,
            auth_strategy: AuthStrategy::None,
            base_path: "",
            request_transformer_kind: "openai.FakeTransformer",
            response_transformer_kind: "openai.FakeTransformer",
            stream_filter_kind: "openai.FakeTransformer",
            anthropic_platform: None,
            direct_provider: None,
        },
    ),
    // --- Direct-provider family (Go channel_llm.go:482-849) -----------------
    (
        "doubao",
        direct(DirectProvider::Doubao, "doubao.OutboundTransformer"),
    ),
    (
        "volcengine",
        direct(DirectProvider::Doubao, "doubao.OutboundTransformer"),
    ),
    (
        "fireworks",
        direct(DirectProvider::Fireworks, "fireworks.OutboundTransformer"),
    ),
    (
        "openrouter",
        direct(DirectProvider::Openrouter, "openrouter.OutboundTransformer"),
    ),
    (
        "cerebras",
        direct(DirectProvider::Cerebras, "cerebras.OutboundTransformer"),
    ),
    (
        "nanogpt",
        direct(DirectProvider::Nanogpt, "nanogpt.OutboundTransformer"),
    ),
    (
        "zai",
        direct(DirectProvider::Zai, "zai.OutboundTransformer"),
    ),
    (
        "zhipu",
        direct(DirectProvider::Zai, "zai.OutboundTransformer"),
    ),
    (
        "xiaomi",
        direct(DirectProvider::Xiaomi, "zai.OutboundTransformer"),
    ),
    (
        "deepseek",
        direct(DirectProvider::Deepseek, "deepseek.OutboundTransformer"),
    ),
    (
        "moonshot",
        direct(DirectProvider::Moonshot, "moonshot.OutboundTransformer"),
    ),
    (
        "xai",
        direct(DirectProvider::Xai, "xai.OutboundTransformer"),
    ),
    (
        "modelscope",
        direct(DirectProvider::Modelscope, "modelscope.OutboundTransformer"),
    ),
    (
        "longcat",
        direct(DirectProvider::Longcat, "longcat.OutboundTransformer"),
    ),
    (
        "bailian",
        direct(DirectProvider::Bailian, "bailian.OutboundTransformer"),
    ),
    (
        "ollama",
        ProviderDescriptor {
            family: ProviderFamily::Direct,
            auth_strategy: AuthStrategy::Bearer,
            base_path: "",
            request_transformer_kind: "ollama.OutboundTransformer",
            response_transformer_kind: "ollama.OutboundTransformer",
            stream_filter_kind: "ollama.OutboundTransformer",
            anthropic_platform: None,
            direct_provider: Some(DirectProvider::Ollama),
        },
    ),
    // --- Gemini variants (Go channel_llm.go:825-836, 985-1009) --------------
    (
        "gemini_openai",
        ProviderDescriptor {
            family: ProviderFamily::GeminiOpenAi,
            auth_strategy: AuthStrategy::Bearer,
            base_path: "",
            request_transformer_kind: "gemini_openai.OutboundTransformer",
            response_transformer_kind: "gemini_openai.OutboundTransformer",
            stream_filter_kind: "gemini_openai.OutboundTransformer",
            anthropic_platform: None,
            direct_provider: None,
        },
    ),
    (
        "gemini",
        ProviderDescriptor {
            family: ProviderFamily::Gemini,
            auth_strategy: AuthStrategy::Bearer,
            base_path: "",
            request_transformer_kind: "gemini.OutboundTransformer",
            response_transformer_kind: "gemini.OutboundTransformer",
            stream_filter_kind: "gemini.OutboundTransformer",
            anthropic_platform: None,
            direct_provider: None,
        },
    ),
    (
        "gemini_vertex",
        ProviderDescriptor {
            family: ProviderFamily::GeminiVertex,
            auth_strategy: AuthStrategy::GcpServiceAccount,
            base_path: "",
            request_transformer_kind: "gemini.OutboundTransformer",
            response_transformer_kind: "gemini.OutboundTransformer",
            stream_filter_kind: "gemini.OutboundTransformer",
            anthropic_platform: None,
            direct_provider: None,
        },
    ),
    // --- Jina (Go channel_llm.go:1010-1021) ---------------------------------
    (
        "jina",
        ProviderDescriptor {
            family: ProviderFamily::Jina,
            auth_strategy: AuthStrategy::Bearer,
            base_path: "",
            request_transformer_kind: "jina.OutboundTransformer",
            response_transformer_kind: "jina.OutboundTransformer",
            stream_filter_kind: "jina.OutboundTransformer",
            anthropic_platform: None,
            direct_provider: None,
        },
    ),
    // --- Antigravity (Go channel_llm.go:1022-1038) --------------------------
    (
        "antigravity",
        ProviderDescriptor {
            family: ProviderFamily::Antigravity,
            auth_strategy: AuthStrategy::OAuth,
            base_path: "",
            request_transformer_kind: "antigravity.Transformer",
            response_transformer_kind: "antigravity.Transformer",
            stream_filter_kind: "antigravity.Transformer",
            anthropic_platform: None,
            direct_provider: None,
        },
    ),
];

// ----- table-row constructor helpers (kept const-callable via `const fn`) ---

const fn openai_compat() -> ProviderDescriptor {
    ProviderDescriptor {
        family: ProviderFamily::OpenAiCompatible,
        auth_strategy: AuthStrategy::Bearer,
        base_path: "/chat/completions",
        request_transformer_kind: "openai.OutboundTransformer",
        response_transformer_kind: "openai.OutboundTransformer",
        stream_filter_kind: "openai.OutboundTransformer",
        anthropic_platform: None,
        direct_provider: None,
    }
}

const fn responses() -> ProviderDescriptor {
    ProviderDescriptor {
        family: ProviderFamily::Responses,
        auth_strategy: AuthStrategy::Bearer,
        base_path: "/responses",
        request_transformer_kind: "responses.OutboundTransformer",
        response_transformer_kind: "responses.OutboundTransformer",
        stream_filter_kind: "responses.OutboundTransformer",
        anthropic_platform: None,
        direct_provider: None,
    }
}

const fn codex() -> ProviderDescriptor {
    ProviderDescriptor {
        family: ProviderFamily::Codex,
        auth_strategy: AuthStrategy::OAuth,
        base_path: "/responses",
        request_transformer_kind: "codex.OutboundTransformer",
        response_transformer_kind: "codex.OutboundTransformer",
        stream_filter_kind: "codex.OutboundTransformer",
        anthropic_platform: None,
        direct_provider: None,
    }
}

const fn claudecode() -> ProviderDescriptor {
    ProviderDescriptor {
        family: ProviderFamily::ClaudeCode,
        auth_strategy: AuthStrategy::OAuth,
        base_path: "/v1/messages",
        request_transformer_kind: "claudecode.OutboundTransformer",
        response_transformer_kind: "claudecode.OutboundTransformer",
        stream_filter_kind: "claudecode.OutboundTransformer",
        anthropic_platform: None,
        direct_provider: None,
    }
}

const fn github_copilot() -> ProviderDescriptor {
    ProviderDescriptor {
        family: ProviderFamily::GithubCopilot,
        auth_strategy: AuthStrategy::OAuth,
        base_path: "/v1/chat/completions",
        request_transformer_kind: "copilot.OutboundTransformer",
        response_transformer_kind: "copilot.OutboundTransformer",
        stream_filter_kind: "copilot.OutboundTransformer",
        anthropic_platform: None,
        direct_provider: None,
    }
}

const fn anthropic_direct(platform: AnthropicPlatform) -> ProviderDescriptor {
    ProviderDescriptor {
        family: ProviderFamily::Anthropic,
        auth_strategy: AuthStrategy::ApiKeyHeader,
        base_path: "/v1/messages",
        request_transformer_kind: "anthropic.OutboundTransformer",
        response_transformer_kind: "anthropic.OutboundTransformer",
        stream_filter_kind: "anthropic.OutboundTransformer",
        anthropic_platform: Some(platform),
        direct_provider: None,
    }
}

const fn direct(provider: DirectProvider, kind: &'static str) -> ProviderDescriptor {
    ProviderDescriptor {
        family: ProviderFamily::Direct,
        auth_strategy: AuthStrategy::Bearer,
        base_path: "",
        request_transformer_kind: kind,
        response_transformer_kind: kind,
        stream_filter_kind: kind,
        anthropic_platform: None,
        direct_provider: Some(provider),
    }
}

/// S10/S11 — Look up the [`ProviderDescriptor`] for a given `channel_type`.
///
/// Returns `None` for unknown types, mirroring Go's
/// `default: return nil, errors.New("unknown channel type")`
/// (channel_llm.go:1057-1059). Lookup is case-insensitive to match Go's
/// lowercasing convention used elsewhere in the transformers crate
/// (`TransformerKey::new`).
pub fn provider_descriptor(channel_type: &str) -> Option<ProviderDescriptor> {
    let key = channel_type.to_ascii_lowercase();
    PROVIDER_TABLE
        .iter()
        .find_map(|(table_key, descriptor)| (*table_key == key).then_some(*descriptor))
}

/// S10/S11 — The list of channel-type strings the registry recognizes.
///
/// Useful for parity audits (the count and set should match Go's
/// `channel.Type` enum) and for rejection-message generation in
/// `ChannelService::build_channel_with_transformer`.
pub fn known_channel_types() -> Vec<&'static str> {
    PROVIDER_TABLE.iter().map(|(key, _)| *key).collect()
}

// ---------------------------------------------------------------------------
// CredentialRequirement (S05)
// ---------------------------------------------------------------------------

/// Identifies what kind of credential a channel type demands, mirroring the
/// validation `switch c.Type` at Go `channel_llm.go:450-473`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CredentialRequirement {
    /// Codex / ClaudeCode — accept OAuth OR at least one enabled API key
    /// (Go channel_llm.go:451-454).
    OAuthOrApiKey,
    /// github_copilot — OAuth credentials are strictly required (device flow)
    /// (Go channel_llm.go:455-459).
    OAuthOnly,
    /// antigravity — legacy `APIKey` field holding a
    /// `<refreshToken>|<projectID>` composite, or OAuth; the Go validation
    /// only checks the API key is non-empty (channel_llm.go:460-464), so we
    /// expose this as "OAuth OR API key" to keep the door open for either.
    /// The descriptor-side family is [`ProviderFamily::Antigravity`].
    AntigravityLegacy,
    /// anthropic_gcp — GCP service-account JSON is required (Go
    /// channel_llm.go:786-791, 465-468).
    GcpCredentials,
    /// anthropic_fake / openai_fake — no credentials (test fixtures) (Go
    /// channel_llm.go:465-468).
    None,
    /// ollama — API key optional, may run keyless locally (Go
    /// channel_llm.go:1039-1056, falls through the default branch only when
    /// keys are configured).
    OptionalApiKey,
    /// everyone else — at least one enabled API key (Go channel_llm.go:469-473).
    ApiKey,
}

impl CredentialRequirement {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OAuthOrApiKey => "oauth_or_api_key",
            Self::OAuthOnly => "oauth_only",
            Self::AntigravityLegacy => "antigravity_legacy",
            Self::GcpCredentials => "gcp_credentials",
            Self::None => "none",
            Self::OptionalApiKey => "optional_api_key",
            Self::ApiKey => "api_key",
        }
    }
}

/// S05 — Look up the credential requirement for a given channel type.
///
/// Mirrors Go `channel_llm.go:450-473` exactly:
///
/// ```text
/// switch c.Type {
/// case channel.TypeCodex, channel.TypeClaudecode:
///     if !IsOAuth() && len(enabledKeys) == 0 { error }
/// case channel.TypeGithubCopilot:
///     if !IsOAuth() { error }
/// case channel.TypeAntigravity:
///     if APIKey == "" { error }
/// case channel.TypeAnthropicGcp, channel.TypeAnthropicFake, channel.TypeOpenaiFake:
///     // skip API key check
/// default:
///     if len(enabledKeys) == 0 { error }
/// }
/// ```
///
/// The "default requires API key" branch is overridden for `ollama`, which Go
/// handles at the *transformer-construction* stage (channel_llm.go:1039-1044:
/// "Ollama is often used locally without API key"). The Rust side surfaces
/// that override here so callers can do a single early validation pass.
pub fn required_credential_kind(channel_type: &str) -> CredentialRequirement {
    let key = channel_type.to_ascii_lowercase();
    match key.as_str() {
        "codex" | "claudecode" => CredentialRequirement::OAuthOrApiKey,
        "github_copilot" => CredentialRequirement::OAuthOnly,
        "antigravity" => CredentialRequirement::AntigravityLegacy,
        "anthropic_gcp" => CredentialRequirement::GcpCredentials,
        "anthropic_fake" | "openai_fake" => CredentialRequirement::None,
        "ollama" => CredentialRequirement::OptionalApiKey,
        _ => CredentialRequirement::ApiKey,
    }
}

// ---------------------------------------------------------------------------
// KeyProviderKind (S06)
// ---------------------------------------------------------------------------

/// Identifies the `auth.APIKeyProvider` implementation Go would pick for a
/// given channel, mirroring `getAPIKeyProvider` (channel_llm.go:155-170):
///
/// ```text
/// if apiKeyOverride != "" { return StaticKeyProvider(override) }
/// if len(enabled) > 1     { return NewTraceStickyKeyProvider(ch) }
/// if len(enabled) == 1    { return StaticKeyProvider(enabled[0]) }
/// panic("no enabled api key")
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyProviderKind {
    /// Go `auth.NewStaticKeyProvider` — single enabled key OR an override.
    Static,
    /// Go `NewTraceStickyKeyProvider(ch)` — multiple enabled keys, hashed per
    /// trace for sticky routing.
    TraceSticky,
    /// Ollama (and any other keyless-optional channel) with zero enabled keys
    /// — Go leaves `apiKeyProvider` nil (channel_llm.go:1041-1043).
    None,
}

impl KeyProviderKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Static => "static",
            Self::TraceSticky => "trace_sticky",
            Self::None => "none",
        }
    }
}

/// S06 — Decide which key provider the channel should use given the channel
/// type and the count of enabled API keys.
///
/// Mirrors Go `getAPIKeyProvider` (channel_llm.go:155-170) plus the Ollama
/// special-case at channel_llm.go:1039-1044 (which leaves the provider nil
/// when zero keys are configured).
///
/// `enabled_key_count` is the length of `Credentials.GetEnabledAPIKeys(...)`
/// (already excluding disabled keys). A non-zero `api_key_override` (the
/// Go `apiKeyOverride` parameter) collapses the decision to
/// [`KeyProviderKind::Static`] regardless of count — callers should pass
/// `Some(...)` when the override is set; this helper folds it in via the
/// `has_override` flag.
pub fn key_provider_kind(
    channel_type: &str,
    enabled_key_count: usize,
    has_override: bool,
) -> KeyProviderKind {
    // Go: `if ch.apiKeyOverride != "" { return auth.NewStaticKeyProvider(...) }`
    if has_override {
        return KeyProviderKind::Static;
    }

    // Ollama with zero keys: Go explicitly leaves the provider nil (the
    // transformer accepts a nil APIKeyProvider).
    let requirement = required_credential_kind(channel_type);
    if matches!(requirement, CredentialRequirement::OptionalApiKey) && enabled_key_count == 0 {
        return KeyProviderKind::None;
    }

    if enabled_key_count > 1 {
        KeyProviderKind::TraceSticky
    } else {
        // len == 1 → Static; len == 0 → Go panics, but the credential
        // validation at buildChannelWithTransformer (channel_llm.go:450-473)
        // is supposed to have failed first. We surface Static here so callers
        // that have already validated credentials can use this without
        // panicking; the upstream validation step owns the "no keys" error.
        KeyProviderKind::Static
    }
}

// ---------------------------------------------------------------------------
// Tests — mirror Go golden intent (per-channel-type lookup, credential kind,
// key-provider kind). No Go *_test.go table maps 1:1 to these helpers (Go
// tests them indirectly through ChannelService); the assertions here pin the
// pure-logic contract the future ChannelService port must respect.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ----- S10/S11 provider_descriptor --------------------------------------

    #[test]
    fn provider_descriptor_openai_compatible_family_covers_go_switch_956_971() -> Result<(), String>
    {
        // Mirrors Go channel_llm.go:956-971 — one shared openai-compatible
        // branch for these 13 channel types.
        let openai_compat_channels = [
            "openai",
            "atlascloud",
            "deepinfra",
            "qiniu",
            "minimax",
            "ppio",
            "siliconflow",
            "vercel",
            "aihubmix",
            "burncloud",
            "github",
            "opencode_go",
            "evolink",
        ];
        for ct in openai_compat_channels {
            let descriptor =
                provider_descriptor(ct).ok_or_else(|| format!("missing descriptor for {ct}"))?;
            assert_eq!(
                descriptor.family,
                ProviderFamily::OpenAiCompatible,
                "channel {ct}"
            );
            assert_eq!(descriptor.auth_strategy, AuthStrategy::Bearer);
            assert_eq!(descriptor.base_path, "/chat/completions");
            assert_eq!(
                descriptor.request_transformer_kind,
                "openai.OutboundTransformer"
            );
            assert_eq!(
                descriptor.response_transformer_kind,
                descriptor.request_transformer_kind
            );
            assert_eq!(
                descriptor.stream_filter_kind,
                descriptor.request_transformer_kind
            );
            assert_eq!(descriptor.anthropic_platform, None);
            assert_eq!(descriptor.direct_provider, None);
        }
        Ok(())
    }

    #[test]
    fn provider_descriptor_responses_family_covers_go_switch_972_984() -> Result<(), String> {
        // Mirrors Go channel_llm.go:972-984 — openai_responses and
        // nanogpt_responses share the responses outbound.
        for ct in ["openai_responses", "nanogpt_responses"] {
            let descriptor =
                provider_descriptor(ct).ok_or_else(|| format!("missing descriptor for {ct}"))?;
            assert_eq!(descriptor.family, ProviderFamily::Responses, "channel {ct}");
            assert_eq!(descriptor.auth_strategy, AuthStrategy::Bearer);
            assert_eq!(descriptor.base_path, "/responses");
            assert_eq!(
                descriptor.request_transformer_kind,
                "responses.OutboundTransformer"
            );
        }
        Ok(())
    }

    #[test]
    fn provider_descriptor_codex_uses_codex_family_go_switch_887_896() -> Result<(), String> {
        let descriptor = provider_descriptor("codex").ok_or("codex descriptor must be present")?;
        assert_eq!(descriptor.family, ProviderFamily::Codex);
        assert_eq!(descriptor.auth_strategy, AuthStrategy::OAuth);
        assert_eq!(descriptor.base_path, "/responses");
        assert_eq!(
            descriptor.request_transformer_kind,
            "codex.OutboundTransformer"
        );
        Ok(())
    }

    #[test]
    fn provider_descriptor_claudecode_uses_claudecode_family_go_switch_641_706()
    -> Result<(), String> {
        let descriptor =
            provider_descriptor("claudecode").ok_or("claudecode descriptor must be present")?;
        assert_eq!(descriptor.family, ProviderFamily::ClaudeCode);
        assert_eq!(descriptor.auth_strategy, AuthStrategy::OAuth);
        assert_eq!(descriptor.base_path, "/v1/messages");
        Ok(())
    }

    #[test]
    fn provider_descriptor_github_copilot_is_strict_oauth_go_switch_897_955() -> Result<(), String>
    {
        let descriptor = provider_descriptor("github_copilot")
            .ok_or("github_copilot descriptor must be present")?;
        assert_eq!(descriptor.family, ProviderFamily::GithubCopilot);
        assert_eq!(descriptor.auth_strategy, AuthStrategy::OAuth);
        Ok(())
    }

    #[test]
    fn provider_descriptor_anthropic_direct_variants_share_family_go_switch_628_785()
    -> Result<(), String> {
        // Mirrors the Go `case TypeAnthropic, TypeMinimaxAnthropic, …`
        // branch (channel_llm.go:628) and the per-Platform* branches
        // (deepseek_anthropic line 707, doubao_anthropic 720, etc.).
        let direct_anthropic = [
            ("anthropic", AnthropicPlatform::Direct),
            ("minimax_anthropic", AnthropicPlatform::Direct),
            ("volcengine_anthropic", AnthropicPlatform::Direct),
            ("aihubmix_anthropic", AnthropicPlatform::Direct),
            ("xiaomi_anthropic", AnthropicPlatform::Direct),
            ("evolink_anthropic", AnthropicPlatform::Direct),
            ("bailian_anthropic", AnthropicPlatform::Direct),
            ("moonshot_coding", AnthropicPlatform::Direct),
            ("opencode_go_anthropic", AnthropicPlatform::Direct),
        ];
        for (ct, platform) in direct_anthropic {
            let descriptor =
                provider_descriptor(ct).ok_or_else(|| format!("missing descriptor for {ct}"))?;
            assert_eq!(descriptor.family, ProviderFamily::Anthropic, "channel {ct}");
            assert_eq!(descriptor.auth_strategy, AuthStrategy::ApiKeyHeader);
            assert_eq!(
                descriptor.anthropic_platform,
                Some(platform),
                "channel {ct}"
            );
            assert_eq!(descriptor.base_path, "/v1/messages", "channel {ct}");
        }

        // Per-Platform* variants.
        assert_eq!(
            provider_descriptor("longcat_anthropic").map(|d| d.anthropic_platform),
            Some(Some(AnthropicPlatform::LongCat))
        );
        assert_eq!(
            provider_descriptor("deepseek_anthropic").map(|d| d.anthropic_platform),
            Some(Some(AnthropicPlatform::DeepSeek))
        );
        assert_eq!(
            provider_descriptor("doubao_anthropic").map(|d| d.anthropic_platform),
            Some(Some(AnthropicPlatform::Doubao))
        );
        assert_eq!(
            provider_descriptor("moonshot_anthropic").map(|d| d.anthropic_platform),
            Some(Some(AnthropicPlatform::Moonshot))
        );
        assert_eq!(
            provider_descriptor("zhipu_anthropic").map(|d| d.anthropic_platform),
            Some(Some(AnthropicPlatform::Zhipu))
        );
        assert_eq!(
            provider_descriptor("zai_anthropic").map(|d| d.anthropic_platform),
            Some(Some(AnthropicPlatform::Zai))
        );
        assert_eq!(
            provider_descriptor("anthropic_aws").map(|d| d.anthropic_platform),
            Some(Some(AnthropicPlatform::Bedrock))
        );
        Ok(())
    }

    #[test]
    fn provider_descriptor_anthropic_gcp_uses_gcp_auth_go_switch_786_805() -> Result<(), String> {
        // Mirrors Go channel_llm.go:786-805 — anthropic_gcp uses Vertex
        // platform + GCP service-account credentials, not an API key.
        let descriptor = provider_descriptor("anthropic_gcp")
            .ok_or("anthropic_gcp descriptor must be present")?;
        assert_eq!(descriptor.family, ProviderFamily::Anthropic);
        assert_eq!(descriptor.auth_strategy, AuthStrategy::GcpServiceAccount);
        assert_eq!(
            descriptor.anthropic_platform,
            Some(AnthropicPlatform::Vertex)
        );
        Ok(())
    }

    #[test]
    fn provider_descriptor_fake_transformers_require_no_auth_go_switch_806_812()
    -> Result<(), String> {
        // Mirrors Go channel_llm.go:806-812 — anthropic_fake / openai_fake
        // are test fixtures.
        let anthropic_fake = provider_descriptor("anthropic_fake")
            .ok_or("anthropic_fake descriptor must be present")?;
        assert_eq!(anthropic_fake.family, ProviderFamily::AnthropicFake);
        assert_eq!(anthropic_fake.auth_strategy, AuthStrategy::None);

        let openai_fake =
            provider_descriptor("openai_fake").ok_or("openai_fake descriptor must be present")?;
        assert_eq!(openai_fake.family, ProviderFamily::OpenAiFake);
        assert_eq!(openai_fake.auth_strategy, AuthStrategy::None);
        Ok(())
    }

    #[test]
    fn provider_descriptor_doubao_volcengine_share_doubao_provider_go_switch_482_493()
    -> Result<(), String> {
        // Mirrors Go channel_llm.go:482-493 — `case TypeDoubao, TypeVolcengine`
        // shares a dedicated `doubao.NewOutboundTransformer…` constructor.
        for ct in ["doubao", "volcengine"] {
            let descriptor =
                provider_descriptor(ct).ok_or_else(|| format!("missing descriptor for {ct}"))?;
            assert_eq!(descriptor.family, ProviderFamily::Direct, "channel {ct}");
            assert_eq!(descriptor.auth_strategy, AuthStrategy::Bearer);
            assert_eq!(
                descriptor.direct_provider,
                Some(DirectProvider::Doubao),
                "channel {ct}"
            );
            assert_eq!(
                descriptor.request_transformer_kind, "doubao.OutboundTransformer",
                "channel {ct}"
            );
        }
        Ok(())
    }

    #[test]
    fn provider_descriptor_direct_provider_variants_go_switch_494_849() -> Result<(), String> {
        // Each direct provider gets its own dedicated outbound in Go.
        let cases: &[(&str, DirectProvider, &str)] = &[
            (
                "fireworks",
                DirectProvider::Fireworks,
                "fireworks.OutboundTransformer",
            ),
            (
                "openrouter",
                DirectProvider::Openrouter,
                "openrouter.OutboundTransformer",
            ),
            (
                "cerebras",
                DirectProvider::Cerebras,
                "cerebras.OutboundTransformer",
            ),
            (
                "nanogpt",
                DirectProvider::Nanogpt,
                "nanogpt.OutboundTransformer",
            ),
            ("zai", DirectProvider::Zai, "zai.OutboundTransformer"),
            ("zhipu", DirectProvider::Zai, "zai.OutboundTransformer"),
            ("xiaomi", DirectProvider::Xiaomi, "zai.OutboundTransformer"),
            (
                "deepseek",
                DirectProvider::Deepseek,
                "deepseek.OutboundTransformer",
            ),
            (
                "moonshot",
                DirectProvider::Moonshot,
                "moonshot.OutboundTransformer",
            ),
            ("xai", DirectProvider::Xai, "xai.OutboundTransformer"),
            (
                "modelscope",
                DirectProvider::Modelscope,
                "modelscope.OutboundTransformer",
            ),
            (
                "longcat",
                DirectProvider::Longcat,
                "longcat.OutboundTransformer",
            ),
            (
                "bailian",
                DirectProvider::Bailian,
                "bailian.OutboundTransformer",
            ),
        ];
        for (ct, provider, kind) in cases {
            let descriptor =
                provider_descriptor(ct).ok_or_else(|| format!("missing descriptor for {ct}"))?;
            assert_eq!(descriptor.family, ProviderFamily::Direct, "channel {ct}");
            assert_eq!(
                descriptor.auth_strategy,
                AuthStrategy::Bearer,
                "channel {ct}"
            );
            assert_eq!(descriptor.direct_provider, Some(*provider), "channel {ct}");
            assert_eq!(descriptor.request_transformer_kind, *kind, "channel {ct}");
        }
        Ok(())
    }

    #[test]
    fn provider_descriptor_ollama_is_direct_family_with_optional_keys_go_switch_1039_1056()
    -> Result<(), String> {
        let descriptor =
            provider_descriptor("ollama").ok_or("ollama descriptor must be present")?;
        assert_eq!(descriptor.family, ProviderFamily::Direct);
        assert_eq!(descriptor.direct_provider, Some(DirectProvider::Ollama));
        assert_eq!(descriptor.auth_strategy, AuthStrategy::Bearer);
        Ok(())
    }

    #[test]
    fn provider_descriptor_gemini_variants_go_switch_825_1009() -> Result<(), String> {
        let gemini_openai = provider_descriptor("gemini_openai")
            .ok_or("gemini_openai descriptor must be present")?;
        assert_eq!(gemini_openai.family, ProviderFamily::GeminiOpenAi);
        assert_eq!(gemini_openai.auth_strategy, AuthStrategy::Bearer);

        let gemini = provider_descriptor("gemini").ok_or("gemini descriptor must be present")?;
        assert_eq!(gemini.family, ProviderFamily::Gemini);
        assert_eq!(gemini.auth_strategy, AuthStrategy::Bearer);

        let gemini_vertex = provider_descriptor("gemini_vertex")
            .ok_or("gemini_vertex descriptor must be present")?;
        assert_eq!(gemini_vertex.family, ProviderFamily::GeminiVertex);
        assert_eq!(gemini_vertex.auth_strategy, AuthStrategy::GcpServiceAccount);
        Ok(())
    }

    #[test]
    fn provider_descriptor_jina_and_antigravity_go_switch_1010_1038() -> Result<(), String> {
        let jina = provider_descriptor("jina").ok_or("jina descriptor must be present")?;
        assert_eq!(jina.family, ProviderFamily::Jina);
        assert_eq!(jina.auth_strategy, AuthStrategy::Bearer);

        let antigravity =
            provider_descriptor("antigravity").ok_or("antigravity descriptor must be present")?;
        assert_eq!(antigravity.family, ProviderFamily::Antigravity);
        assert_eq!(antigravity.auth_strategy, AuthStrategy::OAuth);
        Ok(())
    }

    #[test]
    fn provider_descriptor_unknown_channel_type_returns_none() {
        // Mirrors Go's `default: return nil, errors.New("unknown channel type")`.
        assert_eq!(provider_descriptor("not-a-real-channel"), None);
        assert_eq!(provider_descriptor(""), None);
    }

    #[test]
    fn provider_descriptor_lookup_is_case_insensitive() {
        // Matches the lowercase normalization Go and the Rust transformers
        // crate both apply for channel-type keys.
        assert_eq!(
            provider_descriptor("OpenAI").map(|d| d.family),
            Some(ProviderFamily::OpenAiCompatible)
        );
        assert_eq!(
            provider_descriptor("GITHUB_Copilot").map(|d| d.family),
            Some(ProviderFamily::GithubCopilot)
        );
    }

    #[test]
    fn known_channel_types_covers_every_go_channel_type_constant() {
        // Mirrors Go internal/ent/channel/channel.go:200-257 — every Type*
        // constant defined there should appear in the registry. The count
        // (57 as of the Go snapshot) is the parity pin.
        let known = known_channel_types();

        // Every channel.Type* value from Go channel.go:200-257:
        let expected = [
            "openai",
            "openai_responses",
            "atlascloud",
            "codex",
            "vercel",
            "anthropic",
            "anthropic_aws",
            "anthropic_gcp",
            "gemini_openai",
            "gemini",
            "gemini_vertex",
            "deepseek",
            "deepseek_anthropic",
            "deepinfra",
            "qiniu",
            "fireworks",
            "doubao",
            "doubao_anthropic",
            "moonshot",
            "moonshot_anthropic",
            "zhipu",
            "zai",
            "zhipu_anthropic",
            "zai_anthropic",
            "anthropic_fake",
            "openai_fake",
            "openrouter",
            "xiaomi",
            "xiaomi_anthropic",
            "xai",
            "ppio",
            "siliconflow",
            "volcengine",
            "volcengine_anthropic",
            "longcat",
            "longcat_anthropic",
            "minimax",
            "minimax_anthropic",
            "aihubmix",
            "aihubmix_anthropic",
            "burncloud",
            "modelscope",
            "bailian",
            "bailian_anthropic",
            "moonshot_coding",
            "jina",
            "github",
            "github_copilot",
            "claudecode",
            "cerebras",
            "antigravity",
            "nanogpt",
            "nanogpt_responses",
            "opencode_go",
            "opencode_go_anthropic",
            "ollama",
            "evolink",
            "evolink_anthropic",
        ];

        for ct in expected {
            assert!(
                known.contains(&ct),
                "registry missing channel type {ct:?} (parity with Go channel.go)"
            );
        }

        // And no extras.
        assert_eq!(
            known.len(),
            expected.len(),
            "registry has {known:?} but Go declares {expected:?}"
        );
    }

    // ----- S05 required_credential_kind -------------------------------------

    #[test]
    fn required_credential_kind_codex_and_claudecode_accept_oauth_or_api_key_go_451_454() {
        assert_eq!(
            required_credential_kind("codex"),
            CredentialRequirement::OAuthOrApiKey
        );
        assert_eq!(
            required_credential_kind("claudecode"),
            CredentialRequirement::OAuthOrApiKey
        );
    }

    #[test]
    fn required_credential_kind_github_copilot_is_oauth_only_go_455_459() {
        assert_eq!(
            required_credential_kind("github_copilot"),
            CredentialRequirement::OAuthOnly
        );
    }

    #[test]
    fn required_credential_kind_antigravity_is_legacy_key_go_460_464() {
        assert_eq!(
            required_credential_kind("antigravity"),
            CredentialRequirement::AntigravityLegacy
        );
    }

    #[test]
    fn required_credential_kind_anthropic_gcp_requires_gcp_credentials_go_465_468() {
        assert_eq!(
            required_credential_kind("anthropic_gcp"),
            CredentialRequirement::GcpCredentials
        );
    }

    #[test]
    fn required_credential_kind_fake_transformers_require_nothing_go_465_468() {
        assert_eq!(
            required_credential_kind("anthropic_fake"),
            CredentialRequirement::None
        );
        assert_eq!(
            required_credential_kind("openai_fake"),
            CredentialRequirement::None
        );
    }

    #[test]
    fn required_credential_kind_ollama_allows_no_keys_go_1039_1044() {
        assert_eq!(
            required_credential_kind("ollama"),
            CredentialRequirement::OptionalApiKey
        );
    }

    #[test]
    fn required_credential_kind_default_requires_api_key_go_469_473() {
        // Default branch — every channel type not special-cased above.
        for ct in [
            "openai",
            "deepseek",
            "doubao",
            "fireworks",
            "anthropic",
            "gemini",
            "zhipu",
            "xai",
            "openai_responses",
            "openrouter",
        ] {
            assert_eq!(
                required_credential_kind(ct),
                CredentialRequirement::ApiKey,
                "channel {ct}"
            );
        }
    }

    #[test]
    fn required_credential_kind_unknown_channel_type_falls_through_to_api_key() {
        // Go's default branch — an unrecognized type goes to the
        // `if len(enabledKeys) == 0 { error }` check before the
        // transformer-construction switch rejects it as "unknown channel
        // type". Mirror that ordering: credential check first.
        assert_eq!(
            required_credential_kind("not-a-real-channel"),
            CredentialRequirement::ApiKey
        );
    }

    // ----- S06 key_provider_kind --------------------------------------------

    #[test]
    fn key_provider_kind_single_key_is_static_go_155_170() {
        assert_eq!(
            key_provider_kind("openai", 1, false),
            KeyProviderKind::Static
        );
        assert_eq!(
            key_provider_kind("deepseek", 1, false),
            KeyProviderKind::Static
        );
    }

    #[test]
    fn key_provider_kind_multiple_keys_is_trace_sticky_go_161_163() {
        assert_eq!(
            key_provider_kind("openai", 2, false),
            KeyProviderKind::TraceSticky
        );
        assert_eq!(
            key_provider_kind("openai", 5, false),
            KeyProviderKind::TraceSticky
        );
    }

    #[test]
    fn key_provider_kind_api_key_override_forces_static_go_156_158() {
        // Go: `if ch.apiKeyOverride != "" { return auth.NewStaticKeyProvider(...) }`
        assert_eq!(
            key_provider_kind("openai", 10, true),
            KeyProviderKind::Static
        );
        assert_eq!(
            key_provider_kind("openai", 0, true),
            KeyProviderKind::Static
        );
    }

    #[test]
    fn key_provider_kind_ollama_with_zero_keys_is_none_go_1039_1044() {
        // Mirrors Go's "Ollama is often used locally without API key" —
        // the provider is left nil when no keys are configured.
        assert_eq!(key_provider_kind("ollama", 0, false), KeyProviderKind::None);
    }

    #[test]
    fn key_provider_kind_ollama_with_keys_still_uses_provider_logic_go_1042_1044() {
        assert_eq!(
            key_provider_kind("ollama", 1, false),
            KeyProviderKind::Static
        );
        assert_eq!(
            key_provider_kind("ollama", 3, false),
            KeyProviderKind::TraceSticky
        );
    }

    #[test]
    fn key_provider_kind_zero_keys_non_optional_returns_static_go_155_170() {
        // Mirrors Go behavior: `len(enabled) == 0` would panic, but the
        // credential-validation switch at channel_llm.go:450-473 should have
        // rejected the channel first. We surface Static so callers that have
        // already validated can use this without panicking; the upstream
        // validation step owns the "no keys" error.
        assert_eq!(
            key_provider_kind("openai", 0, false),
            KeyProviderKind::Static
        );
    }

    #[test]
    fn key_provider_kind_fake_transformers_default_to_static() {
        // *_fake channels don't use any keys; the credential check returns
        // None for them, so the key-provider decision is moot. Default
        // behavior (Static at count 0) is safe since no transformer uses
        // the provider.
        assert_eq!(
            key_provider_kind("anthropic_fake", 0, false),
            KeyProviderKind::Static
        );
    }

    // ----- enum as_str smoke ------------------------------------------------

    #[test]
    fn enum_as_str_tags_are_stable() {
        // Pin the diagnostic labels so downstream tooling (logs, error
        // messages) can match on them.
        assert_eq!(
            ProviderFamily::OpenAiCompatible.as_str(),
            "openai_compatible"
        );
        assert_eq!(AuthStrategy::Bearer.as_str(), "bearer");
        assert_eq!(CredentialRequirement::ApiKey.as_str(), "api_key");
        assert_eq!(KeyProviderKind::TraceSticky.as_str(), "trace_sticky");
    }
}
