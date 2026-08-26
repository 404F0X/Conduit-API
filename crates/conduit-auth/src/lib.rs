#![forbid(unsafe_code)]

pub mod apikey;
pub mod http_auth;
pub mod jwt;
pub mod password;
pub mod policy;
pub mod principal;
pub mod rbac;
pub mod request_context;
pub mod scopes;

pub use apikey::{
    API_KEY_HEX_CHARS, API_KEY_RANDOM_BYTES, ApiKeyError, generate_api_key, generate_secret_key,
    reject_no_auth_sentinel,
};
pub use http_auth::{
    API_KEY_HEADER, ApiKeyHeader, ApiKeySource, AuthExtractionResult, AuthFailure, ExtractedApiKey,
    GOOGLE_API_KEY_HEADER, NO_AUTH_SENTINEL, extract_api_key, extract_gemini_api_key,
};
pub use jwt::{Claims, DEFAULT_JWT_TTL, JwtError, decode_hs256, encode_hs256};
pub use password::{
    DEFAULT_BCRYPT_COST, PasswordError, encode_password_bcrypt_hex, verify_password_bcrypt_hex,
};
pub use policy::{
    AuditEntry, BypassAuditSink, BypassError, BypassOr, BypassReason, MutationOp, MutationTarget,
    OwnerScope, PolicyGuard,
};
pub use principal::{Principal, PrincipalKind};
pub use rbac::{PermissionDecision, PermissionSource, has_project_scope, has_scope};
pub use request_context::{ContextConflictError, RequestContext, RequestSource};
pub use scopes::{Scope, ScopeSet, slug};

pub const CRATE_NAME: &str = "conduit-auth";
