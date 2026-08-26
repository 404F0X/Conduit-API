//! Typed view of the `DataStorage.settings` JSON blob and the partial-update
//! merge rules that back `DataStorageService.UpdateDataStorage`.
//!
//! **Contract source** (`conduit/internal/objects/data_stograge.go`):
//!
//! ```go
//! type DataStorageSettings struct {
//!     DSN      *string  `json:"dsn"`
//!     Directory *string `json:"directory"`
//!     S3       *S3      `json:"s3"`
//!     GCS      *GCS     `json:"gcs"`
//!     WebDAV   *WebDAV  `json:"webdav"`
//! }
//! type S3 struct {
//!     BucketName string `json:"bucketName"`
//!     Endpoint   string `json:"endpoint"`
//!     Region     string `json:"region"`
//!     AccessKey  string `json:"accessKey"`
//!     SecretKey  string `json:"secretKey"`
//!     PathStyle  bool   `json:"pathStyle"`
//! }
//! type GCS struct {
//!     BucketName string `json:"bucketName"`
//!     Credential string `json:"credential"`
//! }
//! type WebDAV struct {
//!     URL             string `json:"url"`
//!     Username        string `json:"username"`
//!     Password        string `json:"password"`
//!     InsecureSkipTLS bool   `json:"insecure_skip_tls"` // snake_case!
//!     Path            string `json:"path"`
//! }
//! ```
//!
//! This module covers RUST-P13-001 S14: typed parsing of the S3/GCS/WebDAV
//! credential JSON plus the merge semantics from Go's
//! `DataStorageService.mergeSettings` (in
//! `internal/server/biz/data_storage.go`), which preserves existing
//! credentials when an update payload omits them. The merge is a pure
//! function over plain values (no I/O, no SDK), so it can be unit-tested
//! directly against the Go golden cases in `data_storage_test.go`.

use serde::{Deserialize, Serialize};

/// S3 backend settings. Mirrors `objects.S3`.
///
/// All json tags are camelCase; `rename_all = "camelCase"` on the struct
/// produces the correct tags for every field here. Every field is
/// `#[serde(default)]` so partial JSON (e.g. `{"bucketName":"b"}`) parses the
/// same way Go's zero-fill of non-pointer struct fields does.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct S3Settings {
    pub bucket_name: String,
    pub endpoint: String,
    pub region: String,
    pub access_key: String,
    pub secret_key: String,
    pub path_style: bool,
}

/// GCS backend settings. Mirrors `objects.GCS`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct GcsSettings {
    pub bucket_name: String,
    pub credential: String,
}

/// WebDAV backend settings. Mirrors `objects.WebDAV`.
///
/// **Gotcha**: the Go tag for `InsecureSkipTLS` is the snake_case
/// `insecure_skip_tls` (every other field is camelCase). We add an explicit
/// `#[serde(rename = ...)]` so round-tripping through JSON matches the Go
/// contract byte-for-byte. CLAUDE.md's acronym-rename gotcha does not apply
/// here (no all-caps acronym), but the mixed casing in the same struct is
/// the same class of bug.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct WebDavSettings {
    pub url: String,
    pub username: String,
    pub password: String,
    #[serde(rename = "insecure_skip_tls")]
    pub insecure_skip_tls: bool,
    pub path: String,
}

/// Typed view of the `DataStorage.settings` JSON blob (S14). All field names
/// are lower-case single words in the Go json tags, so no rename is needed
/// beyond `rename_all = "camelCase"` collapsing `bucket_name` etc. inside the
/// nested backends.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DataStorageConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dsn: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub s3: Option<S3Settings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gcs: Option<GcsSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webdav: Option<WebDavSettings>,
}

impl DataStorageConfig {
    /// Parse a settings blob from a [`serde_json::Value`] (the form stored on
    /// the `DataStorage` row). Unknown fields are ignored so forward-compatible
    /// additions in Go do not break parsing.
    pub fn from_value(value: &serde_json::Value) -> Result<Self, serde_json::Error> {
        serde_json::from_value(value.clone())
    }

