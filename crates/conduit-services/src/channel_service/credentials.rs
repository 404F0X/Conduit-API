//! S07 credentials selection (multi-API-key rotation, disabled-key skip,
//! OAuth/Azure/GCP kind dispatch).
//!
//! Ported from the pure-decision core of Go:
//! - `internal/server/biz/channel_llm.go` `getAPIKeyProvider` (the
//!   `apiKeyOverride > multi-key > single-key > panic` branching).
//! - `internal/server/biz/channel_apikey_provider.go` `TraceStickyKeyProvider.Get`
//!   (sticky-by-trace selection with LRU cache + rendezvous hashing fallback,
//!   and a random pick when no trace is present).
//! - `internal/server/biz/channel_apikey_provider.go` `rendezvousSelect` /
//!   `hashAPIKey` (FNV-1a 64-bit Highest Random Weight hashing).
//! - `internal/server/biz/channel_apikey.go` (disabled-key tracking surface).
//!
//! The HTTP auth materialization for OAuth / Azure / GCP is intentionally out
//! of scope (it lives in the transformer/oauth ports). This module captures
//! only the *decision*: which credential kind and which concrete API key (if
//! any) should be used for the next request, given a channel's credential
//! snapshot, the disabled-key state, the per-request trace id, and an optional
//! explicit override key.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Stable, non-secret identity for correlating one configured credential
/// across routing, execution persistence, and health aggregation.
///
/// The plaintext credential must never be written to metadata or storage.
pub fn credential_fingerprint(secret: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(secret.as_bytes()))
}

/// FNV-1a 64-bit hash, mirroring Go `hashAPIKey` (`hash/fnv` `New64a`).
///
/// Used as the scoring function for rendezvous (Highest Random Weight)
/// hashing so trace-sticky selection is stable under key-set churn.
pub fn hash_api_key_fnv64a(s: &str) -> u64 {
    // FNV-1a 64-bit offset basis and prime (matches Go's `hash/fnv`).
    const FNV_OFFSET_BASIS_64: u64 = 0xcbf29ce484222325;
    const FNV_PRIME_64: u64 = 0x100000001b3;

    let mut hash = FNV_OFFSET_BASIS_64;
    for &byte in s.as_bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(FNV_PRIME_64);
    }
    hash
}

/// Pick one key from `keys` using rendezvous (Highest Random Weight) hashing
/// with `seed` as the per-request discriminator. Ported 1:1 from Go
/// `rendezvousSelect`.
///
/// Stable and order-independent: the winning key is the one maximizing
/// `hash(seed + "|" + key)`, so adding/removing a non-winning key leaves
/// existing selections unchanged.
///
/// Returns `None` only when `keys` is empty.
pub fn rendezvous_select<'a>(keys: &'a [String], seed: &str) -> Option<&'a str> {
    if keys.is_empty() {
        return None;
    }
    let mut best_key = keys[0].as_str();
    let mut best_score = hash_api_key_fnv64a(&format!("{seed}|{best_key}"));
    for key in &keys[1..] {
        let score = hash_api_key_fnv64a(&format!("{seed}|{key}"));
        if score > best_score {
            best_score = score;
            best_key = key.as_str();
        }
    }
    Some(best_key)
}

/// The kind of credential that backs a channel. Drives the HTTP-auth builder
/// in the transformer layer (out of scope here); the decision helper only
/// reports which branch Go's `Channel.GetCredential`-family would take.
///
/// Mirrors Go's per-channel-type branches in
/// `internal/server/biz/channel_llm.go` (`IsOAuth` / Azure / GCP / API-key).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialKind {
    /// OAuth (`oauth` field populated with a non-empty `access_token`, or the
    /// legacy `api_key` field contains an OAuth JSON blob). The OAuth
    /// `access_token` itself is the rotated secret for HTTP auth.
    OAuth,
    /// Azure-managed configuration (`credentials.azure` present).
    Azure,
    /// GCP service-account credentials (`credentials.gcp` present).
    Gcp,
    /// Plain API key(s). The selected concrete key lives in
    /// [`CredentialDecision::api_key`].
    ApiKey,
}

