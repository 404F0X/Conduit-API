#![forbid(unsafe_code)]

pub mod adapter;
pub mod backend;
pub mod gcs;
pub mod http;
pub mod s3;
pub mod service;
pub mod settings;
pub mod webdav;

pub use adapter::{
    DataStorageKind, DataStorageRow, DataStorageSettings, InMemoryStorageAdapter,
    LocalStorageAdapter, StorageAdapter, StorageError, StorageMetadata, StorageObject,
    StorageResult, mask_storage_credentials, resolve_local_object_key, validate_single_primary,
};
pub use backend::{
    DatabaseStorageAdapter, build_gcs_backend, build_gcs_production_backend, build_s3_backend,
    build_s3_production_backend, build_storage_backend, build_webdav_backend,
    build_webdav_production_backend,
};
pub use gcs::{
    DeferredGcsSigner, GcsSigner, GcsSigningRequest, GcsStorageAdapter, RecordingEntry,
    RecordingSigner, ServiceAccountGcsSigner, StaticSigner,
};
pub use http::{DEFAULT_WEBDAV_TIMEOUT, ReqwestStorageHttpClient};
pub use s3::{
    AwsSigV4Signer, DeferredSigV4Signer, PresigningRequest, RecordingSigner as S3RecordingSigner,
    S3Signer, S3StorageAdapter, SigningRequest, StaticSigner as S3StaticSigner,
};
pub use service::DataStorageService;
pub use settings::{
    DataStorageConfig, GcsSettings, S3Settings, WebDavSettings, is_gcs_provided, is_s3_provided,
    is_webdav_provided, merge_data_storage_settings,
};
pub use webdav::{
    InMemoryHttpClient, StorageHttpClient, StorageHttpRequest, StorageHttpResponse,
    WebDavStorageAdapter,
};

pub const CRATE_NAME: &str = "conduit-storage";