    /// Serialize back to a JSON object suitable for storage on the row.
    pub fn to_value(&self) -> serde_json::Result<serde_json::Value> {
        serde_json::to_value(self)
    }
}

/// Mirrors Go `isS3Provided` (`internal/server/biz/data_storage.go`): any
/// non-empty S3 field counts as "the caller is supplying S3 config", which
/// triggers the per-field merge. Returns `false` for `None`.
pub fn is_s3_provided(s3: Option<&S3Settings>) -> bool {
    match s3 {
        Some(s) => {
            !s.bucket_name.is_empty()
                || !s.endpoint.is_empty()
                || !s.region.is_empty()
                || !s.access_key.is_empty()
                || !s.secret_key.is_empty()
        }
        None => false,
    }
}

/// Mirrors Go `isGCSProvided`.
pub fn is_gcs_provided(gcs: Option<&GcsSettings>) -> bool {
    match gcs {
        Some(s) => !s.bucket_name.is_empty() || !s.credential.is_empty(),
        None => false,
    }
}

/// Mirrors Go `isWebDAVProvided`. Note: `InsecureSkipTLS` is intentionally
/// NOT in the "provided" predicate — Go's version only checks the four
/// string fields. A payload that toggles only the bool would be treated as
/// "not provided" by Go, so we match that exactly to preserve merge parity.
pub fn is_webdav_provided(webdav: Option<&WebDavSettings>) -> bool {
    match webdav {
        Some(s) => {
            !s.url.is_empty()
                || !s.username.is_empty()
                || !s.password.is_empty()
                || !s.path.is_empty()
        }
        None => false,
    }
}

/// Merge an `existing` settings blob with an `input` partial update, mirroring
/// Go's `DataStorageService.mergeSettings`
/// (`internal/server/biz/data_storage.go`, lines ~707-800).
///
/// Rules (verbatim from Go):
/// 1. `directory` and `dsn` are non-sensitive — input wins if `Some`,
///    otherwise existing is preserved.
/// 2. For each backend (`s3`/`gcs`/`webdav`): if the input backend is
///    "provided" (see `is_*_provided`), the non-sensitive fields are taken
///    from input; **sensitive fields** (`S3.accessKey`, `S3.secretKey`,
///    `GCS.credential`, `WebDAV.password`) are taken from input only when
///    non-empty, otherwise preserved from `existing`. If the input backend
///    is NOT provided, the existing backend is preserved verbatim.
/// 3. `None` input behaves like "no changes" and returns the existing config
///    unchanged (Go: `if input == nil { return existing }`).
///
/// This is the load-bearing invariant for partial updates: an admin who
/// submits only `{"s3": {"bucketName": "new"}}` must not accidentally wipe
/// the stored access/secret keys.
pub fn merge_data_storage_settings(
    existing: Option<&DataStorageConfig>,
    input: Option<&DataStorageConfig>,
) -> DataStorageConfig {
    let Some(input) = input else {
        // Go: `if input == nil { return existing }`. Clone to satisfy ownership.
        return existing.cloned().unwrap_or_default();
    };
    let existing_ref = existing.cloned().unwrap_or_default();

    // 1. directory (non-sensitive)
    let directory = match (&input.directory, &existing_ref.directory) {
        (Some(d), _) => Some(d.clone()),
        (None, prev) => prev.clone(),
    };
    // 2. dsn (sensitive for database, but Go still treats it like directory
    //    in the merge: input wins when Some)
    let dsn = match (&input.dsn, &existing_ref.dsn) {
        (Some(d), _) => Some(d.clone()),
        (None, prev) => prev.clone(),
    };

    // 3. S3
    let s3 = if is_s3_provided(input.s3.as_ref()) {
        let i = input.s3.clone().unwrap_or_default();
        let prev = existing_ref.s3.as_ref();
        Some(S3Settings {
            bucket_name: i.bucket_name,
            endpoint: i.endpoint,
            region: i.region,
            path_style: i.path_style,
            access_key: non_empty_or(i.access_key, prev.map(|p| p.access_key.as_str())),
            secret_key: non_empty_or(i.secret_key, prev.map(|p| p.secret_key.as_str())),
        })
    } else {
        existing_ref.s3.clone()
    };

    // 4. GCS
    let gcs = if is_gcs_provided(input.gcs.as_ref()) {
        let i = input.gcs.clone().unwrap_or_default();
        let prev = existing_ref.gcs.as_ref();
        Some(GcsSettings {
            bucket_name: i.bucket_name,
            credential: non_empty_or(i.credential, prev.map(|p| p.credential.as_str())),
        })
    } else {
        existing_ref.gcs.clone()
    };

    // 5. WebDAV
    let webdav = if is_webdav_provided(input.webdav.as_ref()) {
        let i = input.webdav.clone().unwrap_or_default();
        let prev = existing_ref.webdav.as_ref();
        Some(WebDavSettings {
            url: i.url,
            username: i.username,
            insecure_skip_tls: i.insecure_skip_tls,
            path: i.path,
            password: non_empty_or(i.password, prev.map(|p| p.password.as_str())),
        })
    } else {
        existing_ref.webdav.clone()
    };

    DataStorageConfig {
        dsn,
        directory,
        s3,
        gcs,
        webdav,
    }
}

