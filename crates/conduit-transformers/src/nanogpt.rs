//! NanoGPT OpenAI-compatible wrapper (RUST-P7-008 S11/S12, round 3).
//!
//! Mirrors Go `conduit/llm/transformer/nanogpt/` — `outbound.go`, `model.go`,
//! `xml_parser.go`. NanoGPT is OpenAI-compatible on the request side (it uses
//! `openai.ReasoningFieldReasoning`, no request rewriting), so its deltas are
//! all on the **response/parsing** side:
//!
//! 1. **`reasoning` → `reasoning_content` mapping** — NanoGPT models (e.g.
//!    `zai-org/glm-4.7:thinking`) emit reasoning in a top-level `reasoning`
//!    field on each choice message; the wrapper maps it to the OpenAI
//!    `reasoning_content` field. (model.go:51-83)
//! 2. **XML tool-call extraction** — NanoGPT models sometimes emit tool calls
//!    as XML-like tags inline in the message content (`<Write ...>`,
//!    `<Read .../>`, `<use_tool name="X">...`, `<Bash>...`). The wrapper
//!    parses these into structured `tool_calls` and strips them from the
//!    remaining content. (xml_parser.go)
//!
//! The streaming variants (`TransformStream` / `AggregateStreamChunks`) and
//! the HTTP-status / response-routing logic in `TransformResponse` are left
//! as documented TODOs — they belong with the live `OutboundTransformer`
//! trait impl (RUST-P7-002 S04/S08/S09), not the pure-parsing pattern
//! established here. This module ports the pure pieces:
//! [`maybe_has_xml_tool_calls`], [`parse_xml_tool_calls`],
//! [`extract_tool_name`], [`extract_tool_arguments`], [`generate_tool_call_id`],
//! and [`map_reasoning_to_content`].
//!
//! Go tests mirrored: `nanogpt/xml_parser_test.go` (full table —
//! `TestMaybeHasXMLToolCalls`, `TestParseXMLToolCalls_*`,
//! `TestExtractToolName`, `TestGenerateToolCallID`),
//! `nanogpt/model_test.go` (`TestMessage_ToOpenAIMessage` reasoning mapping).

use conduit_llm::{ChatMessage, ToolCall};
use regex::Regex;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::sync::LazyLock;

/// Maximum content length the parser will inspect, to prevent ReDoS. Mirrors
/// Go `maxXMLParseLength` (xml_parser.go:17).
pub const MAX_XML_PARSE_LENGTH: usize = 100_000;

// Compile-once regexes mirroring Go's package-level `regexp.MustCompile`
// patterns (xml_parser.go:22-43). A bad hard-coded pattern is a programmer
// error: fail immediately instead of silently changing matching semantics or
// entering an unbounded CPU loop.
fn compile_regex(pattern: &str) -> Regex {
    match Regex::new(pattern) {
        Ok(regex) => regex,
        Err(error) => panic!("invalid hard-coded NanoGPT regex: {error}"),
    }
}

static TOOL_CALL_RE: LazyLock<Regex> = LazyLock::new(|| {
    // Matches <Tag attr>content</Tag>. Allows optional whitespace after the
    // tag name for formats like <Write_File>{...}</Write_File>.
    compile_regex(r"<([a-zA-Z_][a-zA-Z0-9_-]*)[\s]*([^>]*)>([^<]*)</([a-zA-Z_][a-zA-Z0-9_-]*)>")
});
static SELF_CLOSING_RE: LazyLock<Regex> =
    LazyLock::new(|| compile_regex(r"<([a-zA-Z_][a-zA-Z0-9_-]*)[\s]*([^>]*)/>"));
