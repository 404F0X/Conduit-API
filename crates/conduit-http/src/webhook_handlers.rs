use std::collections::BTreeMap;

use axum::{
    Json,
    body::Bytes,
    http::{HeaderMap, Method, Uri},
};
use serde::Serialize;
use serde_json::Value;

/// Webhook debugging response. Query fields retain their multi-valued form,
/// while headers are restricted to a non-sensitive allowlist so credentials
/// and proxy-internal routing data can never be reflected to a caller.
#[derive(Debug, Serialize, PartialEq)]
pub struct WebhookEchoResponse {
    pub method: String,
    pub path: String,
    pub query: BTreeMap<String, Vec<String>>,
    pub headers: BTreeMap<String, Vec<String>>,
    pub body: Value,
}

pub async fn webhook_echo(
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Json<WebhookEchoResponse> {
    Json(webhook_echo_response(
        &method,
        uri.path(),
        uri.query(),
        &headers,
        parse_echo_body(&body),
    ))
}

pub fn webhook_echo_response(
    method: &Method,
    path: impl Into<String>,
    query: Option<&str>,
    headers: &HeaderMap,
    body: Value,
) -> WebhookEchoResponse {
    WebhookEchoResponse {
        method: method.as_str().to_owned(),
        path: path.into(),
        query: parse_query_multi(query),
        headers: echo_safe_headers_multi(headers),
        body,
    }
}

/// Headers that are useful for webhook diagnostics and safe to reflect.
const SAFE_ECHO_HEADERS: &[&str] = &[
    "accept",
    "content-length",
    "content-type",
    "user-agent",
    "x-correlation-id",
    "x-request-id",
    "x-webhook-id",
    "x-webhook-timestamp",
];

fn echo_safe_headers_multi(headers: &HeaderMap) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (name, value) in headers.iter() {
        if !SAFE_ECHO_HEADERS.contains(&name.as_str()) {
            continue;
        }
        if let Ok(s) = value.to_str() {
            out.entry(canonical_mime_header(name.as_str()))
                .or_default()
                .push(s.to_owned());
        }
    }
    out
}

/// Parse a raw query string into a multi-valued map using standard URL form
/// decoding (`+` to space and `%XX` to UTF-8), mirroring Go `url.ParseQuery`.
fn parse_query_multi(query: Option<&str>) -> BTreeMap<String, Vec<String>> {
    let Some(q) = query else {
        return BTreeMap::new();
    };
    if q.is_empty() {
        return BTreeMap::new();
    }
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (key, value) in url::form_urlencoded::parse(q.as_bytes()) {
        out.entry(key.into_owned())
            .or_default()
            .push(value.into_owned());
    }
    out
}

/// Canonicalize a header name the way Go's `textproto.CanonicalMIMEHeaderKey`
/// does: the first letter of each hyphen-separated word upper-cased, the
/// remaining letters lower-cased (e.g. `content-type` → `Content-Type`,
/// `x-request-id` → `X-Request-Id`).
fn canonical_mime_header(name: &str) -> String {
    name.split('-')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => {
                    first.to_ascii_uppercase().to_string() + &chars.as_str().to_ascii_lowercase()
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join("-")
}

fn parse_echo_body(body: &[u8]) -> Value {
    if body.is_empty() {
        return Value::Null;
    }

    serde_json::from_slice(body)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(body).into_owned()))
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use axum::http::{HeaderMap, HeaderValue, Method, header};
    use serde_json::json;

    use super::*;

    #[test]
    fn echo_response_serializes_stable_shape_with_body_and_query() -> Result<(), Box<dyn Error>> {
        let headers = HeaderMap::new();
        let response = webhook_echo_response(
            &Method::POST,
            "/webhooks/echo",
            Some("topic=orders&attempt=2"),
            &headers,
            json!({
                "event": "order.created",
                "id": "evt_123"
            }),
        );

        let body = serde_json::to_value(response)?;

        assert_eq!(
            body,
            json!({
                "method": "POST",
                "path": "/webhooks/echo",
                "query": {
                    "topic": ["orders"],
                    "attempt": ["2"]
                },
                "headers": {},
                "body": {
                    "event": "order.created",
                    "id": "evt_123"
                }
            })
        );
        Ok(())
    }

    #[test]
    fn echo_response_only_echoes_allowlisted_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        headers.insert(header::ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert("x-request-id", HeaderValue::from_static("req_123"));
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer secret"),
        );
        headers.insert("x-api-key", HeaderValue::from_static("secret-key"));
        headers.insert("x-forwarded-for", HeaderValue::from_static("10.0.0.1"));
        // Multi-valued header (Go http.Header preserves all values).
        headers.append("x-multi", HeaderValue::from_static("a"));
        headers.append("x-multi", HeaderValue::from_static("b"));

        let response =
            webhook_echo_response(&Method::GET, "/webhooks/echo", None, &headers, Value::Null);

        // Safe values retain canonical keys; credentials, proxy routing, and
        // unknown fields are never reflected.
        assert_eq!(
            response.headers.get("Content-Type"),
            Some(&vec!["application/json".to_owned()])
        );
        assert_eq!(
            response.headers.get("X-Request-Id"),
            Some(&vec!["req_123".to_owned()])
        );
        assert!(response.headers.contains_key("Accept"));
        assert!(!response.headers.contains_key("Authorization"));
        assert!(!response.headers.contains_key("X-Api-Key"));
        assert!(!response.headers.contains_key("X-Forwarded-For"));
        assert!(!response.headers.contains_key("X-Multi"));
    }

    #[test]
    fn query_multi_valued_groups_repeated_keys() {
        let table = parse_query_multi(Some(
            "tag=a%2Fb&tag=hello+world&single=%E4%B8%AD%E6%96%87&empty=",
        ));
        assert_eq!(
            table.get("tag"),
            Some(&vec!["a/b".to_owned(), "hello world".to_owned()])
        );
        assert_eq!(table.get("single"), Some(&vec!["中文".to_owned()]));
        assert_eq!(table.get("empty"), Some(&vec![String::new()]));
        assert!(parse_query_multi(None).is_empty());
        assert!(parse_query_multi(Some("")).is_empty());
    }

    #[test]
    fn canonical_mime_header_matches_go_textproto() {
        assert_eq!(canonical_mime_header("content-type"), "Content-Type");
        assert_eq!(canonical_mime_header("x-request-id"), "X-Request-Id");
        assert_eq!(canonical_mime_header("user-agent"), "User-Agent");
        assert_eq!(canonical_mime_header("x-forwarded-for"), "X-Forwarded-For");
    }

    #[test]
    fn parse_echo_body_preserves_json_and_falls_back_to_text() {
        assert_eq!(parse_echo_body(br#"{"ok":true}"#), json!({ "ok": true }));
        assert_eq!(parse_echo_body(b"plain text"), json!("plain text"));
        assert_eq!(parse_echo_body(b""), Value::Null);
    }
}