/// Return `candidate` if it is non-empty, otherwise `previous` (which may be
/// `None` → empty string). Mirrors the
/// `if input.X != "" { merged.X = input.X } else if existing != nil { merged.X = existing.X }`
/// pattern in Go's `mergeSettings`.
fn non_empty_or(candidate: String, previous: Option<&str>) -> String {
    if !candidate.is_empty() {
        candidate
    } else if let Some(prev) = previous {
        prev.to_string()
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // -------------------------------------------------------------------------
    // serde parity with `objects/data_stograge.go`
    // -------------------------------------------------------------------------

    #[test]
    fn webdav_round_trips_with_snake_case_insecure_skip_tls() -> Result<(), serde_json::Error> {
        // Go json tag for InsecureSkipTLS is `insecure_skip_tls` (snake_case),
        // even though every other WebDAV field is camelCase. The explicit
        // #[serde(rename)] must preserve that.
        let input = json!({
            "url": "https://dav.example.com",
            "username": "alice",
            "password": "letmein",
            "insecure_skip_tls": true,
            "path": "/dav"
        });
        let parsed: WebDavSettings = serde_json::from_value(input.clone())?;
        assert!(parsed.insecure_skip_tls);
        assert_eq!(parsed.url, "https://dav.example.com");

        // Round-trip: serializing must produce the SAME key (not "insecureSkipTls").
        let back = serde_json::to_value(&parsed)?;
        assert_eq!(
            back.get("insecure_skip_tls").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert!(
            back.get("insecureSkipTls").is_none(),
            "must NOT use camelCase here"
        );
        assert_eq!(back, input);
        Ok(())
    }

    #[test]
    fn s3_settings_round_trip_uses_camel_case_tags() -> Result<(), serde_json::Error> {
        let input = json!({
            "bucketName": "logs",
            "endpoint": "s3.example.com",
            "region": "us-east-1",
            "accessKey": "AKIAEXAMPLE",
            "secretKey": "s3kr3t",
            "pathStyle": true
        });
        let parsed: S3Settings = serde_json::from_value(input.clone())?;
        assert_eq!(parsed.bucket_name, "logs");
        assert_eq!(parsed.access_key, "AKIAEXAMPLE");
        assert_eq!(parsed.secret_key, "s3kr3t");
        assert!(parsed.path_style);

        let back = serde_json::to_value(&parsed)?;
        assert_eq!(back, input);
        Ok(())
    }

    #[test]
    fn data_storage_config_round_trip_preserves_optional_backends() -> Result<(), serde_json::Error>
    {
        let input = json!({
            "dsn": "postgres://db",
            "directory": "/var/data",
            "s3": {"bucketName": "b", "region": "r"},
            "gcs": null
        });
        let parsed = DataStorageConfig::from_value(&input)?;
        assert_eq!(parsed.dsn.as_deref(), Some("postgres://db"));
        assert_eq!(parsed.directory.as_deref(), Some("/var/data"));
        assert_eq!(
            parsed.s3.as_ref().map(|s| s.bucket_name.as_str()),
            Some("b")
        );
        assert_eq!(parsed.gcs, None);
        assert_eq!(parsed.webdav, None);

        // Round-trip: every Set field reappears.
        let back = parsed.to_value()?;
        assert_eq!(
            back.get("dsn").and_then(|v| v.as_str()),
            Some("postgres://db")
        );
        assert_eq!(
            back.get("s3")
                .and_then(|v| v.get("bucketName"))
                .and_then(|v| v.as_str()),
            Some("b")
        );
        // `skip_serializing_if = "Option::is_none"` drops the explicit null.
        assert!(back.get("gcs").is_none());
        Ok(())
    }

    // -------------------------------------------------------------------------
    // merge_data_storage_settings — mirrors Go mergeSettings golden cases
    // (data_storage_test.go "preserves existing credentials when not provided"
    //  and "overrides credentials when provided").
    // -------------------------------------------------------------------------

    fn existing_with_creds() -> DataStorageConfig {
        DataStorageConfig {
            directory: Some("/existing/path".to_string()),
            dsn: Some("existing-dsn".to_string()),
            s3: Some(S3Settings {
                bucket_name: "existing-bucket".to_string(),
                endpoint: "existing-endpoint".to_string(),
                region: "existing-region".to_string(),
                access_key: "existing-access".to_string(),
                secret_key: "existing-secret".to_string(),
                path_style: false,
            }),
            gcs: Some(GcsSettings {
                bucket_name: "existing-gcs-bucket".to_string(),
                credential: "existing-gcs-cred".to_string(),
            }),
            webdav: None,
        }
    }

    #[test]
    fn merge_preserves_existing_credentials_when_input_omits_them() {
        // Mirrors the Go test "preserves existing credentials when not provided":
        // input provides S3 + GCS non-sensitive fields but leaves credential
        // strings empty. Existing creds must survive.
        let existing = existing_with_creds();
        let input = DataStorageConfig {
            s3: Some(S3Settings {
                bucket_name: "updated-bucket".to_string(),
                endpoint: String::new(),
                region: "updated-region".to_string(),
                access_key: String::new(),
                secret_key: String::new(),
                path_style: false,
            }),
            gcs: Some(GcsSettings {
                bucket_name: "updated-gcs-bucket".to_string(),
                credential: String::new(),
            }),
            ..DataStorageConfig::default()
        };

        let merged = merge_data_storage_settings(Some(&existing), Some(&input));

        // Non-sensitive fields taken from input.
        assert_eq!(merged.directory.as_deref(), Some("/existing/path"));
        assert_eq!(merged.dsn.as_deref(), Some("existing-dsn"));
        let s3 = match merged.s3.as_ref() {
            Some(s) => s,
            None => panic!("s3 must be Some after merge"),
        };
        assert_eq!(s3.bucket_name, "updated-bucket");
        assert_eq!(s3.endpoint, ""); // input explicitly empty, no prior endpoint to fall back to inside the same input
        assert_eq!(s3.region, "updated-region");
        // Sensitive credentials preserved from existing.
        assert_eq!(s3.access_key, "existing-access");
        assert_eq!(s3.secret_key, "existing-secret");

        let gcs = match merged.gcs.as_ref() {
            Some(s) => s,
            None => panic!("gcs must be Some after merge"),
        };
        assert_eq!(gcs.bucket_name, "updated-gcs-bucket");
        assert_eq!(gcs.credential, "existing-gcs-cred");
    }

    #[test]
    fn merge_overrides_credentials_when_input_provides_them() {
        // Mirrors the Go test "overrides credentials when provided".
        let existing = existing_with_creds();
        let input = DataStorageConfig {
            directory: Some("/new/path".to_string()),
            dsn: Some("new-dsn".to_string()),
            s3: Some(S3Settings {
                bucket_name: "new-bucket".to_string(),
                endpoint: "new-endpoint".to_string(),
                region: "new-region".to_string(),
                access_key: "new-access".to_string(),
                secret_key: "new-secret".to_string(),
                path_style: false,
            }),
            gcs: Some(GcsSettings {
                bucket_name: "new-gcs-bucket".to_string(),
                credential: "new-gcs-cred".to_string(),
            }),
            ..DataStorageConfig::default()
        };

        let merged = merge_data_storage_settings(Some(&existing), Some(&input));

        assert_eq!(merged.directory.as_deref(), Some("/new/path"));
        assert_eq!(merged.dsn.as_deref(), Some("new-dsn"));
        let default_s3 = S3Settings::default();
        let s3 = merged.s3.as_ref().unwrap_or(&default_s3);
        assert_eq!(s3.bucket_name, "new-bucket");
        assert_eq!(s3.endpoint, "new-endpoint");
        assert_eq!(s3.region, "new-region");
        assert_eq!(s3.access_key, "new-access");
        assert_eq!(s3.secret_key, "new-secret");

        let default_gcs = GcsSettings::default();
        let gcs = merged.gcs.as_ref().unwrap_or(&default_gcs);
        assert_eq!(gcs.bucket_name, "new-gcs-bucket");
        assert_eq!(gcs.credential, "new-gcs-cred");
    }

    #[test]
    fn merge_with_none_input_returns_existing_unchanged() {
        // Go: `if input == nil { return existing }`.
        let existing = existing_with_creds();
        let merged = merge_data_storage_settings(Some(&existing), None);
        assert_eq!(merged, existing);
    }

    #[test]
    fn merge_when_backend_not_provided_preserves_existing_backend_verbatim() {
        // If input has S3=None (or all-empty S3), the existing S3 block must
        // survive untouched, including its credentials.
        let existing = existing_with_creds();
        let input = DataStorageConfig {
            // Touch only the directory; S3/GCS absent.
            directory: Some("/new/dir".to_string()),
            ..DataStorageConfig::default()
        };
        let merged = merge_data_storage_settings(Some(&existing), Some(&input));
        assert_eq!(merged.directory.as_deref(), Some("/new/dir"));
        assert_eq!(merged.s3, existing.s3);
        assert_eq!(merged.gcs, existing.gcs);
    }

    #[test]
    fn merge_with_no_existing_falls_back_to_empty_credentials() {
        // First-time update (existing=None): empty credential strings in input
        // cannot be back-filled, so they stay empty. Non-sensitive fields still
        // take the input values.
        let input = DataStorageConfig {
            s3: Some(S3Settings {
                bucket_name: "fresh-bucket".to_string(),
                access_key: String::new(),
                secret_key: String::new(),
                ..Default::default()
            }),
            ..DataStorageConfig::default()
        };
        let merged = merge_data_storage_settings(None, Some(&input));
        let default_s3 = S3Settings::default();
        let s3 = merged.s3.as_ref().unwrap_or(&default_s3);
        assert_eq!(s3.bucket_name, "fresh-bucket");
        assert_eq!(s3.access_key, "");
        assert_eq!(s3.secret_key, "");
    }

    // -------------------------------------------------------------------------
    // is_*_provided parity
    // -------------------------------------------------------------------------

    #[test]
    fn is_s3_provided_matches_go_predicate() {
        let empty = S3Settings::default();
        assert!(!is_s3_provided(None));
        assert!(!is_s3_provided(Some(&empty)));
        let with_bucket = S3Settings {
            bucket_name: "b".to_string(),
            ..Default::default()
        };
        assert!(is_s3_provided(Some(&with_bucket)));
        // path_style alone is NOT enough (Go only checks the 5 string fields).
        let path_only = S3Settings {
            path_style: true,
            ..Default::default()
        };
        assert!(!is_s3_provided(Some(&path_only)));
    }

    #[test]
    fn is_webdav_provided_ignores_insecure_skip_tls_bool() {
        // Go's isWebDAVProvided checks only url/username/password/path.
        // A payload that toggles only InsecureSkipTLS is "not provided".
        let tls_only = WebDavSettings {
            insecure_skip_tls: true,
            ..Default::default()
        };
        assert!(!is_webdav_provided(Some(&tls_only)));
        let with_url = WebDavSettings {
            url: "https://dav".to_string(),
            ..Default::default()
        };
        assert!(is_webdav_provided(Some(&with_url)));
    }
}