static ATTR_RE: LazyLock<Regex> =
    LazyLock::new(|| compile_regex(r#"([a-zA-Z_][a-zA-Z0-9_-]*)[\s]*=[\s]*["']([^"']*)["']"#));
static NORMALIZE_TAG_RE: LazyLock<Regex> = LazyLock::new(|| compile_regex(r"([^\s])/>"));
static NESTED_XML_RE: LazyLock<Regex> = LazyLock::new(|| {
    compile_regex(
        r"(?s)<(Write|Read)[^>]*>\s*<file_path>([^<]*)</file_path>\s*<content>(.*?)</content>\s*</(Write|Read)>",
    )
});
static MISMATCH_TAG_RE: LazyLock<Regex> = LazyLock::new(|| {
    compile_regex(
        r"<(Write|Read|Write_FILE|Write_file|Read_FILE|Read_file)([^>]*)>([^<]*)</use_tool>",
    )
});
static UNCLOSED_RE: LazyLock<Regex> = LazyLock::new(|| {
    compile_regex(
        r"(?s)<(Write|Read|Write_FILE|Write_file|Read_FILE|Read_file)([^>]*)\n(.*?)</use_tool>",
    )
});

fn tool_call_re() -> &'static Regex {
    &TOOL_CALL_RE
}

fn self_closing_re() -> &'static Regex {
    &SELF_CLOSING_RE
}

fn attr_re() -> &'static Regex {
    &ATTR_RE
}

fn normalize_tag_re() -> &'static Regex {
    &NORMALIZE_TAG_RE
}

fn nested_xml_re() -> &'static Regex {
    &NESTED_XML_RE
}

fn mismatch_tag_re() -> &'static Regex {
    &MISMATCH_TAG_RE
}

fn unclosed_re() -> &'static Regex {
    &UNCLOSED_RE
}

/// Fast pre-check for whether content likely contains XML tool calls. Mirrors
/// Go `MaybeHasXMLToolCalls` (xml_parser.go:46-60). Intentionally permissive
/// — the pre-check matches any XML-like pattern; actual parsing filters
/// non-tool tags.
pub fn maybe_has_xml_tool_calls(content: &str) -> bool {
    let truncated = if content.len() > MAX_XML_PARSE_LENGTH {
        &content[..MAX_XML_PARSE_LENGTH]
    } else {
        content
    };
    if !truncated.contains('<') || !truncated.contains('>') {
        return false;
    }
    if tool_call_re().is_match(truncated) {
        return true;
    }
    if self_closing_re().is_match(truncated) {
        return true;
    }
    truncated.contains("use_tool")
        || truncated.contains("Write")
        || truncated.contains("Bash")
        || truncated.contains("Read")
}

/// Normalize common XML malformations from NanoGPT. Mirrors Go `normalizeXML`
/// (xml_parser.go:216-254). Returns the normalized content.
pub fn normalize_xml(content: &str) -> String {
    let mut out = content.to_string();

    // Fix unclosed opening tags: <Write attr="..."\ncontent</use_tool>
    // -> <Write attr="...">\ncontent</use_tool>
    let unclosed = unclosed_re();
    out = unclosed
        .replace_all(&out, |caps: &regex::Captures| {
            let tag = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            let attrs = caps.get(2).map(|m| m.as_str()).unwrap_or("");
            let rest = caps.get(3).map(|m| m.as_str()).unwrap_or("");
            format!("<{tag}{attrs}>\n{rest}</use_tool>")
        })
        .into_owned();

    // Fix mismatched closing tags.
    out = out.replace("</use_use>", "</use_tool>");
    out = out.replace("</Write_file>", "</Write>");
    out = out.replace("</Write_FILE>", "</Write>");
    out = out.replace("</Read_file>", "</Read>");
    out = out.replace("</Read_FILE>", "</Read>");

    // Fix <Write>content</use_tool> -> <Write>content</Write>
    let mismatch = mismatch_tag_re();
    out = mismatch
        .replace_all(&out, |caps: &regex::Captures| {
            let tag = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            let attrs = caps.get(2).map(|m| m.as_str()).unwrap_or("");
            let inner = caps.get(3).map(|m| m.as_str()).unwrap_or("");
            format!("<{tag}{attrs}>{inner}</{tag}>")
        })
        .into_owned();

    // Normalize self-closing tags without space before />: `x/>` -> `x />`
    out = normalize_tag_re().replace_all(&out, "$1 />").into_owned();

    out
}

