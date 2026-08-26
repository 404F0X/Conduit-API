use std::collections::BTreeMap;

use serde_json::Value;

use crate::fixtures::HeaderFixture;

pub const ALLOWED_PLACEHOLDERS: [&str; 4] = [
    "$ANY_TIMESTAMP",
    "$ANY_ID",
    "$ANY_TRACE_ID",
    "$ANY_THREAD_ID",
];

#[derive(Clone, Debug, PartialEq)]
pub struct GoldenCase {
    pub inbound_http: Value,
    pub unified_request: Value,
    pub selected_channel: Value,
    pub outbound_http: Value,
    pub upstream_http: Value,
    pub client_http: Value,
    pub events: Option<Vec<Value>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GoldenSseEvent {
    pub event: Option<String>,
    pub data: Value,
    pub comment: Option<String>,
}

impl GoldenCase {
    pub fn parse(input: &str) -> Result<Self, String> {
        parse_golden_case(input)
    }

    pub fn validate(&self) -> Result<(), String> {
        validate_placeholders("inbound_http", &self.inbound_http)?;
        validate_placeholders("unified_request", &self.unified_request)?;
        validate_placeholders("selected_channel", &self.selected_channel)?;
        validate_placeholders("outbound_http", &self.outbound_http)?;
        validate_placeholders("upstream_http", &self.upstream_http)?;
        validate_placeholders("client_http", &self.client_http)?;

        if let Some(events) = &self.events {
            for (index, event) in events.iter().enumerate() {
                validate_golden_event(index, event)?;
            }
        }

        Ok(())
    }

    pub fn golden_sse_events(&self) -> Result<Vec<GoldenSseEvent>, String> {
        self.events
            .as_deref()
            .unwrap_or_default()
            .iter()
            .enumerate()
            .map(|(index, event)| parse_golden_event(index, event))
            .collect()
    }
}

pub fn parse_golden_case(input: &str) -> Result<GoldenCase, String> {
    let value = serde_json::from_str::<Value>(input)
        .map_err(|error| format!("golden case JSON parse error: {error}"))?;
    let object = value
        .as_object()
        .ok_or_else(|| "golden case root must be a JSON object".to_string())?;

    let events = match object.get("events") {
        Some(Value::Array(events)) => Some(events.clone()),
        Some(_) => return Err("events must be an array when present".to_string()),
        None => None,
    };

    let golden = GoldenCase {
        inbound_http: required_section(object, "inbound_http")?.clone(),
        unified_request: required_section(object, "unified_request")?.clone(),
        selected_channel: required_section(object, "selected_channel")?.clone(),
        outbound_http: required_section(object, "outbound_http")?.clone(),
        upstream_http: required_section(object, "upstream_http")?.clone(),
        client_http: required_section(object, "client_http")?.clone(),
        events,
    };

    golden.validate()?;
    Ok(golden)
}

fn required_section<'a>(
    object: &'a serde_json::Map<String, Value>,
    name: &str,
) -> Result<&'a Value, String> {
    object
        .get(name)
        .ok_or_else(|| format!("golden case missing required section: {name}"))
}

fn validate_golden_event(index: usize, event: &Value) -> Result<(), String> {
    parse_golden_event(index, event)?;
    validate_placeholders(&format!("events[{index}]"), event)
}

fn parse_golden_event(index: usize, event: &Value) -> Result<GoldenSseEvent, String> {
    let object = event
        .as_object()
        .ok_or_else(|| format!("events[{index}] must be an object"))?;

    let event_type = match object.get("event") {
        Some(Value::String(event)) => Some(event.clone()),
        Some(_) => return Err(format!("events[{index}].event must be a string")),
        None => None,
    };
    let comment = match object.get("comment") {
        Some(Value::String(comment)) => Some(comment.clone()),
        Some(_) => return Err(format!("events[{index}].comment must be a string")),
        None => None,
    };
    let data = object.get("data").cloned().unwrap_or(Value::Null);

    Ok(GoldenSseEvent {
        event: event_type,
        data,
        comment,
    })
}