/// Channel credential snapshot consumed by the selection helper. This is a
/// minimal pure-data view; the real `Channel` port will plug into the
/// `conduit_core::objects::channel_settings::ChannelCredentials` type and the
/// channel's cached disabled-key set.
///
/// `api_keys` is the *enabled* key list (Go's `cachedEnabledAPIKeys`):
/// disabled keys are already filtered out by the caller via
/// [`conduit_core::objects::channel_settings::ChannelCredentials::get_enabled_api_keys`].
/// This keeps the decision function pure with respect to its inputs.
//
// [Helmholtz-the-3rd ?] TODO: swap this local snapshot for the real `Channel`
// once the ent-backed `Channel` struct (Go's `biz.Channel` cached-fields port)
// lands in `conduit-services`. The selection helper signature
// (`decide_credential`) is stable and won't change — only the construction
// site moves.
#[derive(Debug, Clone, Default)]
pub struct CredentialSnapshot<'a> {
    /// All configured API keys (legacy `api_key` + `api_keys`), already
    /// filtered to remove disabled keys. Mirrors Go's
    /// `Channel.cachedEnabledAPIKeys`. When empty and the channel is not
    /// OAuth, the channel is considered key-exhausted.
    pub enabled_api_keys: &'a [String],
    /// Legacy single `api_key` field, used as the no-trace fallback when the
    /// enabled list is empty (mirrors Go's
    /// `TraceStickyKeyProvider.Get` fallback to `Credentials.APIKeys[0]`).
    /// For OAuth channels this is the raw OAuth JSON blob.
    pub legacy_api_key: &'a str,
    /// True when the channel has a populated OAuth credential (Go
    /// `Credentials.IsOAuth()`).
    pub is_oauth: bool,
    /// True when an Azure credential is configured.
    pub has_azure: bool,
    /// True when a GCP credential is configured.
    pub has_gcp: bool,
    /// Per-request override key (Go's `apiKeyOverride`): when non-empty it
    /// always wins. Used by the channel-probe path to force a specific key.
    pub override_key: &'a str,
}

impl<'a> CredentialSnapshot<'a> {
    /// Build a snapshot from a `ChannelCredentials` value plus the runtime
    /// state that lives outside the credentials struct (enabled keys and the
    /// request override). Convenience constructor for the common case.
    pub fn from_credentials(
        creds: &'a conduit_core::objects::channel_settings::ChannelCredentials,
        enabled_api_keys: &'a [String],
        override_key: &'a str,
    ) -> Self {
        Self {
            enabled_api_keys,
            legacy_api_key: &creds.api_key,
            is_oauth: creds.is_oauth(),
            has_azure: creds.azure.is_some(),
            has_gcp: creds.gcp.is_some(),
            override_key,
        }
    }
}

/// The credential-kind dispatch plus the concrete API key to use (when
/// applicable). Produced by [`decide_credential`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialDecision<'a> {
    /// Which credential branch the channel uses.
    pub kind: CredentialKind,
    /// The selected API key, when `kind == ApiKey`. `None` for OAuth/Azure/GCP
    /// (their auth materialization is deferred to the transformer layer).
    pub api_key: Option<&'a str>,
    /// Whether the enabled-key set is exhausted (no enabled API key remains
    /// and the channel is not OAuth). Callers translate this into a
    /// channel-disable action — mirrors Go's
    /// `len(enabledKeys) == 0` branch in `DisableAPIKey`.
    pub keys_exhausted: bool,
}

/// Trace-state input for sticky selection. Mirrors Go's
/// `contexts.GetTrace(ctx)` — the trace is *optional*.
#[derive(Debug, Clone, Default)]
pub struct TraceKeyState<'a> {
    /// The per-request trace id, when present. Same-trace re-attempts select
    /// the same key (Go: LRU-cached rendezvous pick).
    pub trace_id: Option<&'a str>,
    /// Previously cached sticky pick for this trace id, if any. When the
    /// cached key is still in the enabled set, it is reused verbatim
    /// (mirrors Go's LRU cache hit). `None` means no prior selection.
    pub cached_sticky_key: Option<&'a str>,
}