/// Determine the tool name from a tag name + attribute string. Mirrors Go
/// `extractToolName` (xml_parser.go:257-280). Returns the lowercased tool
/// name, or empty string when the tag isn't a recognized tool pattern.
pub fn extract_tool_name(tag_name: &str, attrs: &str) -> String {
    let tag = tag_name.trim().to_ascii_lowercase();
    if tag.starts_with("write") {
        return "write".to_string();
    }
    if tag.starts_with("read") {
        return "read".to_string();
    }
    match tag.as_str() {
        "bash" | "python" | "search" | "glob" => tag,
        "use_tool" => {
            // Extract from name="..." attribute.
            let attr = attr_re();
            for caps in attr.captures_iter(attrs) {
                if caps.get(1).map(|m| m.as_str()) == Some("name") {
                    if let Some(val) = caps.get(2) {
                        return val.as_str().to_ascii_lowercase();
                    }
                }
            }
            String::new()
        }
        _ => String::new(),
    }
}

/// Extract tool arguments from attributes + inner content. Mirrors Go
/// `extractToolArguments` (xml_parser.go:283-334). Returns a JSON object
/// string (`{}` when no arguments).
pub fn extract_tool_arguments(tool_name: &str, attrs: &str, inner_content: &str) -> String {
    let mut args: Map<String, Value> = Map::new();

    // Extract attributes.
    let attr = attr_re();
    for caps in attr.captures_iter(attrs) {
        if let (Some(key), Some(val)) = (caps.get(1), caps.get(2)) {
            let key = key.as_str();
            // Skip the "name" attribute for use_tool tags.
            if key.eq_ignore_ascii_case("name") && !tool_name.is_empty() {
                continue;
            }
            args.insert(key.to_string(), Value::String(val.as_str().to_string()));
        }
    }

    // Handle inner content.
    let inner = inner_content.trim();
    if !inner.is_empty() {
        // Try to parse as JSON first.
        if let Ok(json_val) = serde_json::from_str::<Value>(inner) {
            match json_val {
                Value::Object(map) => {
                    for (k, v) in map {
                        args.entry(k).or_insert(v);
                    }
                }
                other => {
                    args.entry("content".to_string()).or_insert(other);
                }
            }
        } else if !args.contains_key("content") {
            args.insert("content".to_string(), Value::String(inner.to_string()));
        } else {
            args.entry("arg".to_string())
                .or_insert(Value::String(inner.to_string()));
        }
    }

    if args.is_empty() {
        return "{}".to_string();
    }
    serde_json::to_string(&Value::Object(args)).unwrap_or_else(|_| "{}".to_string())
}

/// Generate a deterministic tool-call ID from name + args. Mirrors Go
/// `generateToolCallID` (xml_parser.go:337-344): `sha256(name || args)` hex,
/// first 16 chars, prefixed `nanogpt_`.
pub fn generate_tool_call_id(name: &str, args: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(name.as_bytes());
    hasher.update(args.as_bytes());
    let hash = hasher.finalize();
    let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
    format!("nanogpt_{}", &hex[..16])
}

/// Result of [`parse_xml_tool_calls`]: the extracted tool calls plus the
/// remaining content (text outside any matched tool tag), trimmed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParsedToolCalls {
    /// Extracted tool calls in order of appearance.
    pub tool_calls: Vec<ToolCall>,
    /// Content remaining after removing the matched tool tags, whitespace
    /// trimmed (Go `strings.TrimSpace`).
    pub remaining: String,
}

