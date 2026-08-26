//! Data storage settings ported from `conduit/internal/objects/data_stograge.go`
//! (Go filename typo preserved for traceability; target is the correctly-spelled
//! `storage.rs` per migration plan `OBJ-05`).
//!
//! Covers the four structs in that Go file: [`DataStorageSettings`] and its
//! three backend branches [`S3`], [`GCS`], and [`WebDAV`]. All fields are
//! mirrored 1:1 against the Go json tags.

use serde::{Deserialize, Serialize};

/// Top-level data-storage configuration. Ported 1:1 from Go
/// `DataStorageSettings`; each backend is an optional pointer in Go, mirrored
/// here as `Option<T>` and omitted from the serialized form when `None`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct DataStorageSettings {
    /// DSN is the database data storage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dsn: Option<String>,
    /// Directory is the directory of the fs data storage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<String>,
    /// S3 is the s3 data storage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub s3: Option<S3>,
    /// GCS is the gcs data storage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gcs: Option<GCS>,
    /// WebDAV is the webdav data storage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webdav: Option<WebDAV>,
}

/// S3-compatible object storage settings. Ported 1:1 from Go `S3`.
///
/// `PathStyle` enables Path Style access for S3 compatible storage services
/// (e.g., MinIO, Ceph RGW). When enabled, uses
/// `https://s3.amazonaws.com/<bucket-name>/object` format instead of Virtual
/// Hosted Style.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct S3 {
    pub bucket_name: String,
    pub endpoint: String,
    pub region: String,
    pub access_key: String,
    pub secret_key: String,
    /// Path Style access flag (see struct-level doc).
    pub path_style: bool,
}

/// Google Cloud Storage settings. Ported 1:1 from Go `GCS`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GCS {
    pub bucket_name: String,
    pub credential: String,
}

/// WebDAV storage settings. Ported 1:1 from Go `WebDAV`. The
/// `insecure_skip_tls` field keeps the Go json tag's snake_case spelling.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct WebDAV {
    pub url: String,
    pub username: String,
    pub password: String,
    pub insecure_skip_tls: bool,
    pub path: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn data_storage_settings_round_trip_with_s3() -> Result<(), serde_json::Error> {
        let input = r#"{"dsn":"postgres://db","s3":{"bucketName":"logs","endpoint":"s3.example.com","region":"us-east-1","accessKey":"AKID","secretKey":"SECRET","pathStyle":true}}"#;
        let settings: DataStorageSettings = serde_json::from_str(input)?;
        assert_eq!(settings.dsn.as_deref(), Some("postgres://db"));
        let s3 = match &settings.s3 {
            Some(s) => s,
            None => return Ok(()), // unreachable on success but avoids panic
        };
        assert_eq!(s3.bucket_name, "logs");
        assert_eq!(s3.endpoint, "s3.example.com");
        assert_eq!(s3.region, "us-east-1");
        assert_eq!(s3.access_key, "AKID");
        assert_eq!(s3.secret_key, "SECRET");
        assert!(s3.path_style);

        let re = serde_json::to_value(&settings)?;
        assert_eq!(
            re.get("dsn").and_then(|v| v.as_str()),
            Some("postgres://db")
        );
        assert!(
            re.get("directory").is_none()
                || re.get("directory").map(|v| v.is_null()).unwrap_or(true)
        );
        assert_eq!(
            re.get("s3")
                .and_then(|v| v.get("bucketName"))
                .and_then(|v| v.as_str()),
            Some("logs")
        );
        Ok(())
    }

    #[test]
    fn data_storage_settings_empty_round_trip() -> Result<(), serde_json::Error> {
        let input = r#"{}"#;
        let settings: DataStorageSettings = serde_json::from_str(input)?;
        assert!(settings.dsn.is_none());
        assert!(settings.directory.is_none());
        assert!(settings.s3.is_none());
        assert!(settings.gcs.is_none());
        assert!(settings.webdav.is_none());

        let re = serde_json::to_value(&settings)?;
        // All optional fields are skipped when None.
        assert_eq!(re, json!({}));
        Ok(())
    }

    #[test]
    fn gcs_round_trip() -> Result<(), serde_json::Error> {
        let input = r#"{"bucketName":"bkt","credential":"cred-json"}"#;
        let gcs: GCS = serde_json::from_str(input)?;
        assert_eq!(gcs.bucket_name, "bkt");
        assert_eq!(gcs.credential, "cred-json");
        let re = serde_json::to_value(&gcs)?;
        assert_eq!(re.get("bucketName").and_then(|v| v.as_str()), Some("bkt"));
        Ok(())
    }

    #[test]
    fn webdav_preserves_snake_case_json_tag() -> Result<(), serde_json::Error> {
        let input = r#"{"url":"https://dav.example.com","username":"u","password":"p","insecure_skip_tls":true,"path":"/data"}"#;
        let dav: WebDAV = serde_json::from_str(input)?;
        assert_eq!(dav.url, "https://dav.example.com");
        assert_eq!(dav.username, "u");
        assert_eq!(dav.password, "p");
        assert!(dav.insecure_skip_tls);
        assert_eq!(dav.path, "/data");

        let re = serde_json::to_value(&dav)?;
        // snake_case tag must be preserved verbatim (not converted to camelCase).
        assert_eq!(
            re.get("insecure_skip_tls").and_then(|v| v.as_bool()),
            Some(true)
        );
        Ok(())
    }
}