/// Decide which credential kind and which concrete API key (if any) the next
/// request should use. Pure: no IO, no LRU allocation. Mirrors Go's
/// `getAPIKeyProvider` + `TraceStickyKeyProvider.Get` decision tree:
///
/// 1. If `snapshot.override_key` is set, return it as the single static key.
/// 2. Else if the channel is OAuth / Azure / GCP, return the corresponding
///    kind with no per-key selection (HTTP auth is the transformer's job).
/// 3. Else (plain API-key channel):
///    a. If the enabled list has >= 1 key, pick one (sticky-by-trace when a
///       trace id is present and the cached pick is still valid; otherwise
///       rendezvous hashing on the trace id; otherwise the single enabled key
///       when there is exactly one, or the first key as a deterministic
///       no-trace fallback — see note below).
///    b. If the enabled list is empty, mark the channel as `keys_exhausted`.
///
/// **No-trace fallback note:** Go's `TraceStickyKeyProvider.Get` uses
/// `rand.IntN` when no trace is present, which is intentionally non-deterministic.
/// A pure function cannot replicate that without a RNG seed, and the routing
/// layer above always injects a trace id anyway; we therefore fall back to the
/// first enabled key (deterministic). Callers that need the random behaviour
/// should pass a synthetic trace id or pre-compute the index themselves.
pub fn decide_credential<'a>(
    snapshot: &CredentialSnapshot<'a>,
    trace: &TraceKeyState<'a>,
) -> CredentialDecision<'a> {
    // 1. Explicit per-request override always wins (Go: apiKeyOverride).
    if !snapshot.override_key.is_empty() {
        return CredentialDecision {
            kind: CredentialKind::ApiKey,
            api_key: Some(snapshot.override_key),
            keys_exhausted: false,
        };
    }

    // 2. Credential-kind dispatch.
    let kind = if snapshot.is_oauth {
        CredentialKind::OAuth
    } else if snapshot.has_azure {
        CredentialKind::Azure
    } else if snapshot.has_gcp {
        CredentialKind::Gcp
    } else {
        CredentialKind::ApiKey
    };

    if kind != CredentialKind::ApiKey {
        // OAuth / Azure / GCP: no per-key selection here.
        return CredentialDecision {
            kind,
            api_key: None,
            keys_exhausted: false,
        };
    }

    // 3. Plain API-key channel.
    let enabled = snapshot.enabled_api_keys;

    if enabled.is_empty() {
        // No enabled keys remain. Go's TraceStickyKeyProvider.Get falls back
        // to `Credentials.APIKeys[0]` here (for diagnostics); we surface
        // exhaustion so the caller can disable the channel, and only fall
        // back to the legacy single key when one exists.
        return CredentialDecision {
            kind: CredentialKind::ApiKey,
            api_key: if snapshot.legacy_api_key.is_empty() {
                None
            } else {
                Some(snapshot.legacy_api_key)
            },
            keys_exhausted: true,
        };
    }

    if enabled.len() == 1 {
        // Single key: static selection (Go: NewStaticKeyProvider(enabled[0])).
        return CredentialDecision {
            kind: CredentialKind::ApiKey,
            api_key: Some(enabled[0].as_str()),
            keys_exhausted: false,
        };
    }

    // Multi-key: trace-sticky when a trace id is present.
    let selected = match trace.trace_id {
        Some(trace_id) => {
            // LRU cache hit: reuse prior pick if it is still enabled.
            if let Some(cached) = trace.cached_sticky_key {
                if enabled.iter().any(|k| k.as_str() == cached) {
                    cached
                } else {
                    // Cached key no longer enabled (was disabled): re-pick.
                    rendezvous_select(enabled, trace_id).unwrap_or(enabled[0].as_str())
                }
            } else {
                rendezvous_select(enabled, trace_id).unwrap_or(enabled[0].as_str())
            }
        }
        // No trace id: Go uses rand.IntN; we deterministically pick the first
        // key (see fn doc — pure-function constraint).
        None => enabled[0].as_str(),
    };

    CredentialDecision {
        kind: CredentialKind::ApiKey,
        api_key: Some(selected),
        keys_exhausted: false,
    }
}

#[cfg(test)]
mod fingerprint_tests {
    use super::credential_fingerprint;

    #[test]
    fn credential_fingerprint_is_stable_distinct_and_non_secret() {
        let first = credential_fingerprint("sk-secret-a");
        assert_eq!(first, credential_fingerprint("sk-secret-a"));
        assert_ne!(first, credential_fingerprint("sk-secret-b"));
        assert!(first.starts_with("sha256:"));
        assert!(!first.contains("sk-secret-a"));
    }
}
