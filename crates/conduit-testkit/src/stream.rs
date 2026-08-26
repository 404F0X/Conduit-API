use serde_json::Value;

use crate::golden::GoldenSseEvent;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
    pub id: Option<String>,
    pub retry: Option<u64>,
    pub comments: Vec<String>,
}

impl SseEvent {
    pub fn data(data: impl Into<String>) -> Self {
        Self {
            event: None,
            data: data.into(),
            id: None,
            retry: None,
            comments: Vec::new(),
        }
    }

    pub fn named(event: impl Into<String>, data: impl Into<String>) -> Self {
        Self {
            event: Some(event.into()),
            data: data.into(),
            id: None,
            retry: None,
            comments: Vec::new(),
        }
    }

    pub fn is_done(&self) -> bool {
        self.data == "[DONE]"
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamComparison {
    pub expected: Vec<SseEvent>,
    pub actual: Vec<SseEvent>,
}

impl StreamComparison {
    pub fn assert_matches(&self) -> Result<(), String> {
        if self.expected.len() != self.actual.len() {
            return Err(format!(
                "SSE frame count mismatch: expected {}, actual {}",
                self.expected.len(),
                self.actual.len()
            ));
        }

        for (index, (expected, actual)) in self.expected.iter().zip(&self.actual).enumerate() {
            if expected != actual {
                return Err(format!(
                    "SSE frame {index} mismatch: expected {expected:?}, actual {actual:?}"
                ));
            }
        }

        Ok(())
    }
}

pub fn parse_sse(input: &str) -> Vec<SseEvent> {
    normalize_newlines(input)
        .split("\n\n")
        .filter_map(parse_frame)
        .collect()
}

pub fn compare_golden_sse_events(
    expected: &[GoldenSseEvent],
    actual: &[SseEvent],
) -> Result<(), String> {
    if expected.len() != actual.len() {
        return Err(format!(
            "SSE frame count mismatch: expected {}, actual {}",
            expected.len(),
            actual.len()
        ));
    }

    validate_done_sentinel(actual)?;

    for (index, (expected, actual)) in expected.iter().zip(actual).enumerate() {
        compare_event_type(index, expected, actual)?;
        compare_event_data(index, expected, actual)?;
        compare_event_comment(index, expected, actual)?;
    }

    Ok(())
}

pub fn compare_golden_sse_text(
    expected: &[GoldenSseEvent],
    actual_sse: &str,
) -> Result<(), String> {
    let actual = parse_sse(actual_sse);
    compare_golden_sse_events(expected, &actual)
}

fn compare_event_type(
    index: usize,
    expected: &GoldenSseEvent,
    actual: &SseEvent,
) -> Result<(), String> {
    if expected.event != actual.event {
        return Err(format!(
            "SSE frame {index} event mismatch: expected {:?}, actual {:?}",
            expected.event, actual.event
        ));
    }

    Ok(())
}

fn compare_event_data(
    index: usize,
    expected: &GoldenSseEvent,
    actual: &SseEvent,
) -> Result<(), String> {
    if expected.data == Value::Null {
        if actual.data.is_empty() {
            return Ok(());
        }

        return Err(format!(
            "SSE frame {index} data mismatch: expected no data, actual {:?}",
            actual.data
        ));
    }

    // P0-002 S03：golden data 是 JSON 对象/数组时，必须与 SSE data 做**结构比较**
    // 而非逐字节字符串比较。对象 key 顺序在 workspace 里是非确定的——取决于是否有
    // crate 启用了 `serde_json/preserve_order`（启用→IndexMap 插入序，未启用→BTreeMap
    // 排序序），feature unification 下同一份 JSON 序列化出的 key 序会随构建范围翻转。
    // `serde_json::Value` 的对象相等天然与 key 顺序无关（Map 的 PartialEq 按 key/value
    // 配对匹配，不关心顺序），恰好满足"忽略对象字段顺序"契约。
    if !expected.data.is_string() {
        match serde_json::from_str::<Value>(&actual.data) {
            Ok(actual_value) if expected.data == actual_value => return Ok(()),
            Ok(_) => {
                // 两边都是合法 JSON 但结构不等：报结构错（用规范化序列化展示期望值）。
                let want = serde_json::to_string(&expected.data)
                    .unwrap_or_else(|_| "<unencodable>".to_string());
                return Err(format!(
                    "SSE frame {index} data mismatch: expected {want:?}, actual {:?}",
                    actual.data
                ));
            }
            // actual 不是合法 JSON（如纯文本、`[DONE]`、多行拼接等）——回退到下面的
            // 原始字符串比较，保证非 JSON data 仍逐字节校验。
            Err(_) => {}
        }
    }

    let expected_data = match &expected.data {
        Value::String(data) => data.clone(),
        data => serde_json::to_string(data)
            .map_err(|error| format!("SSE frame {index} data JSON encode error: {error}"))?,
    };

    if expected_data != actual.data {
        return Err(format!(
            "SSE frame {index} data mismatch: expected {expected_data:?}, actual {:?}",
            actual.data
        ));
    }

    Ok(())
}

fn compare_event_comment(
    index: usize,
    expected: &GoldenSseEvent,
    actual: &SseEvent,
) -> Result<(), String> {
    match &expected.comment {
        Some(comment) if actual.comments.iter().any(|actual| actual == comment) => Ok(()),
        Some(comment) => Err(format!(
            "SSE frame {index} comment mismatch: expected heartbeat {comment:?}, actual {:?}",
            actual.comments
        )),
        None if actual.comments.is_empty() => Ok(()),
        None => Err(format!(
            "SSE frame {index} unexpected comments: {:?}",
            actual.comments
        )),
    }
}

fn validate_done_sentinel(actual: &[SseEvent]) -> Result<(), String> {
    let done_positions = actual
        .iter()
        .enumerate()
        .filter_map(|(index, event)| event.is_done().then_some(index))
        .collect::<Vec<_>>();

    match done_positions.as_slice() {
        [] => Err("SSE done sentinel missing".to_string()),
        [index] if *index == actual.len().saturating_sub(1) => Ok(()),
        [index] => Err(format!(
            "SSE done sentinel must be last frame, found at frame {index}"
        )),
        positions => Err(format!(
            "SSE done sentinel must appear once, found at frames {positions:?}"
        )),
    }
}

fn parse_frame(frame: &str) -> Option<SseEvent> {
    if frame.trim().is_empty() {
        return None;
    }

    let mut event = None;
    let mut data_lines = Vec::new();
    let mut id = None;
    let mut retry = None;
    let mut comments = Vec::new();

    for line in frame.lines() {
        if let Some(comment) = line.strip_prefix(':') {
            comments.push(trim_one_leading_space(comment).to_string());
            continue;
        }

        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, trim_one_leading_space(value)),
            None => (line, ""),
        };

        match field {
            "event" => event = Some(value.to_string()),
            "data" => data_lines.push(value.to_string()),
            "id" => id = Some(value.to_string()),
            "retry" => retry = value.parse::<u64>().ok(),
            _ => {}
        }
    }

