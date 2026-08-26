use thiserror::Error;

/// Go `bcrypt.DefaultCost` is **10**; match it so Rust-generated hashes are
/// byte-compatible with Go (`auth.go::HashPassword`). Validation is
/// cost-agnostic (bcrypt reads cost from the hash), but generation must
/// not emit cost-12 hashes Go would never produce.
pub const DEFAULT_BCRYPT_COST: u32 = 10;

#[derive(Debug, Error)]
pub enum PasswordError {
    #[error("bcrypt failed: {0}")]
    Bcrypt(#[from] bcrypt::BcryptError),
    #[error("stored bcrypt hex has an odd number of characters")]
    InvalidHexLength,
    #[error("stored bcrypt hex contains non-hex byte at index {index}: 0x{byte:02x}")]
    InvalidHex { index: usize, byte: u8 },
    #[error("decoded bcrypt hash is not utf-8: {0}")]
    InvalidUtf8(#[from] std::string::FromUtf8Error),
}

pub fn encode_password_bcrypt_hex(password: &str, cost: u32) -> Result<String, PasswordError> {
    let hash = bcrypt::hash(password, cost)?;
    Ok(encode_hex(hash.as_bytes()))
}

pub fn verify_password_bcrypt_hex(
    password: &str,
    stored_bcrypt_hex: &str,
) -> Result<bool, PasswordError> {
    let hash = String::from_utf8(decode_hex(stored_bcrypt_hex)?)?;
    Ok(bcrypt::verify(password, &hash)?)
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

fn decode_hex(value: &str) -> Result<Vec<u8>, PasswordError> {
    let bytes = value.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return Err(PasswordError::InvalidHexLength);
    }

    let mut out = Vec::with_capacity(bytes.len() / 2);
    for (pair_index, pair) in bytes.chunks_exact(2).enumerate() {
        let high = decode_nibble(pair[0], pair_index * 2)?;
        let low = decode_nibble(pair[1], pair_index * 2 + 1)?;
        out.push((high << 4) | low);
    }
    Ok(out)
}

fn decode_nibble(byte: u8, index: usize) -> Result<u8, PasswordError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(PasswordError::InvalidHex { index, byte }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bcrypt_hex_round_trips_with_local_fixture() -> Result<(), PasswordError> {
        let stored = encode_password_bcrypt_hex("correct horse battery staple", 4)?;

        assert!(stored.chars().all(|ch| ch.is_ascii_hexdigit()));
        assert!(verify_password_bcrypt_hex(
            "correct horse battery staple",
            &stored
        )?);
        assert!(!verify_password_bcrypt_hex("wrong password", &stored)?);
        Ok(())
    }

    #[test]
    fn rejects_invalid_hex_storage() {
        assert!(matches!(
            verify_password_bcrypt_hex("password", "abc"),
            Err(PasswordError::InvalidHexLength)
        ));
        assert!(matches!(
            verify_password_bcrypt_hex("password", "zz"),
            Err(PasswordError::InvalidHex { .. })
        ));
    }

    /// Go parity pin (`biz/auth_test.go:24` `TestHashPassword`): the hash is
    /// non-empty, differs from the plaintext, and two hashes of the same
    /// password differ (bcrypt salt). Cost 4 keeps the test fast.
    #[test]
    fn hash_password_matches_go_semantics() -> Result<(), PasswordError> {
        let password = "test-password-123";
        let hashed = encode_password_bcrypt_hex(password, 4)?;
        assert!(!hashed.is_empty());
        assert_ne!(hashed, password);
        let hashed2 = encode_password_bcrypt_hex(password, 4)?;
        assert_ne!(hashed, hashed2, "bcrypt salt must make hashes unique");
        Ok(())
    }

    /// Go parity pin (`biz/auth_test.go:38` `TestVerifyPassword`): correct
    /// password verifies, wrong password fails, and a malformed stored hash
    /// (Go's literal `"invalid-hash"`) errors instead of panicking.
    #[test]
    fn verify_password_matches_go_semantics() -> Result<(), PasswordError> {
        let password = "test-password-123";
        let hashed = encode_password_bcrypt_hex(password, 4)?;
        assert!(verify_password_bcrypt_hex(password, &hashed)?);
        assert!(!verify_password_bcrypt_hex("wrong-password", &hashed)?);
        // Go's `VerifyPassword("invalid-hash", password)` → error. The hyphen
        // is non-hex so Rust surfaces `InvalidHex` (same error outcome).
        assert!(matches!(
            verify_password_bcrypt_hex(password, "invalid-hash"),
            Err(PasswordError::InvalidHex { .. })
        ));
        Ok(())
    }
}
