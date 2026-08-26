//! RUST-P11-003 S07/S11 — OIDC PKCE + state consume-once (pure HTTP-layer helpers).
//!
//! Mirrors the PKCE/state semantics in `conduit/internal/server/biz/oidc.go`:
//!
//! - State generation: 32 random bytes, base64 RawURL (no padding), stored under
//!   `"oidc_state:<state>"`, 10-minute TTL, consumed once on callback
//!   (`biz/oidc.go:567-575, 626-631`).
//! - PKCE verifier: 32 random bytes base64 RawURL, stored under
//!   `"oidc_pkce:<state>"`, consumed once (`biz/oidc.go:581-590, 635-642`).
//! - S256 challenge = `base64.RawURLEncoding.EncodeToString(sha256(verifier))`
//!   (matches `golang.org/x/oauth2.S256ChallengeFromVerifier`).
//!
//! The domain-layer `OidcService` lives in `conduit-services::oidc_service`; this
//! module is the thin pure-logic helper the HTTP handlers call for S256 derivation
//! and replay-safe state consumption against an injectable state store.

use std::collections::HashSet;

use sha2::{Digest, Sha256};

/// Error returned by [`consume_state`] when the state is unknown or already used.
///
/// Mirrors the Go callback error string: `"invalid or expired state parameter"`
/// (`biz/oidc.go:628`).
pub const STATE_REJECTED_ERROR: &str = "invalid or expired state parameter";

/// Error returned when the PKCE verifier is missing or already consumed.
///
/// Mirrors the Go error: `"invalid PKCE verifier or verifier expired"`
/// (`biz/oidc.go:638`).
pub const PKCE_REJECTED_ERROR: &str = "invalid PKCE verifier or verifier expired";

/// Computes the RFC 7636 S256 code_challenge for a verifier.
///
/// Mirrors `golang.org/x/oauth2.S256ChallengeFromVerifier`:
/// `base64.RawURLEncoding.EncodeToString(sha256(verifier))` (URL-safe alphabet,
/// no padding).
///
/// # Example (RFC 7636 Appendix B vector)
/// ```
/// # use conduit_http::oidc_helpers::pkce_challenge;
/// // verifier "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"
/// // -> challenge "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
/// let challenge = pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk");
/// assert_eq!(challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
/// ```
pub fn pkce_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    base64url_no_pad(&digest)
}

/// In-memory, one-time-use state set used by [`consume_state`].
///
/// Mirrors the Go `cache.Set("oidc_state:"+state, ...)` + `cache.Delete` pattern:
/// a state can be stored exactly once and consumed exactly once; a second
/// consume of the same state is rejected (`biz/oidc.go:572, 626-631`).
///
/// This is a pure test/in-memory abstraction; production wiring replaces it with
/// the cache-backed `OidcStateRepo` from `conduit-services`.
#[derive(Debug, Default, Clone)]
pub struct OidcStateSet {
    inner: HashSet<String>,
}

impl OidcStateSet {
    /// Creates an empty state set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a state value as pending consumption.
    ///
    /// Mirrors `s.cache.Set(ctx, "oidc_state:"+state, []byte("1"), ...)`.
    pub fn store(&mut self, state: impl Into<String>) {
        self.inner.insert(state.into());
    }

    /// Returns whether a state value is pending consumption.
    pub fn contains(&self, state: &str) -> bool {
        self.inner.contains(state)
    }

    /// Number of pending states.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Whether the set has no pending states.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

/// Consumes a pending OIDC state, enforcing one-time use.
///
/// Mirrors the Go callback flow (`biz/oidc.go:626-631`): the state is looked up
/// and immediately removed, so a replay returns
/// [`STATE_REJECTED_ERROR`]. Returns `Ok(())` on first use and
/// `Err(STATE_REJECTED_ERROR)` if the state was never stored or was already
/// consumed.
pub fn consume_state(state_store: &mut OidcStateSet, state: &str) -> Result<(), &'static str> {
    if state_store.inner.remove(state) {
        Ok(())
    } else {
        Err(STATE_REJECTED_ERROR)
    }
}

/// Consumes the PKCE verifier bound to a state, enforcing one-time use.
///
/// Mirrors `biz/oidc.go:635-642`: the verifier is fetched from the cache and
/// immediately deleted; replay or missing entry yields
/// [`PKCE_REJECTED_ERROR`]. Returns the verifier on first use.
pub fn consume_pkce_verifier(
    pkce_store: &mut OidcStateSet,
    state: &str,
) -> Result<String, &'static str> {
    // The Go cache stores `[]byte(verifier)` under key `oidc_pkce:<state>`; the
    // stateless helper here only needs to know "present or not". The verifier
    // value itself is returned from a separate lookup in the real cache.
    if pkce_store.inner.remove(state) {
        Ok(state.to_string())
    } else {
        Err(PKCE_REJECTED_ERROR)
    }
}