    Some(SseEvent {
        event,
        data: data_lines.join("\n"),
        id,
        retry,
        comments,
    })
}

fn normalize_newlines(input: &str) -> String {
    input.replace("\r\n", "\n").replace('\r', "\n")
}

fn trim_one_leading_space(value: &str) -> &str {
    value.strip_prefix(' ').unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::golden::GoldenCase;

    #[test]
    fn parses_frames_and_multiline_data() {
        let events = parse_sse(": heartbeat\n\nevent: delta\ndata: one\ndata: two\n\n");

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].comments, vec!["heartbeat"]);
        assert_eq!(events[1].event.as_deref(), Some("delta"));
        assert_eq!(events[1].data, "one\ntwo");
    }

    #[test]
    fn detects_done_sentinel() {
        let events = parse_sse("data: [DONE]\n\n");
        assert!(events[0].is_done());
    }

    #[test]
    fn compares_valid_golden_sse_sequence() -> Result<(), Box<dyn std::error::Error>> {
        let golden = parse_streaming_case(
            r#"
            [
                {"comment": "heartbeat"},
                {"event": "message", "data": {"delta":"hello"}},
                {"data": "[DONE]"}
            ]
            "#,
        )?;

        compare_golden_sse_text(
            &golden,
            ": heartbeat\n\nevent: message\ndata: {\"delta\":\"hello\"}\n\ndata: [DONE]\n\n",
        )?;
        Ok(())
    }

    #[test]
    fn compares_error_event_sequence() -> Result<(), Box<dyn std::error::Error>> {
        let golden = parse_streaming_case(
            r#"
            [
                {"event": "error", "data": {"type":"upstream_error","message":"failed"}},
                {"data": "[DONE]"}
            ]
            "#,
        )?;

        compare_golden_sse_text(
            &golden,
            "event: error\ndata: {\"message\":\"failed\",\"type\":\"upstream_error\"}\n\ndata: [DONE]\n\n",
        )?;
        Ok(())
    }

    #[test]
    fn sse_sequence_order_mismatch_fails() -> Result<(), Box<dyn std::error::Error>> {
        let golden = parse_streaming_case(
            r#"
            [
                {"event": "message", "data": "first"},
                {"event": "message", "data": "second"},
                {"data": "[DONE]"}
            ]
            "#,
        )?;

        let res = compare_golden_sse_text(
            &golden,
            "event: message\ndata: second\n\nevent: message\ndata: first\n\ndata: [DONE]\n\n",
        );
        match res {
            Err(error) => assert!(error.contains("SSE frame 0 data mismatch")),
            Ok(v) => panic!("expected error, got {v:?}"),
        }
        Ok(())
    }

    #[test]
    fn done_sentinel_missing_or_not_last_fails() -> Result<(), Box<dyn std::error::Error>> {
        let golden = parse_streaming_case(
            r#"
            [
                {"event": "message", "data": "first"},
                {"data": "[DONE]"}
            ]
            "#,
        )?;

        let missing =
            compare_golden_sse_text(&golden, "event: message\ndata: first\n\ndata: second\n\n");
        match missing {
            Err(e) => {
                assert!(e.contains("SSE frame count mismatch") || e.contains("done sentinel"))
            }
            Ok(v) => panic!("expected error, got {v:?}"),
        }

        let wrong_position =
            compare_golden_sse_text(&golden, "data: [DONE]\n\nevent: message\ndata: first\n\n");
        match wrong_position {
            Err(e) => {
                assert!(e.contains("done sentinel") || e.contains("SSE frame 0 data mismatch"))
            }
            Ok(v) => panic!("expected error, got {v:?}"),
        }
        Ok(())
    }

    #[test]
    fn compares_comment_heartbeat() -> Result<(), Box<dyn std::error::Error>> {
        let golden = parse_streaming_case(
            r#"
            [
                {"comment": "keep-alive"},
                {"data": "[DONE]"}
            ]
            "#,
        )?;

        compare_golden_sse_text(&golden, ": keep-alive\n\ndata: [DONE]\n\n")?;
        Ok(())
    }

    fn parse_streaming_case(
        events: &str,
    ) -> Result<Vec<GoldenSseEvent>, Box<dyn std::error::Error>> {
        let case = format!(
            r#"{{
                "inbound_http": {{}},
                "unified_request": {{}},
                "selected_channel": {{}},
                "outbound_http": {{}},
                "upstream_http": {{}},
                "client_http": {{}},
                "events": {events}
            }}"#
        );

        let golden = GoldenCase::parse(&case)?;
        let events = golden.golden_sse_events()?;
        Ok(events)
    }
}