fn validate_placeholders(path: &str, value: &Value) -> Result<(), String> {
    match value {
        Value::String(value) if looks_like_placeholder(value) && !is_allowed_placeholder(value) => {
            Err(format!("{path}: unsupported placeholder {value}"))
        }
        Value::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                validate_placeholders(&format!("{path}[{index}]"), value)?;
            }
            Ok(())
        }
        Value::Object(values) => {
            for (key, value) in values {
                validate_placeholders(&format!("{path}.{key}"), value)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn looks_like_placeholder(value: &str) -> bool {
    value.starts_with("$ANY_")
}

fn is_allowed_placeholder(value: &str) -> bool {
    ALLOWED_PLACEHOLDERS.contains(&value)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsonComparisonOptions {
    pub placeholders: Vec<String>,
}

impl Default for JsonComparisonOptions {
    fn default() -> Self {
        Self {
            placeholders: ALLOWED_PLACEHOLDERS
                .iter()
                .map(|placeholder| (*placeholder).to_string())
                .collect(),
        }
    }
}

pub fn compare_json(
    expected: &str,
    actual: &str,
    options: &JsonComparisonOptions,
) -> Result<(), String> {
    let expected = serde_json::from_str::<Value>(expected)
        .map_err(|error| format!("expected JSON parse error: {error}"))?;
    let actual = serde_json::from_str::<Value>(actual)
        .map_err(|error| format!("actual JSON parse error: {error}"))?;

    compare_value("$", &expected, &actual, options)
}

fn compare_value(
    path: &str,
    expected: &Value,
    actual: &Value,
    options: &JsonComparisonOptions,
) -> Result<(), String> {
    if is_placeholder(expected, options) {
        return Ok(());
    }

    match (expected, actual) {
        (Value::Object(expected), Value::Object(actual)) => {
            if expected.len() != actual.len() {
                return Err(format!(
                    "{path}: object field count mismatch: expected {}, actual {}",
                    expected.len(),
                    actual.len()
                ));
            }

            for (key, expected_value) in expected {
                let actual_value = actual
                    .get(key)
                    .ok_or_else(|| format!("{path}.{key}: missing field"))?;
                compare_value(
                    &format!("{path}.{key}"),
                    expected_value,
                    actual_value,
                    options,
                )?;
            }

            Ok(())
        }
        (Value::Array(expected), Value::Array(actual)) => {
            if expected.len() != actual.len() {
                return Err(format!(
                    "{path}: array length mismatch: expected {}, actual {}",
                    expected.len(),
                    actual.len()
                ));
            }

            for (index, (expected_value, actual_value)) in expected.iter().zip(actual).enumerate() {
                compare_value(
                    &format!("{path}[{index}]"),
                    expected_value,
                    actual_value,
                    options,
                )?;
            }

            Ok(())
        }
        (Value::Number(expected), Value::Number(actual)) => {
            if numbers_equal(expected, actual) {
                Ok(())
            } else {
                Err(format!(
                    "{path}: number mismatch: expected {expected}, actual {actual}"
                ))
            }
        }
        _ if expected == actual => Ok(()),
        _ => Err(format!(
            "{path}: value mismatch: expected {expected}, actual {actual}"
        )),
    }
}

fn is_placeholder(value: &Value, options: &JsonComparisonOptions) -> bool {
    match value {
        Value::String(value) => options
            .placeholders
            .iter()
            .any(|placeholder| placeholder == value),
        _ => false,
    }
}

fn numbers_equal(expected: &serde_json::Number, actual: &serde_json::Number) -> bool {
    match (expected.as_f64(), actual.as_f64()) {
        (Some(expected), Some(actual)) => expected == actual,
        _ => expected == actual,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HeaderAssertion {
    MustExistMasked {
        name: String,
        scheme: Option<String>,
    },
    Exact {
        name: String,
        value: String,
    },
}

impl HeaderAssertion {
    pub fn masked(name: impl Into<String>) -> Self {
        Self::MustExistMasked {
            name: name.into(),
            scheme: None,
        }
    }

    pub fn masked_with_scheme(name: impl Into<String>, scheme: impl Into<String>) -> Self {
        Self::MustExistMasked {
            name: name.into(),
            scheme: Some(scheme.into()),
        }
    }

    pub fn exact(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self::Exact {
            name: name.into(),
            value: value.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeaderComparison {
    pub assertions: Vec<HeaderAssertion>,
}

impl HeaderComparison {
    pub fn new(assertions: Vec<HeaderAssertion>) -> Self {
        Self { assertions }
    }
}

pub fn compare_headers(
    expected: &HeaderComparison,
    actual: &[HeaderFixture],
) -> Result<(), String> {
    let actual = actual
        .iter()
        .map(|header| {
            (
                header.name.to_ascii_lowercase(),
                header.value.trim().to_string(),
            )
        })
        .collect::<BTreeMap<_, _>>();

    for assertion in &expected.assertions {
        match assertion {
            HeaderAssertion::MustExistMasked { name, scheme } => {
                let key = name.to_ascii_lowercase();
                let value = actual
                    .get(&key)
                    .ok_or_else(|| format!("missing required header: {key}"))?;
                if let Some(scheme) = scheme {
                    let actual_scheme = value.split_whitespace().next().unwrap_or_default();
                    if !actual_scheme.eq_ignore_ascii_case(scheme) {
                        return Err(format!(
                            "header {key}: scheme mismatch: expected {scheme}, actual {actual_scheme}"
                        ));
                    }
                }
            }
            HeaderAssertion::Exact { name, value } => {
                let key = name.to_ascii_lowercase();
                let actual_value = actual
                    .get(&key)
                    .ok_or_else(|| format!("missing required header: {key}"))?;
                if actual_value != value.trim() {
                    return Err(format!("header {key}: exact value mismatch"));
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_ignores_object_order_but_not_array_order() -> Result<(), Box<dyn std::error::Error>> {
        let options = JsonComparisonOptions::default();

        compare_json(r#"{"b":2,"a":1}"#, r#"{"a":1.0,"b":2}"#, &options)?;

        let diff = compare_json(r#"[1,2]"#, r#"[2,1]"#, &options);
        assert!(diff.is_err());
        Ok(())
    }

    #[test]
    fn header_keys_are_case_insensitive_and_auth_value_is_masked()
    -> Result<(), Box<dyn std::error::Error>> {
        let expected = HeaderComparison::new(vec![
            HeaderAssertion::masked_with_scheme("Authorization", "Bearer"),
            HeaderAssertion::exact("Content-Type", "application/json"),
        ]);
        let actual = vec![
            HeaderFixture::new("authorization", "Bearer secret"),
            HeaderFixture::new("content-type", "application/json"),
        ];

        compare_headers(&expected, &actual)?;
        Ok(())
    }

    #[test]
    fn parses_valid_golden_case() -> Result<(), Box<dyn std::error::Error>> {
        let golden = GoldenCase::parse(valid_case())?;

        assert_eq!(golden.inbound_http["method"], "POST");
        let events = golden.events.as_ref().ok_or("expected events")?;
        assert_eq!(events.len(), 2);
        Ok(())
    }

    #[test]
    fn event_schema_requires_object_with_supported_field_shapes()
    -> Result<(), Box<dyn std::error::Error>> {
        GoldenCase::parse(
            r#"{
                "inbound_http": {},
                "unified_request": {},
                "selected_channel": {},
                "outbound_http": {},
                "upstream_http": {},
                "client_http": {},
                "events": [
                    {"event": "message", "data": {"delta": "ok"}},
                    {"comment": "heartbeat"}
                ]
            }"#,
        )?;

        let res = GoldenCase::parse(
            r#"{
                "inbound_http": {},
                "unified_request": {},
                "selected_channel": {},
                "outbound_http": {},
                "upstream_http": {},
                "client_http": {},
                "events": ["data: nope"]
            }"#,
        );
        match res {
            Err(error) => assert!(error.contains("events[0] must be an object")),
            Ok(v) => panic!("expected error, got {v:?}"),
        }
        Ok(())
    }

    #[test]
    fn missing_required_section_fails() {
        let res = GoldenCase::parse(
            r#"{
                "inbound_http": {},
                "unified_request": {},
                "selected_channel": {},
                "outbound_http": {},
                "upstream_http": {}
            }"#,
        );
        match res {
            Err(error) => assert!(error.contains("client_http")),
            Ok(v) => panic!("expected error, got {v:?}"),
        }
    }

    #[test]
    fn event_array_order_is_preserved() -> Result<(), Box<dyn std::error::Error>> {
        let golden = GoldenCase::parse(valid_case())?;
        let events = golden.events.ok_or("expected events")?;

        assert_eq!(events[0]["data"], "first");
        assert_eq!(events[1]["data"], "second");
        Ok(())
    }

    #[test]
    fn placeholder_validation_is_recursive() -> Result<(), Box<dyn std::error::Error>> {
        GoldenCase::parse(
            r#"{
                "inbound_http": {"headers": {"x-request-id": "$ANY_ID"}},
                "unified_request": {"metadata": [{"created_at": "$ANY_TIMESTAMP"}]},
                "selected_channel": {"trace_id": "$ANY_TRACE_ID"},
                "outbound_http": {},
                "upstream_http": {},
                "client_http": {},
                "events": [{"data": {"thread": "$ANY_THREAD_ID"}}]
            }"#,
        )?;

        let res = GoldenCase::parse(
            r#"{
                "inbound_http": {"headers": {"x-request-id": "$ANY_RANDOM"}},
                "unified_request": {},
                "selected_channel": {},
                "outbound_http": {},
                "upstream_http": {},
                "client_http": {}
            }"#,
        );
        match res {
            Err(error) => assert!(error.contains("unsupported placeholder $ANY_RANDOM")),
            Ok(v) => panic!("expected error, got {v:?}"),
        }
        Ok(())
    }

    fn valid_case() -> &'static str {
        r#"{
            "inbound_http": {
                "method": "POST",
                "path": "/v1/chat/completions",
                "headers": {"authorization": "Bearer $ANY_ID"}
            },
            "unified_request": {
                "request_id": "$ANY_ID",
                "created_at": "$ANY_TIMESTAMP"
            },
            "selected_channel": {
                "id": "$ANY_ID",
                "trace_id": "$ANY_TRACE_ID"
            },
            "outbound_http": {
                "method": "POST"
            },
            "upstream_http": {
                "status": 200
            },
            "client_http": {
                "status": 200
            },
            "events": [
                {"event": "delta", "data": "first"},
                {"event": "delta", "data": "second"}
            ]
        }"#
    }
}
