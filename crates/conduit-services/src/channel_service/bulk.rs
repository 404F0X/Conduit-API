//! Channel bulk-update / bulk-create pure decision layer.
//!
//! Minimal port of Go `internal/server/biz/channel_bulk.go`'s pure logic:
//! - `BulkCreateChannelsInput` validation (`no API keys provided`, `base URL
//!   is required`, `invalid channel type`).
//! - Unique-name generator (`"<base> - (<counter>)"`) that skips existing
//!   channel names — mirrors Go's numbered naming loop
//!   (`channel_bulk.go:96-103`).
//! - `ChannelOrderingItem` pure-data type from `BulkUpdateChannelOrdering`.
//!
//! The DB-bound arms (per-channel `client.Channel.Create()` /
//! `UpdateOneID().Save()`, async cache reload via `asyncReloadChannels`) are
//! host-owned and intentionally out of scope; this seam captures only the
//! decision logic so the host can stay thin and parity-testable.

use std::collections::HashSet;

/// Pure-data view of Go's `ChannelOrderingItem`
/// (`internal/server/biz/channel_bulk.go:15-19`): one `(id, weight)` row in a
/// `BulkUpdateChannelOrdering` batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelOrderingItem {
    pub id: i64,
    pub ordering_weight: i64,
}

/// Validation outcome for a bulk-create batch. Mirrors the error branches in
/// Go `BulkCreateChannels` (`channel_bulk.go:60-72`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BulkCreateError {
    /// Go :62-63 — `no API keys provided`.
    #[error("no API keys provided")]
    NoApiKeys,
    /// Go :65-67 — `base URL is required`.
    #[error("base URL is required")]
    MissingBaseUrl,
    /// Go :69-72 — `invalid channel type '%s'`. The offending type string is
    /// captured for diagnostics.
    #[error("invalid channel type {channel_type:?}")]
    InvalidChannelType { channel_type: String },
}

/// Validate the inputs to a `BulkCreateChannels` batch before any DB work.
///
/// Mirrors the three early-return guards in Go's `BulkCreateChannels`
/// (`channel_bulk.go:60-72`). `channel_type_validator` is a host-supplied
/// predicate that returns `false` for unknown types (the host wires it to the
/// transformer-registry's known-type set, the Rust analog of Go's
/// `channel.TypeValidator`).
///
/// `base_url == None` mirrors Go's `input.BaseURL == nil`. An empty-string
/// base URL is allowed (Go does not trim-check here).
pub fn validate_bulk_create(
    api_keys: &[String],
    base_url: Option<&str>,
    channel_type: &str,
    channel_type_validator: impl Fn(&str) -> bool,
) -> Result<(), BulkCreateError> {
    if api_keys.is_empty() {
        return Err(BulkCreateError::NoApiKeys);
    }
    if base_url.is_none() {
        return Err(BulkCreateError::MissingBaseUrl);
    }
    if !channel_type_validator(channel_type) {
        return Err(BulkCreateError::InvalidChannelType {
            channel_type: channel_type.to_string(),
        });
    }
    Ok(())
}

/// Pick the next non-conflicting numbered channel name for a bulk-create
/// batch.
///
/// Mirrors Go's numbered-name loop (`channel_bulk.go:96-103`):
/// - Start with counter = `start_counter` (1 for a fresh batch).
/// - Format as `"{base} - ({counter})"`.
/// - If the formatted name already exists in `existing_names`, increment the
///   counter and retry.
///
/// Returns the first non-conflicting `(name, counter_used)` pair. The host
/// passes `existing_names` as the set of all current channel names (mirrors
/// Go's `existingNames` map built from a `Channel.Query().Select(FieldName)`).
///
/// Pure: no IO. The host applies the returned names to its `Channel.Create()`
/// loop and inserts each into `existing_names` between iterations.
pub fn next_bulk_channel_name(
    base: &str,
    start_counter: i64,
    existing_names: &HashSet<String>,
) -> (String, i64) {
    let mut counter = start_counter.max(1);
    loop {
        let candidate = format!("{base} - ({counter})");
        if !existing_names.contains(&candidate) {
            return (candidate, counter);
        }
        counter += 1;
    }
}

