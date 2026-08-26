#![forbid(unsafe_code)]

pub mod error;
pub mod objects;

pub use error::{
    ApiErrorMappable, ApiErrorMapping, ConduitError, ERROR_RESPONSE_BODY_METADATA,
    ERROR_RESPONSE_REWRITE_CHANNEL_METADATA, ERROR_RESPONSE_TYPE_METADATA, ErrorKind,
    UpstreamErrorPolicy, UpstreamErrorPolicyMode, admin_error_json, anthropic_error_json,
    anthropic_error_type_for_status, api_error_mapping, custom_error_response_body,
    custom_error_response_type, gemini_error_json, map_http_status_to_gemini_status,
    openai_error_json,
};

pub use objects::{Condition, ConditionType};
