# Protocol Golden Cases

Golden cases live at:

```text
tests/contracts/llm_cases/<api_format>/<case_name>.json
```

Non-streaming cases use these top-level sections:

```json
{
  "inbound_http": {},
  "unified_request": {},
  "selected_channel": {},
  "outbound_http": {},
  "upstream_http": {},
  "client_http": {}
}
```

Streaming cases also include an ordered `events` array. An event may contain
an SSE `event` name, a string or JSON `data` value, and a `comment`. The OpenAI
`[DONE]` sentinel must occur exactly once as the final data frame when that
protocol requires it. Gemini JSON-array streams are not SSE and use separate
assertions.

Use placeholders for nondeterministic values:

- `$ANY_TIMESTAMP`
- `$ANY_ID`
- `$ANY_TRACE_ID`
- `$ANY_THREAD_ID`

Binary bodies use lowercase `body_bytes_hex` instead of `body_json`. Fixtures
must come from executable protocol tests or verified provider behavior; do not
add speculative wire shapes. Never include real credentials or customer data.