/// Plan the numbered channel names for an entire `BulkCreateChannels` batch
/// without touching the DB.
///
/// Mirrors the Go batch loop (`channel_bulk.go:86-104`): `counter` starts at
/// 1, each iteration formats `"{base} - ({counter})"`, increments past any
/// existing names, then `counter` advances by one (Go line 103 `counter++`)
/// and the chosen name is recorded as taken before the next iteration.
///
/// Returns the ordered list of names (one per api key). The host applies them
/// to its `Channel.Create()` loop. Pure: no IO.
pub fn plan_bulk_create_names(
    base: &str,
    num_keys: usize,
    existing_names: &HashSet<String>,
) -> Vec<String> {
    let mut taken = existing_names.clone();
    let mut counter = 1i64;
    let mut out = Vec::with_capacity(num_keys);
    for _ in 0..num_keys {
        let (name, used) = next_bulk_channel_name(base, counter, &taken);
        taken.insert(name.clone());
        counter = used + 1;
        out.push(name);
    }
    out
}

/// Resolve the tags to attach to each channel in a `BulkCreateChannels`
/// batch. Mirrors Go `channel_bulk.go:89-92`:
/// `tagsToUse := input.Tags; if len(tagsToUse) == 0 { tagsToUse = []string{input.Name} }`.
/// When the caller supplies no tags, the base name is used as the sole tag
/// (backward-compatible default).
pub fn resolve_bulk_create_tags(input_tags: &[String], base_name: &str) -> Vec<String> {
    if input_tags.is_empty() {
        vec![base_name.to_string()]
    } else {
        input_tags.to_vec()
    }
}

// ---------------------------------------------------------------------------
// BulkImportChannels — pure validation layer
// ---------------------------------------------------------------------------

/// Pure-data view of one row of a `BulkImportChannels` batch. Mirrors Go
/// `BulkImportChannelItem` (`channel_bulk.go:215-222`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BulkImportItemView {
    pub channel_type: String,
    pub name: String,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
}

/// Aggregated plan for a `BulkImportChannels` batch — the part of Go's
/// `BulkImportChannelsResult` (`channel_bulk.go:225-231`) that is decidable
/// before any DB insert. `success` mirrors Go `success := failed == 0`
/// (line 294). The host performs the actual `Channel.Create()` per valid
/// item and collects the resulting entities separately.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BulkImportPlan {
    pub success: bool,
    pub created: usize,
    pub failed: usize,
    pub errors: Vec<String>,
}

/// Validate a single import row, returning the Go-formatted error message
/// when the row is invalid.
///
/// Mirrors the three per-item guards in Go `BulkImportChannels`
/// (`channel_bulk.go:243-266`):
/// 1. Invalid channel type (line 246-251) —
///    `Row {i+1}: Invalid channel type '{type}'`.
/// 2. Missing/empty base URL (line 254-259) —
///    `Row {i+1} ({name}): Base URL is required`.
/// 3. Missing/empty API key (line 261-266) —
///    `Row {i+1} ({name}): API Key is required`.
///
/// `row_index` is the 1-based row number (Go's `i+1`).
/// `channel_type_validator` is the host-supplied known-type predicate (the
/// Rust analog of Go's `channel.TypeValidator`).
///
/// Returns `Ok(())` for a valid row (the host then creates the channel and,
/// on DB error, appends its own `Row {i} ({name}): {err}` message — line 284,
/// which is DB-bound and out of scope here).
pub fn validate_bulk_import_item(
    item: &BulkImportItemView,
    row_index: i64,
    channel_type_validator: impl Fn(&str) -> bool,
) -> Result<(), String> {
    if !channel_type_validator(&item.channel_type) {
        return Err(format!(
            "Row {}: Invalid channel type '{}'",
            row_index, item.channel_type
        ));
    }
    if item
        .base_url
        .as_deref()
        .map(|s| s.is_empty())
        .unwrap_or(true)
    {
        return Err(format!(
            "Row {} ({}): Base URL is required",
            row_index, item.name
        ));
    }
    if item
        .api_key
        .as_deref()
        .map(|s| s.is_empty())
        .unwrap_or(true)
    {
        return Err(format!(
            "Row {} ({}): API Key is required",
            row_index, item.name
        ));
    }
    Ok(())
}

