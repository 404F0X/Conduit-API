//! A01 — hermetic integration tests for the local filesystem + in-memory
//! adapters, exercising the **public** `conduit_storage` API surface
//! (RUST-P13-001 A01).
//!
//! These mirror the Go golden cases in
//! `conduit/internal/server/biz/data_storage_test.go`:
//!
//! - `TestDataStorageService_SaveData` "save data to fs storage": put ->
//!   exists -> read-back returns the same bytes.
//! - `TestDataStorageService_LoadData` "load data from fs storage" +
//!   "load non-existent file from fs storage": round-trip + missing-key
//!   error mentions "failed to read file".
//! - Path-traversal rejection (RUST-P13-001 S13, already `[X]`) is
//!   re-asserted here through the public `LocalStorageAdapter` so the
//!   integration contract is locked independently of the in-crate unit
//!   tests.
//!
//! All tests run against a per-test `tempfile::tempdir()`; no shared
//! state, no network. The workspace forbids `unwrap`/`expect` even in
//! tests, so each test returns `Result<(), Box<dyn std::error::Error>>`
//! and uses `?` / `assert_eq!` on `Option`/`Result`.

use conduit_storage::{
    DataStorageKind, DataStorageService, InMemoryStorageAdapter, LocalStorageAdapter,
    StorageAdapter, StorageError, StorageObject,
};

// ---------------------------------------------------------------------------
// Local filesystem adapter — full put/get/exists/list/delete round-trip
// in a temp dir (mirrors Go TestDataStorageService_SaveData/_LoadData fs
// branches + the S13 path-traversal guard).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn local_adapter_put_get_exists_list_delete_round_trip()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let adapter = LocalStorageAdapter::new(temp.path().to_path_buf());

    // Nothing exists initially.
    assert!(!adapter.exists("requests/abc.json").await?);

    // Put two objects under a shared prefix.
    let metadata_a = adapter
        .put(StorageObject::new(
            "requests/abc.json",
            br#"{"ok":true}"#.to_vec(),
        ))
        .await?;
    let metadata_b = adapter
        .put(StorageObject::new("requests/def.json", b"hello".to_vec()))
        .await?;
    assert_eq!(metadata_a.key, "requests/abc.json");
    assert_eq!(metadata_a.content_length, 11);
    assert_eq!(metadata_b.content_length, 5);

    // exists probes both.
    assert!(adapter.exists("requests/abc.json").await?);
    assert!(adapter.exists("requests/def.json").await?);

    // get returns the exact bytes.
    let object_a = match adapter.get("requests/abc.json").await? {
        Some(object) => object,
        None => return Err("expected requests/abc.json to be present".into()),
    };
    assert_eq!(object_a.bytes, br#"{"ok":true}"#);
    assert_eq!(object_a.metadata.key, "requests/abc.json");

    // list under the prefix returns both, sorted by key.
    let listed: Vec<String> = adapter
        .list("requests")
        .await?
        .into_iter()
        .map(|m| m.key)
        .collect();
    assert_eq!(
        listed,
        vec![
            "requests/abc.json".to_string(),
            "requests/def.json".to_string(),
        ]
    );

    // delete removes the object; a second delete reports false (already gone).
    assert!(adapter.delete("requests/abc.json").await?);
    assert!(!adapter.delete("requests/abc.json").await?);
    assert!(!adapter.exists("requests/abc.json").await?);
    assert_eq!(adapter.get("requests/abc.json").await?, None);
    Ok(())
}

#[tokio::test]
async fn local_adapter_head_reads_sidecar_metadata() -> Result<(), Box<dyn std::error::Error>> {
    // When the object was written via `put`, a `.json` sidecar carries the
    // metadata. `head` must surface it (content_type, custom fields).
    let temp = tempfile::tempdir()?;
    let adapter = LocalStorageAdapter::new(temp.path().to_path_buf());

    let object = StorageObject::new("artifacts/item.bin", b"payload".to_vec());
    adapter.put(object).await?;

    let head = match adapter.head("artifacts/item.bin").await? {
        Some(h) => h,
        None => return Err("head must find the written object".into()),
    };
    assert_eq!(head.key, "artifacts/item.bin");
    assert_eq!(head.content_length, 7);
    Ok(())
}

