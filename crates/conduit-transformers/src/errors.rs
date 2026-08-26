use conduit_core::ErrorKind;
use conduit_core::anthropic_error_type_for_status;
use conduit_core::{ConduitError, admin_error_json, openai_error_json};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorProtocol {
    Admin,
    OpenAi,
    Anthropic,
    Gemini,
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error("{protocol} error mapper is not implemented yet")]
pub struct ErrorMapperNotImplemented {
    pub protocol: &'static str,
}

pub fn map_error_for_protocol(
    protocol: ErrorProtocol,
    err: &ConduitError,
) -> Result<Value, ErrorMapperNotImplemented> {
    match protocol {
        ErrorProtocol::Admin => Ok(admin_error_json(err)),
        ErrorProtocol::OpenAi => Ok(openai_error_json(err)),
        ErrorProtocol::Anthropic => anthropic_error_json(err),
        ErrorProtocol::Gemini => gemini_error_json(err),
    }
}

/// Build the Anthropic error-response JSON for an `ConduitError`.
///
/// Mirrors the Go `(*anthropic.InboundTransformer).TransformError`
/// (`conduit/llm/transformer/anthropic/inbound.go:151-220`) envelope:
///
/// ```json
/// { "type": "<type>", "error": { "message": "...", "type": "<type>" }, "request_id": "..." }
/// ```
///
/// The `<type>` field is the Anthropic API error-type vocabulary
/// (https://platform.claude.com/docs/en/api/errors): `invalid_request_error`,
/// `authentication_error`, `permission_error`, `not_found_error`,
/// `rate_limit_error`, `api_error`, `overloaded_error`, `invalid_model_error`.
/// Go's inbound transformer selects the type from the upstream `llm.ErrorDetail`
/// it already carries; here we derive it from the `ConduitError`'s HTTP status
/// (which is itself derived from `ErrorKind` via `default_http_status`), since
/// the kind→status table is the canonical mapping the Go pipeline relies on.
pub fn anthropic_error_json(err: &ConduitError) -> Result<Value, ErrorMapperNotImplemented> {
    // Single source of truth lives in `conduit-core` so the HTTP layer (which
    // does not depend on this crate) can render the same envelope per route.
    Ok(conduit_core::anthropic_error_json(err))
}

/// Build the Gemini error-response JSON for an `ConduitError`.
///
/// Mirrors the Go `(*gemini.InboundTransformer).TransformError`
/// (`conduit/llm/transformer/gemini/inbound.go:114-190`) envelope:
///
/// ```json
/// { "error": { "code": <http_status>, "message": "...", "status": "<STATUS>" } }
/// ```
///
/// The `status` string comes from Go's `mapHTTPStatusToGeminiStatus`
/// (`conduit/llm/transformer/gemini/inbound.go:192-216`), which maps each HTTP
/// status code to a canonical Google Canonical Status Code string
/// (`INVALID_ARGUMENT`, `UNAUTHENTICATED`, `PERMISSION_DENIED`, `NOT_FOUND`,
/// `ALREADY_EXISTS`, `RESOURCE_EXHAUSTED`, `INTERNAL`, `UNIMPLEMENTED`,
/// `UNAVAILABLE`, `UNKNOWN`).
pub fn gemini_error_json(err: &ConduitError) -> Result<Value, ErrorMapperNotImplemented> {
    // See `anthropic_error_json` — the builder is shared via `conduit-core`.
    Ok(conduit_core::gemini_error_json(err))
}