/// Validate an entire import batch and aggregate the result.
///
/// Mirrors Go `BulkImportChannels` (`channel_bulk.go:234-305`) up to the
/// point where the host would call `Channel.Create()`. For each item the
/// Go per-row error is reproduced verbatim; valid rows count toward
/// `created`; invalid rows count toward `failed` and contribute to `errors`.
/// `success` is `failed == 0` (Go line 294).
///
/// DB-insert failures (Go line 282-288) are host-owned and intentionally not
/// modelled here.
pub fn plan_bulk_import(
    items: &[BulkImportItemView],
    channel_type_validator: impl Fn(&str) -> bool,
) -> BulkImportPlan {
    let mut created = 0usize;
    let mut failed = 0usize;
    let mut errors: Vec<String> = Vec::new();

    for (i, item) in items.iter().enumerate() {
        let row_index = (i as i64) + 1;
        match validate_bulk_import_item(item, row_index, &channel_type_validator) {
            Ok(()) => created += 1,
            Err(msg) => {
                failed += 1;
                errors.push(msg);
            }
        }
    }

    BulkImportPlan {
        success: failed == 0,
        created,
        failed,
        errors,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    // ---------- Parity with Go channel_bulk.go ----------

    #[test]
    fn s12_validate_rejects_empty_key_list() {
        // Go :62-63.
        let err = validate_bulk_create(&[], Some("https://x"), "openai", |_| true);
        match err {
            Err(BulkCreateError::NoApiKeys) => {}
            other => panic!("expected NoApiKeys error, got {other:?}"),
        }
    }

    #[test]
    fn s12_validate_rejects_missing_base_url() {
        // Go :65-67.
        let err = validate_bulk_create(&["k".to_string()], None, "openai", |_| true);
        match err {
            Err(BulkCreateError::MissingBaseUrl) => {}
            other => panic!("expected MissingBaseUrl error, got {other:?}"),
        }
    }

    #[test]
    fn s12_validate_rejects_unknown_channel_type() {
        // Go :69-72.
        let err = validate_bulk_create(
            &["k".to_string()],
            Some("https://x"),
            "not_a_real_type",
            |t| t == "openai",
        );
        match err {
            Err(BulkCreateError::InvalidChannelType { channel_type }) => {
                assert_eq!(channel_type, "not_a_real_type");
            }
            other => panic!("expected InvalidChannelType error, got {other:?}"),
        }
    }

    #[test]
    fn s12_validate_accepts_known_inputs() -> Result<(), BulkCreateError> {
        validate_bulk_create(
            &["k1".to_string(), "k2".to_string()],
            Some("https://api.openai.com/v1"),
            "openai",
            |t| t == "openai",
        )
    }

    #[test]
    fn s12_next_name_starts_at_counter_one_when_no_conflict() {
        // Go channel_bulk.go:96-103 — fresh batch, counter starts at 1.
        let existing = HashSet::new();
        let (name, counter) = next_bulk_channel_name("my-channel", 1, &existing);
        assert_eq!(name, "my-channel - (1)");
        assert_eq!(counter, 1);
    }

    #[test]
    fn s12_next_name_increments_past_existing_counters() {
        // Go :98-101 — when the formatted name exists, the counter increments
        // until a free slot is found.
        let mut existing = HashSet::new();
        existing.insert("base - (1)".to_string());
        existing.insert("base - (2)".to_string());
        existing.insert("base - (3)".to_string());
        let (name, counter) = next_bulk_channel_name("base", 1, &existing);
        assert_eq!(name, "base - (4)");
        assert_eq!(counter, 4);
    }

    #[test]
    fn s12_next_name_clamps_non_positive_start_counter() {
        // Defensive: Go's loop starts at counter=1; we mirror by clamping
        // non-positive callers up to 1.
        let existing = HashSet::new();
        let (name, counter) = next_bulk_channel_name("base", 0, &existing);
        assert_eq!(name, "base - (1)");
        assert_eq!(counter, 1);
    }

    #[test]
    fn s12_next_name_resumes_from_batch_counter() {
        // Mirrors the Go batch behaviour where `counter` is carried across
        // iterations (so channel 2 of a 10-key batch doesn't restart at 1).
        let mut existing = HashSet::new();
        existing.insert("base - (1)".to_string());
        let (name, counter) = next_bulk_channel_name("base", 2, &existing);
        assert_eq!(name, "base - (2)");
        assert_eq!(counter, 2);
    }

    // ---------- Go TestChannelService_BulkCreateChannels (batch-level) ----------
    // Parity with `conduit/internal/server/biz/channel_test.go`
    // `TestChannelService_BulkCreateChannels` (lines 751-1019). The DB-bound
    // arms (per-channel `client.Channel.Create()`, tags persistence, cleanup
    // `Channel.Delete().Exec()`) are host-owned; these tests exercise the
    // pure naming + tag-resolution logic that the Go test asserts on.

    fn existing_set(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn s12_batch_names_fresh_three_keys() {
        // Go channel_test.go:776-787 "create multiple channels successfully".
        // No existing channels; 3 keys; expected "Test Channel - (1..3)".
        let existing = existing_set(&[]);
        let names = plan_bulk_create_names("Test Channel", 3, &existing);
        assert_eq!(
            names,
            vec![
                "Test Channel - (1)",
                "Test Channel - (2)",
                "Test Channel - (3)",
            ]
        );
    }

    #[test]
    fn s12_batch_names_with_existing_base_name() {
        // Go channel_test.go:788-810 "create channels with existing base
        // name". An "Existing Channel" (un-numbered) already exists; the
        // batch still produces "- (1)" and "- (2)" since those numbered
        // names are free.
        let existing = existing_set(&["Existing Channel"]);
        let names = plan_bulk_create_names("Existing Channel", 2, &existing);
        assert_eq!(
            names,
            vec!["Existing Channel - (1)", "Existing Channel - (2)",]
        );
    }

    #[test]
    fn s12_batch_names_with_some_existing_numbered_names() {
        // Go channel_test.go:811-841 "create channels with some existing
        // numbered names". Existing = {"Test", "Test - (1)"}; 3 new keys;
        // expected "Test - (2..4)".
        let existing = existing_set(&["Test", "Test - (1)"]);
        let names = plan_bulk_create_names("Test", 3, &existing);
        assert_eq!(names, vec!["Test - (2)", "Test - (3)", "Test - (4)",]);
    }

    #[test]
    fn s12_batch_names_single_channel_with_numbering() {
        // Go channel_test.go:864-876 "create single channel with numbering".
        let existing = existing_set(&[]);
        let names = plan_bulk_create_names("Single Channel", 1, &existing);
        assert_eq!(names, vec!["Single Channel - (1)"]);
    }

    #[test]
    fn s12_batch_names_single_channel_when_numbered_name_exists() {
        // Go channel_test.go:877-899 "create single channel when numbered name
        // exists". Existing = {"Conflict - (1)"}; 1 key; expected
        // "Conflict - (2)".
        let existing = existing_set(&["Conflict - (1)"]);
        let names = plan_bulk_create_names("Conflict", 1, &existing);
        assert_eq!(names, vec!["Conflict - (2)"]);
    }

    #[test]
    fn s12_batch_names_with_gaps_in_numbering() {
        // Go channel_test.go:900-930 "create channels with gaps in
        // numbering". Existing = {"Gap Test", "Gap Test - (2)"}; 2 keys;
        // expected "Gap Test - (1)" and "Gap Test - (3)".
        let existing = existing_set(&["Gap Test", "Gap Test - (2)"]);
        let names = plan_bulk_create_names("Gap Test", 2, &existing);
        assert_eq!(names, vec!["Gap Test - (1)", "Gap Test - (3)"]);
    }

    #[test]
    fn s12_resolve_tags_uses_base_name_when_no_input_tags() {
        // Go channel_test.go:786/809/840 — wantTags == [baseName] when
        // input.Tags is nil (Go channel_bulk.go:89-92 backward-compatible
        // default).
        let tags = resolve_bulk_create_tags(&[], "Test Channel");
        assert_eq!(tags, vec!["Test Channel"]);
    }

    #[test]
    fn s12_resolve_tags_passes_through_supplied_tags() {
        // Go channel_bulk.go:89 — `tagsToUse := input.Tags` when non-empty.
        let tags = resolve_bulk_create_tags(&["custom".into(), "vip".into()], "Test Channel");
        assert_eq!(tags, vec!["custom", "vip"]);
    }

    // ---------- Go TestChannelService_BulkImportChannels ----------
    // Parity with `conduit/internal/server/biz/channel_test.go`
    // `TestChannelService_BulkImportChannels` (lines 532-663). The Go test
    // asserts (Success, Created, Failed, len(Errors), len(Channels)); the
    // pure plan reproduces the first four. `len(Channels)` is DB-bound and
    // intentionally not modelled here.

    fn import_item(
        channel_type: &str,
        name: &str,
        base_url: Option<&str>,
        api_key: Option<&str>,
    ) -> BulkImportItemView {
        BulkImportItemView {
            channel_type: channel_type.to_string(),
            name: name.to_string(),
            base_url: base_url.map(|s| s.to_string()),
            api_key: api_key.map(|s| s.to_string()),
        }
    }

    /// Known-type predicate mirroring Go `channel.TypeValidator`: accepts
    /// openai + anthropic, rejects everything else.
    fn is_known_channel_type(t: &str) -> bool {
        t == "openai" || t == "anthropic"
    }

    #[test]
    fn s12_import_multiple_channels_successfully() {
        // Go channel_test.go:549-571 "import multiple channels successfully".
        let items = vec![
            import_item(
                "openai",
                "OpenAI Channel 1",
                Some("https://api.openai.com/v1"),
                Some("test-key-1"),
            ),
            import_item(
                "anthropic",
                "Anthropic Channel 1",
                Some("https://api.anthropic.com"),
                Some("test-key-2"),
            ),
        ];
        let plan = plan_bulk_import(&items, is_known_channel_type);
        assert!(plan.success);
        assert_eq!(plan.created, 2);
        assert_eq!(plan.failed, 0);
        assert_eq!(plan.errors.len(), 0);
    }

    #[test]
    fn s12_import_with_invalid_channel_type() {
        // Go channel_test.go:572-588 "import with invalid channel type".
        let items = vec![import_item(
            "invalid_type",
            "Invalid Channel",
            Some("https://api.example.com"),
            Some("test-key"),
        )];
        let plan = plan_bulk_import(&items, is_known_channel_type);
        assert!(!plan.success);
        assert_eq!(plan.created, 0);
        assert_eq!(plan.failed, 1);
        assert_eq!(plan.errors.len(), 1);
        // Go channel_bulk.go:247 — "Row {i+1}: Invalid channel type '{type}'".
        assert_eq!(plan.errors[0], "Row 1: Invalid channel type 'invalid_type'");
    }

    #[test]
    fn s12_import_with_missing_base_url() {
        // Go channel_test.go:589-605 "import with missing base URL".
        let items = vec![import_item(
            "openai",
            "Missing BaseURL",
            None,
            Some("test-key"),
        )];
        let plan = plan_bulk_import(&items, is_known_channel_type);
        assert!(!plan.success);
        assert_eq!(plan.created, 0);
        assert_eq!(plan.failed, 1);
        assert_eq!(plan.errors.len(), 1);
        // Go channel_bulk.go:255 — "Row {i+1} ({name}): Base URL is required".
        assert_eq!(
            plan.errors[0],
            "Row 1 (Missing BaseURL): Base URL is required"
        );
    }

    #[test]
    fn s12_import_with_missing_api_key() {
        // Go channel_test.go:606-622 "import with missing API key".
        let items = vec![import_item(
            "openai",
            "Missing APIKey",
            Some("https://api.openai.com/v1"),
            None,
        )];
        let plan = plan_bulk_import(&items, is_known_channel_type);
        assert!(!plan.success);
        assert_eq!(plan.created, 0);
        assert_eq!(plan.failed, 1);
        assert_eq!(plan.errors.len(), 1);
        // Go channel_bulk.go:262 — "Row {i+1} ({name}): API Key is required".
        assert_eq!(
            plan.errors[0],
            "Row 1 (Missing APIKey): API Key is required"
        );
    }

    #[test]
    fn s12_import_partial_success_some_valid_some_invalid() {
        // Go channel_test.go:623-647 "partial success - some valid, some
        // invalid". One valid openai row + one invalid_type row; expected
        // Success=false, Created=1, Failed=1, len(Errors)=1.
        let items = vec![
            import_item(
                "openai",
                "Valid Channel",
                Some("https://api.openai.com/v1"),
                Some("test-key"),
            ),
            import_item(
                "invalid_type",
                "Invalid Channel",
                Some("https://api.example.com"),
                Some("test-key"),
            ),
        ];
        let plan = plan_bulk_import(&items, is_known_channel_type);
        assert!(!plan.success);
        assert_eq!(plan.created, 1);
        assert_eq!(plan.failed, 1);
        assert_eq!(plan.errors.len(), 1);
        // The invalid row is the 2nd item -> Row 2.
        assert_eq!(plan.errors[0], "Row 2: Invalid channel type 'invalid_type'");
    }

    #[test]
    fn s12_import_empty_base_url_string_treated_as_missing() {
        // Go channel_bulk.go:254 — `item.BaseURL == nil || *item.BaseURL == ""`.
        // An empty-string base URL is rejected just like a nil one (not a Go
        // subtest, but the load-bearing rule the "missing base URL" case
        // implies).
        let items = vec![import_item("openai", "Empty BaseURL", Some(""), Some("k"))];
        let plan = plan_bulk_import(&items, is_known_channel_type);
        assert!(!plan.success);
        assert_eq!(plan.failed, 1);
        assert_eq!(
            plan.errors[0],
            "Row 1 (Empty BaseURL): Base URL is required"
        );
    }

    #[test]
    fn s12_import_empty_api_key_string_treated_as_missing() {
        // Go channel_bulk.go:261 — `item.APIKey == nil || *item.APIKey == ""`.
        let items = vec![import_item(
            "openai",
            "Empty APIKey",
            Some("https://api.openai.com/v1"),
            Some(""),
        )];
        let plan = plan_bulk_import(&items, is_known_channel_type);
        assert!(!plan.success);
        assert_eq!(plan.failed, 1);
        assert_eq!(plan.errors[0], "Row 1 (Empty APIKey): API Key is required");
    }
}