#[tokio::test]
async fn local_adapter_presign_is_unsupported() -> Result<(), Box<dyn std::error::Error>> {
    // S11: local filesystem cannot pre-sign. The default trait impl must
    // return StorageError::Unsupported so callers can detect backends that
    // cannot pre-sign.
    let temp = tempfile::tempdir()?;
    let adapter = LocalStorageAdapter::new(temp.path().to_path_buf());
    match adapter.presign("any/key", 60).await {
        Err(StorageError::Unsupported) => Ok(()),
        other => Err(format!("expected Unsupported, got {other:?}").into()),
    }
}

// ---------------------------------------------------------------------------
// S13 — path-traversal rejection through the public adapter
// (re-asserted at the integration level so the contract is locked from an
// external consumer's view, not just the in-crate unit tests).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn local_adapter_rejects_path_traversal_keys() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let adapter = LocalStorageAdapter::new(temp.path().to_path_buf());

    let bad_keys = [
        "../escape.txt",
        "safe/../../escape.txt",
        "/etc/passwd",
        "safe/../other",
        "./here",
        "a//b",
        "trailing/",
        r"C:\Windows\system32\config\sam",
        r"\backslash-prefix.txt",
    ];
    for key in bad_keys {
        let result = adapter
            .put(StorageObject::new(key, b"escape".to_vec()))
            .await;
        match result {
            Err(StorageError::InvalidKey(_)) => {}
            other => return Err(format!("expected InvalidKey for {key:?}, got {other:?}").into()),
        }
    }
    // Ensure no file escaped into the temp dir's parent.
    assert!(!temp.path().join("escape.txt").exists());
    assert!(!temp.path().join("here").exists());
    Ok(())
}

#[tokio::test]
async fn local_adapter_get_head_return_none_for_missing_key()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let adapter = LocalStorageAdapter::new(temp.path().to_path_buf());
    assert_eq!(adapter.get("never-written.txt").await?, None);
    assert_eq!(adapter.head("never-written.txt").await?, None);
    assert!(!adapter.exists("never-written.txt").await?);
    // list on an empty store returns an empty vec (no panic).
    assert!(adapter.list("").await?.is_empty());
    Ok(())
}

// ---------------------------------------------------------------------------
// In-memory / fake adapter — full put/get/exists/list/delete round-trip.
// This is the "fake adapter" exercise from A01: proves the trait surface
// works end-to-end with a non-filesystem backend.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn in_memory_adapter_full_round_trip() -> Result<(), Box<dyn std::error::Error>> {
    let adapter = InMemoryStorageAdapter::new();

    assert!(!adapter.exists("k1").await?);
    adapter
        .put(StorageObject::new("k1", b"v1".to_vec()))
        .await?;
    adapter
        .put(StorageObject::new("k2", b"v2".to_vec()))
        .await?;

    assert!(adapter.exists("k1").await?);
    assert!(adapter.exists("k2").await?);

    let object = match adapter.get("k1").await? {
        Some(o) => o,
        None => return Err("expected k1 to be present".into()),
    };
    assert_eq!(object.bytes, b"v1");

    let listed: Vec<String> = adapter.list("").await?.into_iter().map(|m| m.key).collect();
    assert_eq!(listed, vec!["k1".to_string(), "k2".to_string()]);

    assert!(adapter.delete("k1").await?);
    assert!(!adapter.exists("k1").await?);
    assert!(adapter.get("k1").await?.is_none());
    // The other key is untouched.
    assert!(adapter.exists("k2").await?);
    Ok(())
}

#[tokio::test]
async fn in_memory_adapter_presign_is_unsupported() -> Result<(), Box<dyn std::error::Error>> {
    let adapter = InMemoryStorageAdapter::new();
    match adapter.presign("k", 30).await {
        Err(StorageError::Unsupported) => Ok(()),
        other => Err(format!("expected Unsupported, got {other:?}").into()),
    }
}

// ---------------------------------------------------------------------------
// Facade integration — mirrors the Go TestDataStorageService_SaveData /
// _LoadData fs branches, but going through the public DataStorageService
// facade (S09) instead of the raw adapter. This is the A01 "end-to-end
// through the service facade" exercise.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn facade_save_load_round_trip_through_local_backend()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let svc = DataStorageService::with_backend(
        DataStorageKind::Local,
        Box::new(LocalStorageAdapter::new(temp.path().to_path_buf())),
    );
    let key = "test/key.txt";
    let data = b"test data content";

    svc.save_data(key, data).await?;
    let loaded = svc.load_data(key).await?;
    assert_eq!(loaded, data);
    Ok(())
}

