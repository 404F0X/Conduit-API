use rand::{RngCore, rngs::OsRng};
use thiserror::Error;

use crate::http_auth::NO_AUTH_SENTINEL;

pub const API_KEY_RANDOM_BYTES: usize = 32;
pub const API_KEY_HEX_CHARS: usize = API_KEY_RANDOM_BYTES * 2;

#[derive(Debug, Error)]
pub enum ApiKeyError {
    #[error("api key prefix must not be empty")]
    EmptyPrefix,
    #[error("api key must not use the no-auth sentinel literal")]
    NoAuthSentinel,
}

pub fn generate_api_key(prefix: &str) -> Result<String, ApiKeyError> {
    // Go parity (`biz/api_key.go:169`): `strings.TrimSpace(prefix) == ""`
    // — a whitespace-only prefix (e.g. "   ") is rejected just like an empty
    // one. `prefix.is_empty()` alone would wrongly accept whitespace.
    if prefix.trim().is_empty() {
        return Err(ApiKeyError::EmptyPrefix);
    }
    reject_no_auth_sentinel(prefix)?;

    let mut bytes = [0_u8; API_KEY_RANDOM_BYTES];
    OsRng.fill_bytes(&mut bytes);
    let api_key = format!("{prefix}-{}", encode_hex(&bytes));
    reject_no_auth_sentinel(&api_key)?;
    Ok(api_key)
}

pub fn reject_no_auth_sentinel(api_key: &str) -> Result<(), ApiKeyError> {
    if api_key == NO_AUTH_SENTINEL {
        Err(ApiKeyError::NoAuthSentinel)
    } else {
        Ok(())
    }
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// Mirrors Go `biz/auth.go:88` `GenerateSecretKey()`: 32 random bytes
/// (256 bits) hex-encoded into a 64-char string, used as the JWT signing
/// secret at system initialization. `OsRng` is infallible, so unlike Go's
/// `(string, error)` this returns `String` directly — the only Go failure
/// mode (`rand.Read` error) cannot occur with the platform CSPRNG.
pub fn generate_secret_key() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    encode_hex(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_prefixed_32_byte_hex_key() -> Result<(), ApiKeyError> {
        let key = generate_api_key("ak")?;
        let (prefix, secret) = key.split_once('-').ok_or(ApiKeyError::NoAuthSentinel)?;

        assert_eq!(prefix, "ak");
        assert_eq!(secret.len(), API_KEY_HEX_CHARS);
        assert!(secret.chars().all(|ch| ch.is_ascii_hexdigit()));
        Ok(())
    }

    #[test]
    fn rejects_no_auth_sentinel_literal() {
        assert!(matches!(
            reject_no_auth_sentinel(NO_AUTH_SENTINEL),
            Err(ApiKeyError::NoAuthSentinel)
        ));
        assert!(matches!(
            generate_api_key(NO_AUTH_SENTINEL),
            Err(ApiKeyError::NoAuthSentinel)
        ));
    }

    /// Go parity pin (`biz/api_key.go:169` `strings.TrimSpace(prefix) == ""`):
    /// a whitespace-only prefix must be rejected just like an empty one —
    /// `prefix.is_empty()` alone would wrongly accept it.
    #[test]
    fn rejects_whitespace_only_prefix_like_go_trimspace() {
        assert!(matches!(
            generate_api_key(""),
            Err(ApiKeyError::EmptyPrefix)
        ));
        assert!(matches!(
            generate_api_key("   "),
            Err(ApiKeyError::EmptyPrefix)
        ));
        assert!(matches!(
            generate_api_key("\t\n"),
            Err(ApiKeyError::EmptyPrefix)
        ));
    }

    /// Go parity pin (`biz/auth.go:88` `GenerateSecretKey`, test `auth_test.go:58`):
    /// 32 random bytes → 64 hex chars, non-empty, and two calls differ
    /// (CSPRNG). Pins the JWT-secret contract so byte length / encoding
    /// cannot drift silently.
    #[test]
    fn generate_secret_key_matches_go_64_hex_chars_and_is_unique() {
        let key = generate_secret_key();
        assert_eq!(key.len(), 64, "32 bytes hex-encoded = 64 chars");
        assert!(key.chars().all(|ch| ch.is_ascii_hexdigit()));
        assert_ne!(key, generate_secret_key());
    }
}
