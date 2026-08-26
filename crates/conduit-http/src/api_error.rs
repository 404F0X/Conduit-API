//! Shared REST-API error shaping + gin binding subset for the admin
//! system/auth handlers (RUST-P11-003 S10).
//!
//! * [`json_error`] ports Go `api.JSONError` verbatim
//!   (`conduit/internal/server/api/error.go:12-20`).
//! * [`is_valid_email`] / [`min_chars`] cover the only gin `binding:"..."`
//!   tags those handlers rely on (`required,email`, `min=6`) — see
//!   `api/system.go:53-60` and `api/auth.go:31-34`.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use conduit_core::objects::response::{Error, ErrorResponse};

/// Rust port of Go `JSONError` (`api/error.go:12-20`):
///
/// ```text
/// func JSONError(c *gin.Context, status int, err error) {
///     _ = c.Error(err)
///     c.JSON(status, objects.ErrorResponse{
///         Error: objects.Error{
///             Type:    http.StatusText(status),
///             Message: err.Error(),
///         },
///     })
/// }
/// ```
///
/// The wire shape is `{"error":{"type":"<StatusText>","message":"<msg>"}}` —
/// note `type` is the *human* status text (`"Bad Request"`, `"Unauthorized"`,
/// `"Internal Server Error"`), not a snake_case error code. The frontend is a
/// fixed contract on this shape.
pub fn json_error(status: StatusCode, message: impl Into<String>) -> Response {
    (
        status,
        Json(ErrorResponse {
            error: Error {
                r#type: http_status_text(status),
                message: message.into(),
            },
        }),
    )
        .into_response()
}

/// Go `http.StatusText(code)` equivalent.
///
/// The `http` crate's `canonical_reason()` strings are identical to Go's
/// `http.StatusText` for every standard code these handlers emit
/// (400 "Bad Request", 401 "Unauthorized", 500 "Internal Server Error");
/// unknown codes yield `""` in both implementations.
pub fn http_status_text(status: StatusCode) -> String {
    status.canonical_reason().unwrap_or("").to_string()
}

/// gin `email` binding tag — pragmatic subset of go-playground/validator's
/// RFC 5322-ish `email` regex (validator@v10 `emailRegexString`):
///
/// * non-empty local part, single unquoted `@`,
/// * domain of at least two dot-separated non-empty labels
///   (the Go regex requires a `label "." label` tail, so `a@b` fails),
/// * no label starting/ending with `-`, no whitespace anywhere.
///
/// This intentionally does not replicate the full quoted-local-part/unicode
/// grammar; all handler golden cases (valid address passes, empty/`no-at`/
/// bare-host inputs fail) behave identically to gin.
pub fn is_valid_email(value: &str) -> bool {
    if value.chars().any(char::is_whitespace) {
        return false;
    }
    let Some((local, domain)) = value.split_once('@') else {
        return false;
    };
    // The unquoted local part of the Go regex cannot contain a second '@'.
    if local.is_empty() || domain.is_empty() || domain.contains('@') {
        return false;
    }
    let labels: Vec<&str> = domain.split('.').collect();
    if labels.len() < 2 {
        return false;
    }
    labels
        .iter()
        .all(|label| !label.is_empty() && !label.starts_with('-') && !label.ends_with('-'))
}

/// gin `min=N` on a string field: go-playground/validator compares
/// `utf8.RuneCountInString`, i.e. characters, not bytes.
pub fn min_chars(value: &str, min: usize) -> bool {
    value.chars().count() >= min
}

#[cfg(test)]
mod tests {
    use std::error::Error as StdError;

    use axum::body::to_bytes;
    use serde_json::{Value, json};

    use super::*;

    async fn response_json(response: Response) -> Result<(StatusCode, Value), Box<dyn StdError>> {
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 4096).await?;
        Ok((status, serde_json::from_slice(&bytes)?))
    }

    /// Mirrors Go `JSONError` output for the three statuses the system/auth
    /// handlers emit (error.go:14-18 + Go `http.StatusText`).
    #[tokio::test]
    async fn json_error_matches_go_status_text_shapes() -> Result<(), Box<dyn StdError>> {
        for (status, want_type) in [
            (StatusCode::BAD_REQUEST, "Bad Request"),
            (StatusCode::UNAUTHORIZED, "Unauthorized"),
            (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error"),
        ] {
            let (got_status, body) = response_json(json_error(status, "boom")).await?;
            assert_eq!(got_status, status);
            assert_eq!(
                body,
                json!({"error": {"type": want_type, "message": "boom"}}),
                "{status}"
            );
        }
        Ok(())
    }

    /// Go `http.StatusText` returns "" for unknown codes; so do we.
    #[test]
    fn http_status_text_unknown_code_is_empty() -> Result<(), Box<dyn StdError>> {
        let status = StatusCode::from_u16(599)?;
        assert_eq!(http_status_text(status), "");
        Ok(())
    }

    #[test]
    fn email_validator_accepts_gin_valid_addresses() {
        for ok in [
            "owner@example.com",
            "first.last@sub.domain.org",
            "a+tag@b.co",
        ] {
            assert!(is_valid_email(ok), "{ok}");
        }
    }

    #[test]
    fn email_validator_rejects_gin_invalid_addresses() {
        for bad in [
            "",        // required fails first in gin, email would too
            "no-at",   // no '@'
            "@x.y",    // empty local
            "a@",      // empty domain
            "a@b",     // Go regex requires a dotted domain tail
            "a b@c.d", // whitespace
            "a@b@c.d", // second '@'
            "a@-b.c",  // label starts with '-'
            "a@b..c",  // empty label
            "a@.b.c",  // leading dot label
        ] {
            assert!(!is_valid_email(bad), "{bad}");
        }
    }

    /// gin `min=6` counts runes: a 6-character CJK password passes even though
    /// it is 18 bytes.
    #[test]
    fn min_chars_counts_runes_not_bytes() {
        assert!(!min_chars("12345", 6));
        assert!(min_chars("123456", 6));
        assert!(min_chars("秘密口令密码", 6));
        assert!(!min_chars("密码只有五字", 7));
    }
}
