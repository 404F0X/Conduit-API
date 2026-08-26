use std::collections::BTreeMap;

use axum::{
    Json,
    body::Bytes,
    http::{HeaderMap, Method, Uri},
};
use serde::Serialize;
use serde_json::Value;

/// Mirrors Go `WebhookDebugResponse` (`conduit/internal/server/api/system.go:68-74`,
/// `:104-130`): echoes the inbound request verbatim — `method`, `path`,
/// multi-valued `query` (Go `url.Values` = `map[string][]string`), the
/// **full** `headers` set with no safe-subset filtering (Go writes
/// `c.Request.Header` verbatim at `system.go:115`), and the JSON `body`.
/// Response is forced to `Content-Type: application/json` (Go `system.go:127`).
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
        headers: echo_all_headers_multi(headers),
        body,
    }
}

/// Collect **every** inbound header into a multi-valued map keyed by the
/// canonical MIME header name (mirrors Go `http.Header`, whose keys are
/// produced by `textproto.CanonicalMIMEHeaderKey`). Go echoes the full
/// header set with no safe-subset filtering (`system.go:115`).
fn echo_all_headers_multi(headers: &HeaderMap) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (name, value) in headers.iter() {
        if let Ok(s) = value.to_str() {
            out.entry(canonical_mime_header(name.as_str()))
                .or_default()
                .push(s.to_owned());
        }
    }
    out
}

/// Parse a raw query string into a multi-valued map, mirroring Go
/// `url.ParseQuery` → `url.Values` (`map[string][]string`). `+` decodes to
/// space (form encoding); `%XX` is left to the caller's downstream decoder —
/// the wire contract is the raw form value as Go preserves it.
fn parse_query_multi(query: Option<&str>) -> BTreeMap<String, Vec<String>> {
    let Some(q) = query else {
        return BTreeMap::new();
    };
    if q.is_empty() {
        return BTreeMap::new();
    }
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for pair in q.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        out.entry(k.to_owned()).or_default().push(v.to_owned());
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
    fn echo_response_echoes_all_headers_multi_valued_canonical_case() {
        // Go echoes the full header set (no safe-subset) as map[string][]string
        // keyed by canonical MIME name. Authorization / x-api-key MUST appear
        // — Go does not filter them.
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
        // Multi-valued header (Go http.Header preserves all values).
        headers.append("x-multi", HeaderValue::from_static("a"));
        headers.append("x-multi", HeaderValue::from_static("b"));

        let response =
            webhook_echo_response(&Method::GET, "/webhooks/echo", None, &headers, Value::Null);

        // Full set, canonical keys, multi-valued.
        assert_eq!(
            response.headers.get("Content-Type"),
            Some(&vec!["application/json".to_owned()])
        );
        assert_eq!(
            response.headers.get("Authorization"),
            Some(&vec!["Bearer secret".to_owned()])
        );
        assert_eq!(
            response.headers.get("X-Multi"),
            Some(&vec!["a".to_owned(), "b".to_owned()])
        );
        assert_eq!(
            response.headers.get("X-Request-Id"),
            Some(&vec!["req_123".to_owned()])
        );
        assert!(response.headers.contains_key("Accept"));
    }

    #[test]
    fn query_multi_valued_groups_repeated_keys() {
        let table = parse_query_multi(Some("tag=a&tag=b&single=1&empty="));
        assert_eq!(
            table.get("tag"),
            Some(&vec!["a".to_owned(), "b".to_owned()])
        );
        assert_eq!(table.get("single"), Some(&vec!["1".to_owned()]));
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