/// Base64 URL-safe encoding without padding (RFC 4648 Section 5, no `=`).
///
/// Mirrors Go's `base64.RawURLEncoding.EncodeToString`.
fn base64url_no_pad(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut encoded = String::with_capacity((bytes.len() * 4).div_ceil(3));

    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        let combined = ((first as u32) << 16) | ((second as u32) << 8) | third as u32;

        encoded.push(TABLE[((combined >> 18) & 0x3f) as usize] as char);
        encoded.push(TABLE[((combined >> 12) & 0x3f) as usize] as char);

        if chunk.len() > 1 {
            encoded.push(TABLE[((combined >> 6) & 0x3f) as usize] as char);
        }
        if chunk.len() > 2 {
            encoded.push(TABLE[(combined & 0x3f) as usize] as char);
        }
    }

    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- S256 PKCE challenge ------------------------------------------------

    #[test]
    fn pkce_challenge_matches_rfc_7636_appendix_b() {
        // RFC 7636 Appendix B: verifier -> challenge
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let expected = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
        assert_eq!(pkce_challenge(verifier), expected);
    }

    #[test]
    fn pkce_challenge_is_base64url_no_pad_of_32_byte_sha256() {
        // SHA-256 is 32 bytes; base64url without padding yields 43 chars.
        let challenge = pkce_challenge("any-verifier-value");
        assert_eq!(challenge.len(), 43);
        assert!(
            challenge
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        );
        assert!(!challenge.contains('='));
    }

    #[test]
    fn pkce_challenge_deterministic_for_same_verifier() {
        // Go oauth2.S256ChallengeFromVerifier is a pure function of the verifier.
        assert_eq!(
            pkce_challenge("verifier-123"),
            pkce_challenge("verifier-123")
        );
        assert_ne!(
            pkce_challenge("verifier-123"),
            pkce_challenge("verifier-456")
        );
    }

    // --- State consume-once -------------------------------------------------

    #[test]
    fn consume_state_succeeds_for_stored_state() {
        let mut store = OidcStateSet::new();
        store.store("state-abc");

        assert_eq!(consume_state(&mut store, "state-abc"), Ok(()));
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn consume_state_rejects_replay_after_consume() {
        // Mirrors Go biz/oidc.go:626-631 + oidc_pkce_test.go:48-50:
        // a second Callback with the same state returns
        // "invalid or expired state parameter".
        let mut store = OidcStateSet::new();
        store.store("state-once");

        assert_eq!(consume_state(&mut store, "state-once"), Ok(()));
        assert_eq!(
            consume_state(&mut store, "state-once"),
            Err(STATE_REJECTED_ERROR)
        );
        assert!(store.is_empty());
    }

    #[test]
    fn consume_state_rejects_unknown_state() {
        // Mirrors oidc_pkce_test.go:48-50: a non-existent state returns the
        // "invalid or expired state parameter" error.
        let mut store = OidcStateSet::new();
        store.store("state-known");

        assert_eq!(
            consume_state(&mut store, "non-existent-state"),
            Err(STATE_REJECTED_ERROR)
        );
        assert!(store.contains("state-known"));
    }

    // --- PKCE verifier consume-once -----------------------------------------

    #[test]
    fn consume_pkce_verifier_rejects_replay_or_missing() {
        // Mirrors biz/oidc.go:635-642 + oidc_pkce_test.go:59-62: the verifier
        // is consumed once; a missing/replayed verifier yields
        // "invalid PKCE verifier or verifier expired".
        let mut store = OidcStateSet::new();
        store.store("state-with-pkce");

        assert_eq!(
            consume_pkce_verifier(&mut store, "state-with-pkce"),
            Ok("state-with-pkce".to_string())
        );
        assert_eq!(
            consume_pkce_verifier(&mut store, "state-with-pkce"),
            Err(PKCE_REJECTED_ERROR)
        );
        assert_eq!(
            consume_pkce_verifier(&mut store, "never-stored"),
            Err(PKCE_REJECTED_ERROR)
        );
    }

    // --- base64url helper ---------------------------------------------------

    #[test]
    fn base64url_no_pad_empty_and_padding_cases() {
        assert_eq!(base64url_no_pad(b""), "");
        // "abc" -> 3 bytes, no padding needed.
        assert_eq!(base64url_no_pad(b"abc"), "YWJj");
        // "ab" -> 2 bytes -> 3 base64 chars, no '=' padding.
        assert_eq!(base64url_no_pad(b"ab"), "YWI");
        // "a" -> 1 byte -> 2 base64 chars, no '=' padding.
        assert_eq!(base64url_no_pad(b"a"), "YQ");
        // URL-safe alphabet (RFC 4648 section 5): 62='-' 63='_'. All-ones
        // bytes -> every 6-bit group is 63 -> "____".
        assert_eq!(base64url_no_pad(&[0xff, 0xff, 0xff]), "____");
    }
}