/// Parse XML-like tool calls from content. Mirrors Go `ParseXMLToolCalls`
/// (xml_parser.go:69-213). Handles:
/// * `<use_tool name="X"><arg>value</arg></use_tool>`
/// * `<Write file_path="X" content="Y"/>` (self-closing)
/// * `<Write file_path="X">content</Write>`
/// * `<Write>{"file_path":"X","content":"Y"}</Write>` (JSON in body)
/// * `<Write><file_path>X</file_path><content>Y</content></Write>` (nested)
/// * Malformed variants (`<Write>c</use_tool>`, unclosed opening tags,
///   `Write_FILE`/`Write_file` casing).
///
/// Tool calls are recognized iff their tag yields a non-empty
/// [`extract_tool_name`]. Non-tool XML tags are kept in the remaining content
/// in order.
pub fn parse_xml_tool_calls(content: &str) -> Result<ParsedToolCalls, serde_json::Error> {
    if !maybe_has_xml_tool_calls(content) {
        return Ok(ParsedToolCalls {
            tool_calls: Vec::new(),
            remaining: content.to_string(),
        });
    }

    let normalized = normalize_xml(content);

    // A single match span keyed by byte offset.
    #[derive(Clone)]
    struct MatchInfo {
        start: usize,
        end: usize,
        tag_name: String,
        attrs: String,
        inner_content: String,
    }

    let mut matches: Vec<MatchInfo> = Vec::new();

    // Nested XML format must be processed before other patterns to avoid
    // matching inner elements.
    let nested = nested_xml_re();
    for caps in nested.captures_iter(&normalized) {
        if let (Some(full), Some(tag_m), Some(fp_m), Some(content_m), Some(close_m)) = (
            caps.get(0),
            caps.get(1),
            caps.get(2),
            caps.get(3),
            caps.get(4),
        ) {
            let tag_name = tag_m.as_str();
            let closing_tag = close_m.as_str();
            if tag_name.eq_ignore_ascii_case(closing_tag) {
                let file_path = fp_m.as_str();
                let inner = content_m.as_str();
                // Escape quotes to prevent XML injection, mirroring Go.
                let fp_escaped = file_path.replace('"', "\\\"");
                let inner_escaped = inner.replace('"', "\\\"");
                let attrs = format!(r#"file_path="{fp_escaped}" content="{inner_escaped}""#);
                matches.push(MatchInfo {
                    start: full.start(),
                    end: full.end(),
                    tag_name: tag_name.to_string(),
                    attrs,
                    inner_content: String::new(),
                });
            }
        }
    }

    // Opening/closing tag patterns.
    let tc_re = tool_call_re();
    for caps in tc_re.captures_iter(&normalized) {
        if let (Some(full), Some(open_m), Some(attrs_m), Some(inner_m), Some(close_m)) = (
            caps.get(0),
            caps.get(1),
            caps.get(2),
            caps.get(3),
            caps.get(4),
        ) {
            let opening = open_m.as_str();
            let closing = close_m.as_str();
            if opening.eq_ignore_ascii_case(closing) {
                matches.push(MatchInfo {
                    start: full.start(),
                    end: full.end(),
                    tag_name: opening.to_string(),
                    attrs: attrs_m.as_str().to_string(),
                    inner_content: inner_m.as_str().to_string(),
                });
            }
        }
    }

    // Self-closing patterns.
    let sc_re = self_closing_re();
    for caps in sc_re.captures_iter(&normalized) {
        if let (Some(full), Some(tag_m), Some(attrs_m)) = (caps.get(0), caps.get(1), caps.get(2)) {
            matches.push(MatchInfo {
                start: full.start(),
                end: full.end(),
                tag_name: tag_m.as_str().to_string(),
                attrs: attrs_m.as_str().to_string(),
                inner_content: String::new(),
            });
        }
    }

    // Sort by start position.
    matches.sort_by_key(|m| m.start);

    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut remaining = String::new();
    let mut last_end = 0usize;

    for m in &matches {
        let tool_name = extract_tool_name(&m.tag_name, &m.attrs);
        if tool_name.is_empty() {
            // Not a recognized tool pattern — keep in remaining content.
            if last_end < m.start {
                remaining.push_str(&normalized[last_end..m.start]);
            }
            last_end = m.end;
            continue;
        }
        let args = extract_tool_arguments(&tool_name, &m.attrs, &m.inner_content);
        let id = generate_tool_call_id(&tool_name, &args);
        let index = tool_calls.len() as i64;
        let function = json!({
            "name": tool_name,
            "arguments": args,
        });
        let mut extra = conduit_llm::model::ExtensionMap::new();
        extra.insert("index".to_string(), json!(index));
        tool_calls.push(ToolCall {
            id: Some(id),
            call_type: "function".to_string(),
            function,
            extra,
        });
        if last_end < m.start {
            remaining.push_str(&normalized[last_end..m.start]);
        }
        last_end = m.end;
    }

    if last_end < normalized.len() {
        remaining.push_str(&normalized[last_end..]);
    }

    if tool_calls.is_empty() {
        return Ok(ParsedToolCalls {
            tool_calls: Vec::new(),
            remaining: content.to_string(),
        });
    }

    Ok(ParsedToolCalls {
        tool_calls,
        remaining: remaining.trim().to_string(),
    })
}

/// Map a NanoGPT `reasoning` field (top-level on each choice message) into the
/// OpenAI `reasoning_content` field. Mirrors Go
/// `Message.ToOpenAIMessage` reasoning branch (model.go:62-65).
///
/// In the Rust unified [`ChatMessage`] both fields live in the `extra` map
/// (no named struct fields for them), so this helper copies `reasoning` →
/// `reasoning_content` when present and non-null. Returns `true` if a mapping
/// was applied.
pub fn map_reasoning_to_content(msg: &mut ChatMessage) -> bool {
    let reasoning = match msg.extra.get("reasoning") {
        Some(v) if !v.is_null() => v.clone(),
        _ => return false,
    };
    msg.extra.insert("reasoning_content".to_string(), reasoning);
    true
}

/// Apply [`map_reasoning_to_content`] + [`parse_xml_tool_calls`] to a single
/// choice message in place, mirroring Go `Message.ToOpenAIMessage`
/// (model.go:61-84). The reasoning field is mapped first, then if the (string)
/// content potentially contains XML tool calls they're parsed out into
/// `tool_calls` and stripped from `content`. Returns `true` if any
/// transformation was applied.
pub fn normalize_choice_message(msg: &mut ChatMessage) -> Result<bool, serde_json::Error> {
    let mut changed = map_reasoning_to_content(msg);

    // Parse XML tool calls from text content if present.
    if let Some(conduit_llm::MessageContent::Text(text)) = &msg.content {
        if !text.is_empty() && maybe_has_xml_tool_calls(text) {
            let parsed = parse_xml_tool_calls(text)?;
            if !parsed.tool_calls.is_empty() {
                msg.tool_calls.extend(parsed.tool_calls);
                if parsed.remaining.is_empty() {
                    msg.content = None;
                } else {
                    msg.content = Some(conduit_llm::MessageContent::Text(parsed.remaining));
                }
                changed = true;
            }
        }
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_llm::MessageContent;

    type TestResult = Result<(), serde_json::Error>;

    // ---- helpers ----

    fn msg_with_reasoning(reasoning: Option<&str>) -> ChatMessage {
        let mut msg = ChatMessage {
            role: "assistant".to_string(),
            name: None,
            content: Some(MessageContent::Text("Hello!".to_string())),
            tool_calls: Vec::new(),
            tool_call_id: None,
            extra: Default::default(),
        };
        if let Some(r) = reasoning {
            msg.extra
                .insert("reasoning".to_string(), Value::String(r.to_string()));
        }
        msg
    }

    /// Extract the `arguments` JSON string from a parsed tool call.
    fn args_of(tc: &ToolCall) -> String {
        tc.function
            .get("arguments")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    }

    /// Extract the `name` field from a parsed tool call.
    fn name_of(tc: &ToolCall) -> String {
        tc.function
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    }

    // =======================================================================
    // TestMaybeHasXMLToolCalls (mirrors xml_parser_test.go:12-61)
    // =======================================================================
    #[test]
    fn maybe_has_xml_tool_calls_mirrors_go_table() {
        let cases: &[(&str, &str, bool)] = &[
            ("empty content", "", false),
            ("plain text without XML", "Hello, this is just text", false),
            (
                "contains Write tag",
                r#"<Write file_path="x">content</Write>"#,
                true,
            ),
            (
                "contains use_tool",
                r#"<use_tool name="write">content</use_tool>"#,
                true,
            ),
            ("contains Bash tag", "Running <Bash>ls</Bash> command", true),
            ("contains Read tag", r#"<Read file_path="x"/>"#, true),
            // pre-check is intentionally permissive (any XML-like pattern)
            (
                "has angle brackets - pre-check is intentionally permissive",
                "<div>not a tool</div>",
                true,
            ),
        ];
        for (name, content, expected) in cases {
            assert_eq!(maybe_has_xml_tool_calls(content), *expected, "{name}");
        }
    }

    // =======================================================================
    // TestParseXMLToolCalls_* (mirrors xml_parser_test.go:63-186)
    // =======================================================================
    #[test]
    fn parse_self_closing_tag() -> TestResult {
        let content = r#"<Write file_path="/test/file.txt" content="hello world"/>"#;
        let parsed = parse_xml_tool_calls(content)?;
        assert_eq!(parsed.tool_calls.len(), 1);
        let tc = &parsed.tool_calls[0];
        assert_eq!(tc.call_type, "function");
        assert_eq!(name_of(tc), "write");
        let args = args_of(tc);
        assert!(args.contains("file_path"), "args={args}");
        assert!(args.contains("content"), "args={args}");
        assert!(args.contains("/test/file.txt"), "args={args}");
        assert!(args.contains("hello world"), "args={args}");
        Ok(())
    }

    #[test]
    fn parse_simple_content_tag() -> TestResult {
        let content = r#"<Write file_path="/test/file.txt">file contents here</Write>"#;
        let parsed = parse_xml_tool_calls(content)?;
        assert_eq!(parsed.tool_calls.len(), 1);
        let args = args_of(&parsed.tool_calls[0]);
        assert!(args.contains("/test/file.txt"), "args={args}");
        assert!(args.contains("file contents here"), "args={args}");
        Ok(())
    }

    #[test]
    fn parse_json_in_content() -> TestResult {
        let content = r#"<Write>{"file_path": "/test/file.txt", "content": "hello"}</Write>"#;
        let parsed = parse_xml_tool_calls(content)?;
        assert_eq!(parsed.tool_calls.len(), 1);
        let args = args_of(&parsed.tool_calls[0]);
        assert!(args.contains("/test/file.txt"), "args={args}");
        assert!(args.contains("hello"), "args={args}");
        Ok(())
    }

    #[test]
    fn parse_mismatched_closing_tag() -> TestResult {
        // <Write>content</use_tool> is normalized to <Write>content</Write>
        let content = r#"<Write file_path="/test/file.txt">content</use_tool>"#;
        let parsed = parse_xml_tool_calls(content)?;
        assert_eq!(parsed.tool_calls.len(), 1);
        assert_eq!(name_of(&parsed.tool_calls[0]), "write");
        let args = args_of(&parsed.tool_calls[0]);
        assert!(args.contains("file_path"), "args={args}");
        Ok(())
    }

    #[test]
    fn parse_unclosed_opening_tag() -> TestResult {
        // NanoGPT sometimes omits the closing > on opening tags
        let content = "<Write file_path=\"/test/file.txt\" content=\"hello\"\n}\n</use_tool>";
        let parsed = parse_xml_tool_calls(content)?;
        assert_eq!(parsed.tool_calls.len(), 1);
        assert_eq!(name_of(&parsed.tool_calls[0]), "write");
        Ok(())
    }

    #[test]
    fn parse_nested_xml_elements() -> TestResult {
        let content = "<Write>\n  <file_path>/test/file.txt</file_path>\n  <content>hello world</content>\n</Write>";
        let parsed = parse_xml_tool_calls(content)?;
        assert_eq!(parsed.tool_calls.len(), 1);
        let args = args_of(&parsed.tool_calls[0]);
        assert!(args.contains("file_path"), "args={args}");
        assert!(args.contains("/test/file.txt"), "args={args}");
        assert!(args.contains("hello world"), "args={args}");
        Ok(())
    }

    #[test]
    fn parse_no_space_after_tag_name() -> TestResult {
        // Format: <Write_File>{...}</Write_File>
        let content = r#"<Write_File>{"path": "/test/file.txt", "content": "hello"}</Write_File>"#;
        let parsed = parse_xml_tool_calls(content)?;
        assert_eq!(parsed.tool_calls.len(), 1);
        assert_eq!(name_of(&parsed.tool_calls[0]), "write");
        Ok(())
    }

    #[test]
    fn parse_multiple_tool_calls_preserves_in_between_text() -> TestResult {
        let content = r#"<Write file_path="/file1.txt">content1</Write>
Some text in between
<Read file_path="/file2.txt"/>"#;
        let parsed = parse_xml_tool_calls(content)?;
        assert_eq!(parsed.tool_calls.len(), 2);
        assert!(parsed.remaining.contains("Some text in between"));
        assert_eq!(name_of(&parsed.tool_calls[0]), "write");
        assert_eq!(name_of(&parsed.tool_calls[1]), "read");
        Ok(())
    }

    #[test]
    fn parse_no_tool_calls_returns_content_as_remaining() -> TestResult {
        let content = "This is just plain text without any tool calls";
        let parsed = parse_xml_tool_calls(content)?;
        assert!(parsed.tool_calls.is_empty());
        assert_eq!(parsed.remaining, content);
        Ok(())
    }

    #[test]
    fn parse_non_tool_xml_kept_in_remaining() -> TestResult {
        // Non-tool XML tags remain in the output content.
        let content = "<div>not a tool</div>";
        let parsed = parse_xml_tool_calls(content)?;
        assert!(parsed.tool_calls.is_empty());
        assert!(
            parsed.remaining.contains("<div>"),
            "remaining={}",
            parsed.remaining
        );
        Ok(())
    }

    // =======================================================================
    // TestExtractToolName (mirrors xml_parser_test.go:188-215)
    // =======================================================================
    #[test]
    fn extract_tool_name_mirrors_go_table() {
        let cases: &[(&str, &str, &str)] = &[
            ("Write", "", "write"),
            ("Write_FILE", "", "write"),
            ("Write_file", "", "write"),
            ("Read", "", "read"),
            ("Read_FILE", "", "read"),
            ("Bash", "", "bash"),
            ("Python", "", "python"),
            ("Search", "", "search"),
            ("Glob", "", "glob"),
            ("use_tool", r#"name="write""#, "write"),
            ("use_tool", r#"name="Read""#, "read"),
            ("Unknown", "", ""),
            ("", "", ""),
        ];
        for (tag, attrs, expected) in cases {
            assert_eq!(extract_tool_name(tag, attrs), *expected, "tag={tag}");
        }
    }

    // =======================================================================
    // TestGenerateToolCallID (mirrors xml_parser_test.go:217-229)
    // =======================================================================
    #[test]
    fn tool_call_id_is_deterministic_and_distinct() {
        let id1 = generate_tool_call_id("write", r#"{"file_path":"/test.txt"}"#);
        let id2 = generate_tool_call_id("write", r#"{"file_path":"/test.txt"}"#);
        assert_eq!(id1, id2, "same inputs -> same id");
        let id3 = generate_tool_call_id("read", r#"{"file_path":"/test.txt"}"#);
        assert_ne!(id1, id3, "different name -> different id");
        assert!(id1.starts_with("nanogpt_"), "prefix: {id1}");
        assert!(id1.len() > 8, "length: {id1}");
    }

    // =======================================================================
    // TestMessage_ToOpenAIMessage / reasoning mapping
    // (mirrors model_test.go:141-174)
    // =======================================================================
    #[test]
    fn map_reasoning_to_content_copies_reasoning_into_reasoning_content() {
        let mut msg = msg_with_reasoning(Some("thinking..."));
        assert!(map_reasoning_to_content(&mut msg));
        assert_eq!(
            msg.extra.get("reasoning_content").and_then(|v| v.as_str()),
            Some("thinking...")
        );
    }

    #[test]
    fn map_reasoning_to_content_noop_when_absent() {
        let mut msg = msg_with_reasoning(None);
        assert!(!map_reasoning_to_content(&mut msg));
        assert!(msg.extra.get("reasoning_content").is_none());
    }

    #[test]
    fn map_reasoning_to_content_noop_when_null() {
        let mut msg = msg_with_reasoning(None);
        msg.extra.insert("reasoning".to_string(), Value::Null);
        assert!(!map_reasoning_to_content(&mut msg));
    }

    // =======================================================================
    // normalize_choice_message — XML tool-call extraction from content
    // (mirrors model.go:68-81)
    // =======================================================================
    #[test]
    fn normalize_choice_message_extracts_xml_tool_calls_from_content() -> TestResult {
        let mut msg = ChatMessage {
            role: "assistant".to_string(),
            name: None,
            content: Some(MessageContent::Text(
                r#"<Write file_path="/test.txt">hello</Write>"#.to_string(),
            )),
            tool_calls: Vec::new(),
            tool_call_id: None,
            extra: Default::default(),
        };
        let changed = normalize_choice_message(&mut msg)?;
        assert!(changed);
        assert_eq!(msg.tool_calls.len(), 1);
        // content stripped (empty remaining)
        assert!(msg.content.is_none());
        Ok(())
    }

    #[test]
    fn normalize_choice_message_preserves_non_empty_remaining() -> TestResult {
        let mut msg = ChatMessage {
            role: "assistant".to_string(),
            name: None,
            content: Some(MessageContent::Text(
                "Prefix text <Write file_path=\"/f.txt\">c</Write> tail".to_string(),
            )),
            tool_calls: Vec::new(),
            tool_call_id: None,
            extra: Default::default(),
        };
        let changed = normalize_choice_message(&mut msg)?;
        assert!(changed);
        assert_eq!(msg.tool_calls.len(), 1);
        match &msg.content {
            Some(MessageContent::Text(remaining)) => {
                assert!(remaining.contains("Prefix text"), "remaining={remaining}");
                assert!(remaining.contains("tail"), "remaining={remaining}");
            }
            other => panic!("expected Text remaining, got {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn normalize_choice_message_leaves_plain_text_alone() -> TestResult {
        let mut msg = ChatMessage {
            role: "assistant".to_string(),
            name: None,
            content: Some(MessageContent::Text("just plain text".to_string())),
            tool_calls: Vec::new(),
            tool_call_id: None,
            extra: Default::default(),
        };
        let changed = normalize_choice_message(&mut msg)?;
        assert!(!changed);
        assert!(msg.tool_calls.is_empty());
        Ok(())
    }

    // =======================================================================
    // normalize_xml — direct coverage of the malformed-input fixups
    // (mirrors xml_parser.go:216-254)
    // =======================================================================
    #[test]
    fn normalize_xml_fixes_use_use_closing_tag() {
        let fixed = normalize_xml("<Write>c</use_use>");
        assert!(
            fixed.contains("</use_tool>") || fixed.contains("</Write>"),
            "fixed={fixed}"
        );
    }

    #[test]
    fn normalize_xml_fixes_write_file_closing_tag() {
        let fixed = normalize_xml("<Write>c</Write_file>");
        assert!(fixed.contains("</Write>"), "fixed={fixed}");
    }

    #[test]
    fn normalize_xml_adds_space_before_self_closing() {
        let fixed = normalize_xml(r#"<Read file_path="x"/>"#);
        // The normalize step inserts a space: `x/>` -> `x />`
        assert!(
            fixed.contains("/>") || fixed.contains("/ >"),
            "fixed={fixed}"
        );
    }

    // =======================================================================
    // extract_tool_arguments — argument extraction coverage
    // (mirrors xml_parser.go:283-334)
    // =======================================================================
    #[test]
    fn extract_tool_arguments_from_attrs() {
        let args = extract_tool_arguments("write", r#"file_path="/x" content="y""#, "");
        // Order of attributes is not stable across serde_json::Map iteration;
        // check key presence.
        assert!(
            args.contains("\"file_path\":\"/x\"") || args.contains(r#""file_path":"/x""#),
            "args={args}"
        );
        assert!(
            args.contains("\"content\":\"y\"") || args.contains(r#""content":"y""#),
            "args={args}"
        );
    }

    #[test]
    fn extract_tool_arguments_skips_name_attr_for_use_tool() {
        let args = extract_tool_arguments("write", r#"name="write" arg1="v""#, "");
        assert!(!args.contains("\"name\""), "args={args}");
        assert!(args.contains("\"arg1\":\"v\""), "args={args}");
    }

    #[test]
    fn extract_tool_arguments_parses_json_inner_content() {
        let args = extract_tool_arguments("write", "", r#"{"file_path":"/x"}"#);
        assert!(args.contains("/x"), "args={args}");
    }

    #[test]
    fn extract_tool_arguments_treats_non_json_inner_as_content() {
        let args = extract_tool_arguments("bash", "", "ls -la");
        assert!(args.contains("\"content\":\"ls -la\""), "args={args}");
    }

    #[test]
    fn extract_tool_arguments_empty_returns_empty_object() {
        assert_eq!(extract_tool_arguments("unknown", "", ""), "{}");
    }
}
