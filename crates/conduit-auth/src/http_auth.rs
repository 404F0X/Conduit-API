use serde::{Deserialize, Serialize};

pub const API_KEY_HEADER: &str = "x-api-key";
pub const GOOGLE_API_KEY_HEADER: &str = "x-goog-api-key";
pub const NO_AUTH_SENTINEL: &str = "CONDUIT_API_KEY_NO_AUTH";

pub type AuthExtractionResult = Result<ExtractedApiKey, AuthFailure>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiKeyHeader {
    XApiKey,
    XGoogApiKey,
}

impl ApiKeyHeader {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::XApiKey => API_KEY_HEADER,
            Self::XGoogApiKey => GOOGLE_API_KEY_HEADER,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiKeySource {
    AuthorizationBearer,
    Header { name: ApiKeyHeader },
    QueryKey,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractedApiKey {
    pub value: String,
    pub source: ApiKeySource,
    pub is_no_auth_sentinel: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum AuthFailure {
    Missing,
    Malformed {
        source: ApiKeySource,
        message: &'static str,
    },
    Unsupported {
        scheme: String,
    },
}

// NOTE: Implemented manually (not via `thiserror::Error`) because thiserror
// treats a field named `source` as the chained error source (requiring
// `ApiKeySource: Error`); a manual impl avoids that while still providing
// `Display` + `Error` so test code can use `?` into `Box<dyn Error>`.
impl std::fmt::Display for AuthFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => formatter.write_str("auth credential missing"),
            Self::Malformed { source, message } => {
                write!(formatter, "malformed {source:?} auth credential: {message}")
            }
            Self::Unsupported { scheme } => {
                write!(formatter, "unsupported auth scheme: {scheme}")
            }
        }
    }
}

impl std::error::Error for AuthFailure {}

impl ExtractedApiKey {
    fn new(value: &str, source: ApiKeySource) -> Self {
        let value = value.trim().to_string();
        Self {
            is_no_auth_sentinel: value == NO_AUTH_SENTINEL,
            value,
            source,
        }
    }
}

pub fn extract_api_key<'a>(
    headers: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> AuthExtractionResult {
    extract_from_headers(headers)
}

pub fn extract_gemini_api_key<'a>(
    query_params: impl IntoIterator<Item = (&'a str, &'a str)>,
    headers: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> AuthExtractionResult {
    if let Some(value) = query_params
        .into_iter()
        .find_map(|(name, value)| (name == "key").then_some(value))
    {
        return extract_raw_key(value, ApiKeySource::QueryKey);
    }

    extract_from_headers(headers)
}

fn extract_from_headers<'a>(
    headers: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> AuthExtractionResult {
    let mut x_api_key = None;
    let mut x_goog_api_key = None;

    for (name, value) in headers {
        if name.eq_ignore_ascii_case("authorization") {
            // A malformed Authorization header should not be bypassed by a fallback key header.
            return extract_authorization_bearer(value);
        }

        if name.eq_ignore_ascii_case(API_KEY_HEADER) {
            x_api_key.get_or_insert(value);
            continue;
        }

        if name.eq_ignore_ascii_case(GOOGLE_API_KEY_HEADER) {
            x_goog_api_key.get_or_insert(value);
        }
    }

    if let Some(value) = x_api_key {
        return extract_raw_key(
            value,
            ApiKeySource::Header {
                name: ApiKeyHeader::XApiKey,
            },
        );
    }

    if let Some(value) = x_goog_api_key {
        return extract_raw_key(
            value,
            ApiKeySource::Header {
                name: ApiKeyHeader::XGoogApiKey,
            },
        );
    }

    Err(AuthFailure::Missing)
}

fn extract_authorization_bearer(value: &str) -> AuthExtractionResult {
    let value = value.trim();
    if value.is_empty() {
        return Err(AuthFailure::Malformed {
            source: ApiKeySource::AuthorizationBearer,
            message: "authorization header is empty",
        });
    }

    let mut parts = value.split_whitespace();
    let Some(scheme) = parts.next() else {
        return Err(AuthFailure::Malformed {
            source: ApiKeySource::AuthorizationBearer,
            message: "authorization scheme is missing",
        });
    };

    if !scheme.eq_ignore_ascii_case("bearer") {
        return Err(AuthFailure::Unsupported {
            scheme: scheme.to_string(),
        });
    }

    let Some(token) = parts.next() else {
        return Err(AuthFailure::Malformed {
            source: ApiKeySource::AuthorizationBearer,
            message: "bearer token is missing",
        });
    };

    if parts.next().is_some() {
        return Err(AuthFailure::Malformed {
            source: ApiKeySource::AuthorizationBearer,
            message: "bearer token contains whitespace",
        });
    }

    extract_raw_key(token, ApiKeySource::AuthorizationBearer)
}

