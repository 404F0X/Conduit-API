//! Authenticated encryption for backup archives that contain provider secrets.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use chacha20poly1305::{Key, KeyInit, XChaCha20Poly1305, XNonce, aead::Aead};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const FORMAT: &str = "conduit_encrypted_backup";
const VERSION: u8 = 1;
const AAD: &[u8] = b"conduit-backup-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackupSections {
    pub include_channels: bool,
    pub include_api_keys: bool,
    pub include_request_logs: bool,
}

impl BackupSections {
    pub fn contains_sensitive_data(self) -> bool {
        self.include_channels || self.include_api_keys || self.include_request_logs
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct EncryptedBackupEnvelope {
    format: String,
    envelope_version: u8,
    algorithm: String,
    key_id: String,
    nonce: String,
    ciphertext: String,
}

fn configured_key() -> Result<Option<[u8; 32]>, String> {
    let Some(raw) = std::env::var_os("CONDUIT_BACKUP_ENCRYPTION_KEY") else {
        return Ok(None);
    };
    let raw = raw.to_string_lossy();
    if raw.trim().is_empty() {
        return Ok(None);
    }
    let decoded = STANDARD
        .decode(raw.trim())
        .map_err(|_| "CONDUIT_BACKUP_ENCRYPTION_KEY must be base64".to_string())?;
    decoded
        .try_into()
        .map(Some)
        .map_err(|_| "CONDUIT_BACKUP_ENCRYPTION_KEY must decode to 32 bytes".to_string())
}

fn key_id(key: &[u8; 32]) -> String {
    let digest = Sha256::digest(key);
    digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn encrypt_if_sensitive(data: &[u8], sections: BackupSections) -> Result<Vec<u8>, String> {
    let Some(key) = configured_key()? else {
        if sections.contains_sensitive_data() {
            return Err(
                "sensitive backup requires CONDUIT_BACKUP_ENCRYPTION_KEY (base64, 32 bytes)"
                    .to_string(),
            );
        }
        return Ok(data.to_vec());
    };
    encrypt_with_key(data, &key)
}

/// Encrypts a backup with an explicitly managed key.
///
/// Production callers normally use [`encrypt_if_sensitive`], which reads the
/// deployment key and fails closed for sensitive sections. This entry point is
/// for callers that already own a validated 32-byte key (including isolated
/// integration-test fixtures).
pub fn encrypt_with_key(data: &[u8], key: &[u8; 32]) -> Result<Vec<u8>, String> {
    let cipher = XChaCha20Poly1305::new(Key::from_slice(key));
    let mut nonce = [0_u8; 24];
    rand::thread_rng().fill_bytes(&mut nonce);
    let ciphertext = cipher
        .encrypt(
            XNonce::from_slice(&nonce),
            chacha20poly1305::aead::Payload {
                msg: data,
                aad: AAD,
            },
        )
        .map_err(|_| "backup encryption failed".to_string())?;
    serde_json::to_vec(&EncryptedBackupEnvelope {
        format: FORMAT.to_string(),
        envelope_version: VERSION,
        algorithm: "xchacha20poly1305".to_string(),
        key_id: key_id(key),
        nonce: STANDARD.encode(nonce),
        ciphertext: STANDARD.encode(ciphertext),
    })
    .map_err(|error| error.to_string())
}

pub fn decrypt_if_enveloped(data: &[u8]) -> Result<Vec<u8>, String> {
    let Some(envelope) = parse_envelope(data)? else {
        return Ok(data.to_vec());
    };
    let Some(key) = configured_key()? else {
        return Err("encrypted backup requires CONDUIT_BACKUP_ENCRYPTION_KEY".to_string());
    };
    if envelope.key_id != key_id(&key) {
        return Err("backup encryption key ID does not match".to_string());
    }
    decrypt_with_key_parts(&envelope, &key)
}

/// Decrypts an encrypted backup with an explicitly managed key while still
/// accepting an unencrypted, non-sensitive archive.
pub fn decrypt_if_enveloped_with_key(data: &[u8], key: &[u8; 32]) -> Result<Vec<u8>, String> {
    let Some(envelope) = parse_envelope(data)? else {
        return Ok(data.to_vec());
    };
    decrypt_with_key_parts(&envelope, key)
}

fn parse_envelope(data: &[u8]) -> Result<Option<EncryptedBackupEnvelope>, String> {
    let Ok(envelope) = serde_json::from_slice::<EncryptedBackupEnvelope>(data) else {
        return Ok(None);
    };
    if envelope.format != FORMAT
        || envelope.envelope_version != VERSION
        || envelope.algorithm != "xchacha20poly1305"
    {
        return Err("unsupported backup encryption envelope".to_string());
    }
    Ok(Some(envelope))
}

fn decrypt_with_key_parts(
    envelope: &EncryptedBackupEnvelope,
    key: &[u8; 32],
) -> Result<Vec<u8>, String> {
    if envelope.key_id != key_id(key) {
        return Err("backup encryption key ID does not match".to_string());
    }
    let nonce = STANDARD
        .decode(&envelope.nonce)
        .map_err(|_| "invalid backup nonce".to_string())?;
    let nonce: [u8; 24] = nonce
        .try_into()
        .map_err(|_| "invalid backup nonce length".to_string())?;
    let ciphertext = STANDARD
        .decode(&envelope.ciphertext)
        .map_err(|_| "invalid backup ciphertext".to_string())?;
    XChaCha20Poly1305::new(Key::from_slice(key))
        .decrypt(
            XNonce::from_slice(&nonce),
            chacha20poly1305::aead::Payload {
                msg: &ciphertext,
                aad: AAD,
            },
        )
        .map_err(|_| "backup authentication failed".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypted_backup_round_trips_and_changes_nonce() -> Result<(), Box<dyn std::error::Error>> {
        let sections = BackupSections {
            include_channels: true,
            include_api_keys: false,
            include_request_logs: false,
        };
        let key = [7_u8; 32];
        assert!(sections.contains_sensitive_data());
        let first = encrypt_with_key(b"provider-secret", &key)?;
        let second = encrypt_with_key(b"provider-secret", &key)?;
        assert_ne!(first, second);
        assert!(!String::from_utf8_lossy(&first).contains("provider-secret"));
        let envelope: EncryptedBackupEnvelope = serde_json::from_slice(&first)?;
        assert_eq!(decrypt_with_key_parts(&envelope, &key)?, b"provider-secret");
        Ok(())
    }

    #[test]
    fn sensitive_sections_are_detected() {
        let sections = BackupSections {
            include_channels: true,
            include_api_keys: false,
            include_request_logs: false,
        };
        assert!(sections.contains_sensitive_data());
    }

    #[test]
    fn wrong_key_and_tampering_are_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let key = [11_u8; 32];
        let bytes = encrypt_with_key(b"provider-secret", &key)?;
        let mut envelope: EncryptedBackupEnvelope = serde_json::from_slice(&bytes)?;
        assert!(decrypt_with_key_parts(&envelope, &[12_u8; 32]).is_err());

        let mut ciphertext = STANDARD.decode(&envelope.ciphertext)?;
        ciphertext[0] ^= 1;
        envelope.ciphertext = STANDARD.encode(ciphertext);
        assert!(decrypt_with_key_parts(&envelope, &key).is_err());
        Ok(())
    }
}