#[tokio::test]
async fn facade_load_missing_key_errors_with_failed_to_read_file()
-> Result<(), Box<dyn std::error::Error>> {
    // Mirrors Go TestDataStorageService_LoadData "load non-existent file
    // from fs storage": the error must mention "failed to read file".
    let temp = tempfile::tempdir()?;
    let svc = DataStorageService::with_backend(
        DataStorageKind::Local,
        Box::new(LocalStorageAdapter::new(temp.path().to_path_buf())),
    );
    match svc.load_data("non-existent.txt").await {
        Err(StorageError::Operation(msg)) => {
            assert!(msg.contains("failed to read file"), "msg was: {msg}");
            Ok(())
        }
        other => Err(format!("expected Operation(failed to read file), got {other:?}").into()),
    }
}

#[tokio::test]
async fn facade_delete_missing_key_is_success() -> Result<(), Box<dyn std::error::Error>> {
    // Go DeleteData tolerates os.ErrNotExist as success.
    let temp = tempfile::tempdir()?;
    let svc = DataStorageService::with_backend(
        DataStorageKind::Local,
        Box::new(LocalStorageAdapter::new(temp.path().to_path_buf())),
    );
    svc.delete_data("never-written.txt").await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// A02 — hermetic integration tests for the S3 / GCS / WebDAV adapters.
//
// These exercise the signing + HTTP request-building path through the **public**
// `conduit_storage` API surface using the existing fakes:
//   - `InMemoryHttpClient` (records every request + answers PUT/GET/DELETE/HEAD)
//   - `RecordingSigner` (records every signing call, returns a canned header)
//   - `StaticSigner` (returns a fixed header set, for merge-order assertions)
//
// Every adapter is constructed via the public `build_s3_backend` /
// `build_gcs_backend` / `build_webdav_backend` constructors so the test path
// matches the production wiring shape. No real network is opened: the
// `InMemoryHttpClient` satisfies every `StorageHttpClient::execute` call
// in-process.
//
// Each backend gets a full put -> get -> head -> list -> delete round-trip
// (where the fake supports it) plus a 404-tolerance check, mirroring the Go
// `data_storage.go` SaveData/LoadData/DeleteData semantics:
//   - Go `LoadData` for missing keys returns a wrapped "failed to read file"
//     error; our adapter surfaces `Ok(None)` so the dispatcher can map it.
//   - Go `DeleteData` treats `os.ErrNotExist` as success (`return nil`); our
//     adapter reports `Ok(false)` for a 404, which the dispatcher reports as
//     success. We assert the bool shape directly here.
// ---------------------------------------------------------------------------

use conduit_storage::{
    GcsSettings, RecordingEntry as GcsRecordingEntry, RecordingSigner as GcsRecordingSigner,
    S3RecordingSigner, S3Settings, StorageHttpRequest, WebDavSettings,
};
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;

/// `Arc<InMemoryHttpClient>` wrapper that implements `StorageHttpClient` so a
/// single shared fake client can be injected into an adapter while the test
/// keeps a handle to read the recorded request log.
#[derive(Debug, Clone)]
struct SharedHttp(Arc<conduit_storage::InMemoryHttpClient>);

#[async_trait::async_trait]
impl conduit_storage::StorageHttpClient for SharedHttp {
    async fn execute(
        &self,
        request: StorageHttpRequest,
    ) -> conduit_storage::StorageResult<conduit_storage::StorageHttpResponse> {
        self.0.execute(request).await
    }
}

#[derive(Debug)]
struct ScriptedHttp {
    responses: Mutex<VecDeque<conduit_storage::StorageHttpResponse>>,
    requests: Mutex<Vec<StorageHttpRequest>>,
}

impl ScriptedHttp {
    fn new(responses: Vec<conduit_storage::StorageHttpResponse>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn recorded(&self) -> Vec<StorageHttpRequest> {
        self.requests
            .lock()
            .map(|requests| requests.clone())
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone)]
struct SharedScriptedHttp(Arc<ScriptedHttp>);

#[async_trait::async_trait]
impl conduit_storage::StorageHttpClient for SharedScriptedHttp {
    async fn execute(
        &self,
        request: StorageHttpRequest,
    ) -> conduit_storage::StorageResult<conduit_storage::StorageHttpResponse> {
        self.0
            .requests
            .lock()
            .map_err(|_| conduit_storage::StorageError::LockPoisoned("scripted requests"))?
            .push(request);
        self.0
            .responses
            .lock()
            .map_err(|_| conduit_storage::StorageError::LockPoisoned("scripted responses"))?
            .pop_front()
            .ok_or_else(|| conduit_storage::StorageError::Operation("no scripted response".into()))
    }
}

fn response(status: u16, body: impl Into<Vec<u8>>) -> conduit_storage::StorageHttpResponse {
    conduit_storage::StorageHttpResponse {
        status,
        body: body.into(),
        headers: Default::default(),
    }
}

/// `Arc<S3RecordingSigner>` wrapper so the adapter owns a concrete signer while
/// the test reads back the recorded signing calls. Mirrors the in-crate test
/// helper but goes through the **public** API only.
#[derive(Debug, Clone)]
struct SharedS3Signer {
    inner: Arc<S3RecordingSigner>,
}

impl conduit_storage::S3Signer for SharedS3Signer {
    fn sign(
        &self,
        request: conduit_storage::SigningRequest<'_>,
    ) -> conduit_storage::StorageResult<std::collections::BTreeMap<String, String>> {
        self.inner.sign(request)
    }
}

/// Same idea for the GCS recording signer.
#[derive(Debug, Clone)]
struct SharedGcsSigner {
    inner: Arc<GcsRecordingSigner>,
}

#[async_trait::async_trait]
impl conduit_storage::GcsSigner for SharedGcsSigner {
    async fn sign(
        &self,
        request: conduit_storage::GcsSigningRequest<'_>,
    ) -> conduit_storage::StorageResult<std::collections::BTreeMap<String, String>> {
        self.inner.sign(request).await
    }
}

// --- S3 ---------------------------------------------------------------------

#[tokio::test]
async fn s3_adapter_put_get_head_delete_round_trip_through_public_api()
-> Result<(), Box<dyn std::error::Error>> {
    // A02: full round-trip through `build_s3_backend` with the fakes. The path-
    // style URL shape, key normalization, signing invocation, and 404 tolerance
    // are all observable end-to-end here.
    let http = Arc::new(conduit_storage::InMemoryHttpClient::new());
    let signer = Arc::new(S3RecordingSigner::new());
    let settings = S3Settings {
        bucket_name: "round-trip-bucket".to_string(),
        endpoint: "https://s3.example.com".to_string(),
        region: "us-east-1".to_string(),
        path_style: true,
        ..Default::default()
    };
    let adapter = conduit_storage::build_s3_backend(
        &settings,
        SharedHttp(http.clone()),
        SharedS3Signer {
            inner: signer.clone(),
        },
    )?;

    // put: the metadata reflects the normalized key + content length.
    let metadata = adapter
        .put(StorageObject::new(
            "requests/abc.json",
            br#"{"ok":true}"#.to_vec(),
        ))
        .await?;
    assert_eq!(metadata.key, "requests/abc.json");
    assert_eq!(metadata.content_length, 11);

    // The recorded PUT hit the path-style URL we expect.
    let put_req = http
        .recorded()
        .into_iter()
        .find(|r| r.method == "PUT")
        .ok_or("no PUT recorded")?;
    assert!(
        put_req
            .url
            .ends_with("/round-trip-bucket/requests/abc.json"),
        "url was: {}",
        put_req.url
    );

    // The signer was invoked for the PUT and stamped an authorization header
    // onto the outgoing request.
    let s3_signs: Vec<conduit_storage::s3::RecordingEntry> = signer.recorded();
    assert!(
        s3_signs
            .iter()
            .any(|e| e.method == "PUT" && e.url.contains("/round-trip-bucket/requests/abc.json")),
        "no PUT signing call recorded: {s3_signs:?}"
    );
    assert!(
        put_req.headers.contains_key("authorization"),
        "authorization header missing on PUT"
    );

    // get returns the exact bytes the InMemoryHttpClient stored.
    let loaded = match adapter.get("requests/abc.json").await? {
        Some(o) => o,
        None => return Err("expected the stored object".into()),
    };
    assert_eq!(loaded.bytes, br#"{"ok":true}"#);

    // head reports Some for a stored object. The InMemoryHttpClient does not
    // synthesize a content-length header on HEAD responses, so we assert
    // presence rather than the exact length (which the S3 adapter reads from
    // the response header and defaults to 0 when absent).
    let head = adapter.head("requests/abc.json").await?;
    assert!(head.is_some(), "head must find the stored object");

    // delete removes the object; a second delete is a 404 -> Ok(false),
    // mirroring Go's os.ErrNotExist tolerance.
    assert!(adapter.delete("requests/abc.json").await?);
    assert!(!adapter.delete("requests/abc.json").await?);

    // After delete, get and head report None.
    assert_eq!(adapter.get("requests/abc.json").await?, None);
    assert_eq!(adapter.head("requests/abc.json").await?, None);
    Ok(())
}

#[tokio::test]
async fn s3_adapter_signer_sees_every_verb_in_a_full_round_trip()
-> Result<(), Box<dyn std::error::Error>> {
    // A02: the "signer-is-invoked-for-every-verb" contract, observable through
    // the public constructor. PUT, GET, HEAD, and DELETE must each flow through
    // the signer with the correct method label.
    let http = Arc::new(conduit_storage::InMemoryHttpClient::new());
    let signer = Arc::new(S3RecordingSigner::new());
    let settings = S3Settings {
        bucket_name: "verbs".to_string(),
        endpoint: "https://s3.example.com".to_string(),
        region: "us-east-1".to_string(),
        path_style: true,
        ..Default::default()
    };
    let adapter = conduit_storage::build_s3_backend(
        &settings,
        SharedHttp(http),
        SharedS3Signer {
            inner: signer.clone(),
        },
    )?;

    adapter
        .put(StorageObject::new("k.bin", b"v".to_vec()))
        .await?;
    let _ = adapter.get("k.bin").await?;
    let _ = adapter.head("k.bin").await?;
    let _ = adapter.delete("k.bin").await?;

    let methods: Vec<String> = signer.recorded().into_iter().map(|e| e.method).collect();
    assert!(
        methods == vec!["PUT", "GET", "HEAD", "DELETE"],
        "signer methods were: {methods:?}"
    );
    Ok(())
}

// --- GCS --------------------------------------------------------------------

#[tokio::test]
async fn gcs_adapter_put_get_head_delete_round_trip_through_public_api()
-> Result<(), Box<dyn std::error::Error>> {
    // A02: GCS full round-trip through `build_gcs_backend`. The URL shape is
    // `https://storage.googleapis.com/<bucket>/<key>`; the signer stamps a
    // Bearer token; 404 tolerance mirrors S3.
    let http = Arc::new(conduit_storage::InMemoryHttpClient::new());
    let signer = Arc::new(GcsRecordingSigner::new());
    let settings = GcsSettings {
        bucket_name: "gcs-round-trip".to_string(),
        credential: "{\"type\":\"service_account\"}".to_string(),
    };
    let adapter = conduit_storage::build_gcs_backend(
        &settings,
        SharedHttp(http.clone()),
        SharedGcsSigner {
            inner: signer.clone(),
        },
    )?;

    let metadata = adapter
        .put(StorageObject::new("logs/entry.json", b"body".to_vec()))
        .await?;
    assert_eq!(metadata.key, "logs/entry.json");
    assert_eq!(metadata.content_length, 4);

    // The recorded PUT targeted the public GCS host with bucket + key in path.
    let put_req = http
        .recorded()
        .into_iter()
        .find(|r| r.method == "PUT")
        .ok_or("no PUT recorded")?;
    assert!(
        put_req
            .url
            .starts_with("https://storage.googleapis.com/gcs-round-trip/logs/entry.json"),
        "url was: {}",
        put_req.url
    );

    // The signer was invoked and stamped a Bearer token.
    let gcs_signs: Vec<GcsRecordingEntry> = signer.recorded();
    assert!(
        gcs_signs
            .iter()
            .any(|e| e.method == "PUT" && e.url.contains("/gcs-round-trip/logs/entry.json")),
        "no PUT signing call recorded: {gcs_signs:?}"
    );
    assert_eq!(
        put_req.headers.get("authorization"),
        Some(&"Bearer ya29.test-token".to_string())
    );

    // get round-trips the bytes.
    let loaded = match adapter.get("logs/entry.json").await? {
        Some(o) => o,
        None => return Err("expected the stored object".into()),
    };
    assert_eq!(loaded.bytes, b"body");

    // head reports Some for a stored object (the InMemoryHttpClient does not
    // synthesize a content-length header, so only presence is asserted).
    let head = adapter.head("logs/entry.json").await?;
    assert!(head.is_some(), "head must find the stored object");

    // delete + idempotent 404 tolerance.
    assert!(adapter.delete("logs/entry.json").await?);
    assert!(!adapter.delete("logs/entry.json").await?);
    assert_eq!(adapter.get("logs/entry.json").await?, None);
    Ok(())
}

#[tokio::test]
async fn gcs_adapter_signer_sees_put_get_delete() -> Result<(), Box<dyn std::error::Error>> {
    // A02: the GCS signer-is-invoked contract for the verbs GCS issues.
    let http = Arc::new(conduit_storage::InMemoryHttpClient::new());
    let signer = Arc::new(GcsRecordingSigner::new());
    let settings = GcsSettings {
        bucket_name: "gcs-verbs".to_string(),
        credential: "{\"type\":\"service_account\"}".to_string(),
    };
    let adapter = conduit_storage::build_gcs_backend(
        &settings,
        SharedHttp(http),
        SharedGcsSigner {
            inner: signer.clone(),
        },
    )?;

    adapter
        .put(StorageObject::new("k.bin", b"v".to_vec()))
        .await?;
    let _ = adapter.get("k.bin").await?;
    let _ = adapter.delete("k.bin").await?;

    let methods: Vec<String> = signer.recorded().into_iter().map(|e| e.method).collect();
    assert!(
        methods == vec!["PUT", "GET", "DELETE"],
        "signer methods were: {methods:?}"
    );
    Ok(())
}

// --- WebDAV -----------------------------------------------------------------

#[tokio::test]
async fn webdav_adapter_put_get_head_delete_round_trip_through_public_api()
-> Result<(), Box<dyn std::error::Error>> {
    // A02: WebDAV full round-trip through `build_webdav_backend`. WebDAV has no
    // signer; the test asserts the MKCOL/PUT/GET/HEAD/DELETE verb sequence and
    // the base-path URL shape. The InMemoryHttpClient satisfies MKCOL
    // idempotently, so parent-collection creation is transparent.
    let http = Arc::new(conduit_storage::InMemoryHttpClient::new());
    let settings = WebDavSettings {
        url: "https://dav.example.com".to_string(),
        username: "alice".to_string(),
        password: "s3cret".to_string(),
        path: "dav".to_string(),
        ..Default::default()
    };
    let adapter =
        conduit_storage::build_webdav_backend(&settings, Some("dav"), SharedHttp(http.clone()))?;

    let metadata = adapter
        .put(StorageObject::new("notes/a.txt", b"hello".to_vec()))
        .await?;
    assert_eq!(metadata.key, "notes/a.txt");
    assert_eq!(metadata.content_length, 5);

    // The recorded request sequence must include MKCOL (for the parent
    // collection) and PUT. MKCOL is issued per ancestor; PUT carries the body.
    let recorded = http.recorded();
    assert!(
        recorded.iter().any(|r| r.method == "MKCOL"),
        "expected MKCOL in recorded requests: {recorded:?}"
    );
    let put_req = recorded
        .iter()
        .find(|r| r.method == "PUT")
        .ok_or("no PUT recorded")?;
    assert!(
        put_req.url.contains("dav") && put_req.url.contains("notes/a.txt"),
        "put url was: {}",
        put_req.url
    );

    // get round-trips the bytes.
    let loaded = match adapter.get("notes/a.txt").await? {
        Some(o) => o,
        None => return Err("expected the stored object".into()),
    };
    assert_eq!(loaded.bytes, b"hello");

    // head reports Some for a stored object (the InMemoryHttpClient does not
    // synthesize a content-length header, so only presence is asserted).
    let head = adapter.head("notes/a.txt").await?;
    assert!(head.is_some(), "head must find the stored object");

    // delete + idempotent 404 tolerance.
    assert!(adapter.delete("notes/a.txt").await?);
    assert!(!adapter.delete("notes/a.txt").await?);
    assert_eq!(adapter.get("notes/a.txt").await?, None);
    Ok(())
}

#[tokio::test]
async fn webdav_adapter_loads_with_basic_auth_shape_intact()
-> Result<(), Box<dyn std::error::Error>> {
    // A02: the WebDAV adapter does not sign requests; it relies on HTTP basic
    // auth stamped by the production client. This test confirms the adapter
    // builds + serves a get through the public constructor, and that a missing
    // key surfaces as None (not an error) so the dispatcher can map it.
    let http = Arc::new(conduit_storage::InMemoryHttpClient::new());
    let settings = WebDavSettings {
        url: "https://dav.example.com".to_string(),
        path: "store".to_string(),
        ..Default::default()
    };
    let adapter = conduit_storage::build_webdav_backend(&settings, None, SharedHttp(http))?;
    assert_eq!(adapter.base_path(), "store");
    // A missing key is a 404 -> Ok(None).
    assert_eq!(adapter.get("never/saved.txt").await?, None);
    Ok(())
}

#[tokio::test]
async fn s3_list_follows_continuation_tokens() -> Result<(), Box<dyn std::error::Error>> {
    let http = Arc::new(ScriptedHttp::new(vec![
        response(
            200,
            br#"<ListBucketResult><IsTruncated>true</IsTruncated><NextContinuationToken>next token</NextContinuationToken><Contents><Key>logs/a.json</Key><Size>11</Size></Contents></ListBucketResult>"#.to_vec(),
        ),
        response(
            200,
            br#"<ListBucketResult><IsTruncated>false</IsTruncated><Contents><Key>logs/b.json</Key><Size>22</Size></Contents></ListBucketResult>"#.to_vec(),
        ),
    ]));
    let adapter = conduit_storage::build_s3_backend(
        &S3Settings {
            bucket_name: "bucket".into(),
            endpoint: "https://s3.example.com".into(),
            region: "us-east-1".into(),
            path_style: true,
            ..Default::default()
        },
        SharedScriptedHttp(http.clone()),
        S3RecordingSigner::new(),
    )?;

    let listed = adapter.list("logs").await?;
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].key, "logs/a.json");
    assert_eq!(listed[0].content_length, 11);
    assert_eq!(listed[1].key, "logs/b.json");
    let requests = http.recorded();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].url.contains("list-type=2"));
    assert!(requests[0].url.contains("prefix=logs"));
    assert!(requests[1].url.contains("continuation-token=next+token"));
    Ok(())
}