/// Convenience: classify an `ConduitError`'s `ErrorKind` into the Anthropic
/// error-type string directly (used by callers that already hold the kind).
#[allow(dead_code)]
fn anthropic_error_type_for_kind(kind: ErrorKind) -> &'static str {
    anthropic_error_type_for_status(kind.default_http_status())
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_core::ConduitError;
    use serde_json::json;

    // ---- Anthropic: per-ErrorKind error-type mapping parity ----
    //
    // Mirrors the Anthropic API error-type vocabulary selected by Go's
    // `(*anthropic.InboundTransformer).TransformError` per status branch.
    // The ConduitError's HTTP status is derived from `ErrorKind::default_http_status`,
    // so each kind maps to a stable Anthropic error type.

    #[test]
    fn anthropic_invalid_request_maps_to_invalid_request_error() {
        let err = ConduitError::invalid_request("missing model");
        let body = anthropic_error_json(&err).unwrap_or_else(|e| panic!("{e:?}"));

        assert_eq!(body["type"], "invalid_request_error");
        assert_eq!(body["error"]["type"], "invalid_request_error");
        assert_eq!(body["error"]["message"], "missing model");
        assert_eq!(body["request_id"], "");
    }

    #[test]
    fn anthropic_invalid_model_falls_under_invalid_request_error() {
        // ErrorKind::InvalidModel defaults to HTTP 400 in Rust (matching the
        // Rust ErrorKind table); Anthropic's distinct 422 invalid_model_error
        // branch in Go is reached only when the upstream sets status 422
        // explicitly, which ConduitError's kind-derived status does not.
        let err = with_kind_for_test(
            ConduitError::invalid_request("bad model"),
            ErrorKind::InvalidModel,
        );
        let body = anthropic_error_json(&err).unwrap_or_else(|e| panic!("{e:?}"));

        assert_eq!(body["type"], "invalid_request_error");
        assert_eq!(body["error"]["type"], "invalid_request_error");
    }

    #[test]
    fn anthropic_invalid_model_error_type_reachable_with_422_status() {
        // When a caller explicitly marks the error HTTP 422 (parity with Go's
        // ErrInvalidModel branch in the Anthropic inbound), the
        // invalid_model_error Anthropic type is selected.
        let err = ConduitError::invalid_request("bad model").with_http_status(422);
        let body = anthropic_error_json(&err).unwrap_or_else(|e| panic!("{e:?}"));

        assert_eq!(body["type"], "invalid_model_error");
    }

    #[test]
    fn anthropic_unauthorized_maps_to_authentication_error() {
        let err = ConduitError::unauthorized("bad api key");
        let body = anthropic_error_json(&err).unwrap_or_else(|e| panic!("{e:?}"));

        assert_eq!(body["type"], "authentication_error");
        assert_eq!(body["error"]["message"], "Unauthorized");
    }

    #[test]
    fn anthropic_forbidden_maps_to_permission_error() {
        let err = ConduitError::forbidden("no access");
        let body = anthropic_error_json(&err).unwrap_or_else(|e| panic!("{e:?}"));

        assert_eq!(body["type"], "permission_error");
    }

    #[test]
    fn anthropic_not_found_maps_to_not_found_error() {
        let err = ConduitError::not_found("missing");
        let body = anthropic_error_json(&err).unwrap_or_else(|e| panic!("{e:?}"));

        assert_eq!(body["type"], "not_found_error");
    }

    #[test]
    fn anthropic_rate_limited_maps_to_rate_limit_error() {
        let err = ConduitError::rate_limited("slow down");
        let body = anthropic_error_json(&err).unwrap_or_else(|e| panic!("{e:?}"));

        assert_eq!(body["type"], "rate_limit_error");
        assert_eq!(body["error"]["message"], "slow down");
    }

    #[test]
    fn anthropic_quota_exhausted_maps_to_rate_limit_error() {
        // QuotaExhausted has the same 429 default status as RateLimited.
        let err = ConduitError::quota_exhausted("monthly quota exhausted");
        let body = anthropic_error_json(&err).unwrap_or_else(|e| panic!("{e:?}"));

        assert_eq!(body["type"], "rate_limit_error");
    }

    #[test]
    fn anthropic_internal_maps_to_api_error() {
        let err = ConduitError::internal("boom");
        let body = anthropic_error_json(&err).unwrap_or_else(|e| panic!("{e:?}"));

        assert_eq!(body["type"], "api_error");
        // Internal server errors must not leak the raw internal message.
        assert_eq!(body["error"]["message"], "Internal server error");
    }

    #[test]
    fn anthropic_upstream_maps_to_overloaded_error() {
        // Upstream has default status 502 → overloaded_error (parity with
        // Go's overloaded_error handling for upstream/provider failures).
        let err = ConduitError::upstream("provider down");
        let body = anthropic_error_json(&err).unwrap_or_else(|e| panic!("{e:?}"));

        assert_eq!(body["type"], "overloaded_error");
    }

    #[test]
    fn anthropic_timeout_maps_to_overloaded_error() {
        // Timeout default status 504 → overloaded_error.
        let err = with_kind_for_test(ConduitError::internal("timed out"), ErrorKind::Timeout);
        let body = anthropic_error_json(&err).unwrap_or_else(|e| panic!("{e:?}"));

        assert_eq!(body["type"], "overloaded_error");
    }

    #[test]
    fn anthropic_propagates_request_id_from_metadata() {
        let err =
            ConduitError::invalid_request("bad").with_metadata("request_id", json!("req_abc"));
        let body = anthropic_error_json(&err).unwrap_or_else(|e| panic!("{e:?}"));

        assert_eq!(body["request_id"], "req_abc");
    }

    #[test]
    fn anthropic_envelope_shape_matches_go_golden() {
        // Mirrors the Go golden body in TestInboundTransformer_TransformError
        // (inbound_test.go:2037): `{"type":"...","error":{...},"request_id":"..."}`.
        let err = ConduitError::internal("some error").with_metadata("request_id", json!("123456"));
        let body = anthropic_error_json(&err).unwrap_or_else(|e| panic!("{e:?}"));

        assert_eq!(
            body,
            json!({
                "type": "api_error",
                "error": {
                    "message": "Internal server error",
                    "type": "api_error",
                },
                "request_id": "123456",
            })
        );
    }

    // ---- Gemini: per-ErrorKind status mapping parity ----
    //
    // Mirrors Go's `mapHTTPStatusToGeminiStatus`
    // (gemini/inbound.go:192-216) and the `(*gemini.InboundTransformer).TransformError`
    // envelope `{"error":{"code":<int>,"message":"...","status":"<STATUS>"}}`.

    #[test]
    fn gemini_invalid_request_maps_to_invalid_argument() {
        let err = ConduitError::invalid_request("bad");
        let body = gemini_error_json(&err).unwrap_or_else(|e| panic!("{e:?}"));

        assert_eq!(body["error"]["code"], 400);
        assert_eq!(body["error"]["status"], "INVALID_ARGUMENT");
        assert_eq!(body["error"]["message"], "bad");
    }

    #[test]
    fn gemini_unauthorized_maps_to_unauthenticated() {
        let err = ConduitError::unauthorized("no key");
        let body = gemini_error_json(&err).unwrap_or_else(|e| panic!("{e:?}"));

        assert_eq!(body["error"]["code"], 401);
        assert_eq!(body["error"]["status"], "UNAUTHENTICATED");
    }

    #[test]
    fn gemini_forbidden_maps_to_permission_denied() {
        let err = ConduitError::forbidden("denied");
        let body = gemini_error_json(&err).unwrap_or_else(|e| panic!("{e:?}"));

        assert_eq!(body["error"]["code"], 403);
        assert_eq!(body["error"]["status"], "PERMISSION_DENIED");
    }

    #[test]
    fn gemini_not_found_maps_to_not_found() {
        let err = ConduitError::not_found("absent");
        let body = gemini_error_json(&err).unwrap_or_else(|e| panic!("{e:?}"));

        assert_eq!(body["error"]["code"], 404);
        assert_eq!(body["error"]["status"], "NOT_FOUND");
    }

    #[test]
    fn gemini_rate_limited_maps_to_resource_exhausted() {
        let err = ConduitError::rate_limited("slow");
        let body = gemini_error_json(&err).unwrap_or_else(|e| panic!("{e:?}"));

        assert_eq!(body["error"]["code"], 429);
        assert_eq!(body["error"]["status"], "RESOURCE_EXHAUSTED");
    }

    #[test]
    fn gemini_internal_maps_to_internal() {
        let err = ConduitError::internal("boom");
        let body = gemini_error_json(&err).unwrap_or_else(|e| panic!("{e:?}"));

        assert_eq!(body["error"]["code"], 500);
        assert_eq!(body["error"]["status"], "INTERNAL");
        assert_eq!(body["error"]["message"], "Internal server error");
    }

    #[test]
    fn gemini_envelope_shape_matches_go_default() {
        let err = ConduitError::internal("err");
        let body = gemini_error_json(&err).unwrap_or_else(|e| panic!("{e:?}"));

        assert_eq!(
            body,
            json!({
                "error": {
                    "code": 500,
                    "message": "Internal server error",
                    "status": "INTERNAL",
                }
            })
        );
    }

    #[test]
    fn map_error_for_protocol_routes_all_four_protocols() {
        let err = ConduitError::invalid_request("bad");

        let _ =
            map_error_for_protocol(ErrorProtocol::Admin, &err).unwrap_or_else(|e| panic!("{e:?}"));
        let _ =
            map_error_for_protocol(ErrorProtocol::OpenAi, &err).unwrap_or_else(|e| panic!("{e:?}"));
        let _ = map_error_for_protocol(ErrorProtocol::Anthropic, &err)
            .unwrap_or_else(|e| panic!("{e:?}"));
        let _ =
            map_error_for_protocol(ErrorProtocol::Gemini, &err).unwrap_or_else(|e| panic!("{e:?}"));
    }
}

// ---- helper used by tests to build kind-specific errors ----
//
// `ConduitError::new` already constructs an error from any `ErrorKind`, but the
// named constructors only cover the common kinds. This free function lets the
// tests above override the `kind` (and its derived HTTP status) without
// reaching into public constructor surface that doesn't exist for every kind.
#[cfg(test)]
fn with_kind_for_test(mut err: ConduitError, kind: ErrorKind) -> ConduitError {
    err.kind = kind;
    err.http_status = kind.default_http_status();
    err.code = Some(kind.as_str().to_string());
    err.safe_message = Some(kind.default_safe_message().to_string());
    err
}
