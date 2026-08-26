//! Error response objects ported from `conduit/internal/objects/response.go`.

use serde::{Deserialize, Serialize};

/// Ported 1:1 from Go `ErrorResponse`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ErrorResponse {
    #[serde(default)]
    pub error: Error,
}

/// Ported 1:1 from Go `Error`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Error {
    #[serde(default, rename = "type")]
    pub r#type: String,
    #[serde(default)]
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_response_round_trip() -> Result<(), serde_json::Error> {
        let input = r#"{"error":{"type":"invalid_request_error","message":"model is required"}}"#;
        let resp: ErrorResponse = serde_json::from_str(input)?;
        assert_eq!(resp.error.r#type, "invalid_request_error");
        assert_eq!(resp.error.message, "model is required");

        let re = serde_json::to_value(&resp)?;
        assert_eq!(
            re.get("error")
                .and_then(|v| v.get("type"))
                .and_then(|v| v.as_str()),
            Some("invalid_request_error")
        );
        Ok(())
    }
}