fn extract_raw_key(value: &str, source: ApiKeySource) -> AuthExtractionResult {
    if value.trim().is_empty() {
        return Err(AuthFailure::Malformed {
            source,
            message: "api key is empty",
        });
    }

    Ok(ExtractedApiKey::new(value, source))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_authorization_bearer_case_insensitively() -> Result<(), Box<dyn std::error::Error>>
    {
        let extracted = extract_api_key([("authorization", "bearer key-1")])?;

        assert_eq!(extracted.value, "key-1");
        assert_eq!(extracted.source, ApiKeySource::AuthorizationBearer);
        assert!(!extracted.is_no_auth_sentinel);
        Ok(())
    }

    #[test]
    fn extracts_api_key_header_case_insensitively() -> Result<(), Box<dyn std::error::Error>> {
        let lower = extract_api_key([("x-api-key", "key-lower")])?;
        let mixed = extract_api_key([("X-Api-Key", "key-mixed")])?;
        let upper = extract_api_key([("X-API-Key", "key-upper")])?;

        assert_eq!(lower.value, "key-lower");
        assert_eq!(mixed.value, "key-mixed");
        assert_eq!(upper.value, "key-upper");
        assert_eq!(
            mixed.source,
            ApiKeySource::Header {
                name: ApiKeyHeader::XApiKey
            }
        );
        Ok(())
    }

    #[test]
    fn extracts_google_api_key_header_case_insensitively() -> Result<(), Box<dyn std::error::Error>>
    {
        let extracted = extract_api_key([("x-goog-api-key", "google-key")])?;

        assert_eq!(extracted.value, "google-key");
        assert_eq!(
            extracted.source,
            ApiKeySource::Header {
                name: ApiKeyHeader::XGoogApiKey
            }
        );
        Ok(())
    }

    #[test]
    fn gemini_query_key_wins_over_headers() -> Result<(), Box<dyn std::error::Error>> {
        let extracted = extract_gemini_api_key(
            [("key", "query-key")],
            [
                ("Authorization", "Bearer bearer-key"),
                ("X-Goog-Api-Key", "google-header-key"),
            ],
        )?;

        assert_eq!(extracted.value, "query-key");
        assert_eq!(extracted.source, ApiKeySource::QueryKey);
        Ok(())
    }

    #[test]
    fn gemini_header_fallback_when_query_key_missing() -> Result<(), Box<dyn std::error::Error>> {
        let extracted = extract_gemini_api_key(
            [("model", "gemini-pro")],
            [("X-Goog-Api-Key", "google-header-key")],
        )?;

        assert_eq!(extracted.value, "google-header-key");
        assert_eq!(
            extracted.source,
            ApiKeySource::Header {
                name: ApiKeyHeader::XGoogApiKey
            }
        );
        Ok(())
    }

    #[test]
    fn recognizes_no_auth_sentinel_without_allowing_it() -> Result<(), Box<dyn std::error::Error>> {
        let extracted = extract_api_key([("Authorization", "Bearer CONDUIT_API_KEY_NO_AUTH")])?;

        assert_eq!(extracted.value, NO_AUTH_SENTINEL);
        assert!(extracted.is_no_auth_sentinel);
        Ok(())
    }

    #[test]
    fn distinguishes_missing_malformed_and_unsupported() {
        assert_eq!(extract_api_key([]), Err(AuthFailure::Missing));

        assert_eq!(
            extract_api_key([("Authorization", "Bearer")]),
            Err(AuthFailure::Malformed {
                source: ApiKeySource::AuthorizationBearer,
                message: "bearer token is missing",
            })
        );

        assert_eq!(
            extract_api_key([("Authorization", "Basic abc")]),
            Err(AuthFailure::Unsupported {
                scheme: "Basic".to_string(),
            })
        );
    }
}