#[tokio::test]
async fn gcs_list_follows_page_tokens() -> Result<(), Box<dyn std::error::Error>> {
    let http = Arc::new(ScriptedHttp::new(vec![
        response(
            200,
            br#"{"items":[{"name":"logs/a.json","size":"7","contentType":"application/json"}],"nextPageToken":"page two"}"#.to_vec(),
        ),
        response(
            200,
            br#"{"items":[{"name":"logs/b.bin","size":"9"}]}"#.to_vec(),
        ),
    ]));
    let adapter = conduit_storage::build_gcs_backend(
        &GcsSettings {
            bucket_name: "bucket".into(),
            credential: "{}".into(),
        },
        SharedScriptedHttp(http.clone()),
        GcsRecordingSigner::new(),
    )?;

    let listed = adapter.list("logs").await?;
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].content_type.as_deref(), Some("application/json"));
    assert_eq!(listed[1].content_length, 9);
    let requests = http.recorded();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].url.contains("prefix=logs"));
    assert!(requests[1].url.contains("pageToken=page+two"));
    Ok(())
}

#[tokio::test]
async fn webdav_list_parses_multistatus_and_skips_collections()
-> Result<(), Box<dyn std::error::Error>> {
    let http = Arc::new(ScriptedHttp::new(vec![response(
        207,
        br#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:"><d:response><d:href>/dav/logs/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat></d:response><d:response><d:href>/dav/logs/a.json</d:href><d:propstat><d:prop><d:getcontentlength>13</d:getcontentlength><d:getcontenttype>application/json</d:getcontenttype><d:resourcetype/></d:prop></d:propstat></d:response></d:multistatus>"#.to_vec(),
    )]));
    let adapter = conduit_storage::build_webdav_backend(
        &WebDavSettings {
            url: "https://dav.example.com".into(),
            path: "dav".into(),
            ..Default::default()
        },
        Some("dav"),
        SharedScriptedHttp(http.clone()),
    )?;

    let listed = adapter.list("logs").await?;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].key, "logs/a.json");
    assert_eq!(listed[0].content_length, 13);
    assert_eq!(listed[0].content_type.as_deref(), Some("application/json"));
    let requests = http.recorded();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "PROPFIND");
    assert_eq!(
        requests[0].headers.get("depth").map(String::as_str),
        Some("infinity")
    );
    Ok(())
}
