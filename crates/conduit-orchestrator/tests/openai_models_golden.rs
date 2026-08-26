//! RUST-P9-002 A02 — `/v1/models` + `/v1/models/{id}` OpenAI-compatible
//! response golden coverage.
//!
//! These integration tests live in `conduit-orchestrator` (per task A02 crate
//! scope) and exercise the OpenAI-compatible JSON wire shape produced by the
//! `conduit-services` shaping helpers (`shape_basic_facade`,
//! `shape_model_facade`, `ModelInclude`). The orchestrator crate already
//! depends on `conduit-services`, so this is a neutral host crate that avoids
//! concurrency conflicts with ongoing work in `conduit-http` / `conduit-services`.
//!
//! Every test names the Go source test it mirrors (file + lines), and asserts
//! the JSON payload is byte-compatible with what Go's `encoding/json` emits for
//! `OpenAIModel` / `gin.H{"object":"list", ...}` (openai.go:470-504, 784-788).
//!
//! Go sources:
//! * `conduit/internal/server/api/openai_retrieve_test.go` (lines as cited)
//! * `conduit/internal/server/api/openai_model_test.go` (lines as cited)
//! * `conduit/internal/server/api/openai.go` (handlers + type defs)
//!
//! Workspace lints forbid `.unwrap()` / `.expect()` — tests use `?` with
//! `Result<(), Box<dyn std::error::Error>>`.

use conduit_core::objects::{
    ModelCard, ModelCardCost, ModelCardLimit, ModelCardModalities, ModelCardReasoning,
};
use conduit_services::{ModelFacade, ModelInclude, shape_basic_facade, shape_model_facade};
use serde_json::{Value, json};

// -----------------------------------------------------------------------------
// Test fixtures
// -----------------------------------------------------------------------------

/// Sample `ModelCard` mirroring the Go `TestOpenAIHandlers_RetrieveModel_*
/// _ReturnsExtendedConfiguredModel` fixture (openai_retrieve_test.go:163-170).
fn gpt_41_card() -> ModelCard {
    ModelCard {
        reasoning: ModelCardReasoning {
            supported: true,
            default: false,
        },
        tool_call: true,
        temperature: false,
        modalities: ModelCardModalities {
            input: vec!["text".to_string(), "image".to_string()],
            output: vec!["text".to_string()],
        },
        vision: true,
        cost: ModelCardCost {
            input: 2.0,
            output: 8.0,
            cache_read: 0.5,
            cache_write: 1.0,
        },
        limit: ModelCardLimit {
            context: 200_000,
            output: 8_192,
        },
        knowledge: String::new(),
        release_date: String::new(),
        last_updated: String::new(),
    }
}

/// Mirror Go's `gin.H{"object":"list", "data": [...]}` wrapper
/// (openai.go:784-787). The services crate owns `ModelFacade` but not the list
/// envelope, so we reconstruct the host-side wrapping shape here. The data
/// array elements are serialized `ModelFacade` objects.
fn wrap_list(models: &[ModelFacade]) -> Value {
    let data: Vec<Value> = models
        .iter()
        .map(|m| serde_json::to_value(m).unwrap_or(Value::Null))
        .collect();
    json!({
        "object": "list",
        "data": data,
    })
}

// -----------------------------------------------------------------------------
// Retrieve — single model shape (openai.go:699-720)
// -----------------------------------------------------------------------------
//
// Go's `RetrieveModel` returns a bare `OpenAIModel` JSON object — NOT wrapped
// in `{object: "list", data: [...]}`. The basic branch (openai.go:699-701)
// emits `convertModelFacadeToOpenAIModel` which carries only id/object/created/
// owned_by. These four fields MUST be present even on the extended path; the
// extended branch (openai.go:704-720) adds fields inline.

/// Mirrors `TestOpenAIHandlers_RetrieveModel_SupportsSlashModelIDs`
/// (openai_retrieve_test.go:67-98): GET /v1/models/deepseek/deepseek-chat
/// returns a bare model object whose `id` preserves the inner slash. The Go
/// test pins `{id: "deepseek/deepseek-chat", object: "model",
/// created: 1712345678, owned_by: "openai"}` and asserts no extended fields.
#[test]
fn golden_retrieve_basic_facade_shape_and_slash_id_preserved()
-> Result<(), Box<dyn std::error::Error>> {
    let facade = shape_basic_facade("deepseek/deepseek-chat", "openai", 1_712_345_678);
    let body = serde_json::to_value(&facade)?;

    // Bare object shape — no list wrapper.
    assert_eq!(body["object"], "model");
    assert!(
        !body.as_object().map_or(false, |o| o.contains_key("data")),
        "retrieve must NOT wrap the model in a list envelope"
    );

    // Go-basic field set (openai.go:490-494).
    assert_eq!(body["id"], "deepseek/deepseek-chat");
    assert_eq!(body["created"], 1_712_345_678);
    assert_eq!(body["owned_by"], "openai");

    // Extended fields must be absent (Go's `omitempty` tags drop them).
    for field in [
        "name",
        "description",
        "context_length",
        "max_output_tokens",
        "pricing",
        "capabilities",
        "modalities",
        "icon",
        "type",
    ] {
        assert!(
            body.get(field).is_none() || body[field].is_null(),
            "extended field `{field}` must be absent on the basic facade path"
        );
    }
    Ok(())
}

/// Mirrors `TestOpenAIHandlers_RetrieveModel_FallsBackToBasicWhenConfiguredMetadataMissing`
/// (openai_retrieve_test.go:100-133): when `?include=all` is requested but no
/// configured DB row exists, the response degrades to the basic facade. Go
/// asserts `got.Name`/`got.Capabilities`/`got.Pricing` are all empty/nil.
#[test]
fn golden_retrieve_extended_request_falls_back_to_basic_when_card_missing()
-> Result<(), Box<dyn std::error::Error>> {
    let include = ModelInclude::parse("all", false);

    // No ModelCard available — pass `None`.
    let facade = shape_model_facade(
        "gpt-4o-mini",
        "openai",
        1_712_345_688,
        "", // name unknown on the basic fallback path
        "", // icon unknown
        "", // type unknown
        None,
        None,
        &include,
    );
    let body = serde_json::to_value(&facade)?;

    assert_eq!(body["id"], "gpt-4o-mini");
    assert_eq!(body["object"], "model");
    assert_eq!(body["created"], 1_712_345_688);
    assert_eq!(body["owned_by"], "openai");
    // Go asserts Name/Capabilities/Pricing are all empty on the fallback path.
    assert!(
        !body
            .as_object()
            .map_or(false, |o| o.contains_key("capabilities"))
            || body["capabilities"].is_null(),
        "capabilities must be absent when no DB row exists"
    );
    assert!(
        !body
            .as_object()
            .map_or(false, |o| o.contains_key("pricing"))
            || body["pricing"].is_null(),
        "pricing must be absent when no DB row exists"
    );
    Ok(())
}

/// Mirrors `TestOpenAIHandlers_RetrieveModel_ReturnsExtendedConfiguredModel`
/// (openai_retrieve_test.go:135-216) AND `TestConvertModelToOpenAIExtended_CompleteData`
/// (openai_model_test.go:39-76): include=all with a full ModelCard populates
/// every extended field. The serialized JSON must match Go's wire shape
/// field-for-field.
#[test]
fn golden_retrieve_extended_complete_payload_matches_go_wire_shape()
-> Result<(), Box<dyn std::error::Error>> {
    let card = gpt_41_card();
    let include = ModelInclude::parse("all", false);
    let facade = shape_model_facade(
        "gpt-4.1",
        "openai",
        1_712_345_708,
        "GPT-4.1",
        "openai",
        "chat",
        Some("GPT-4.1 reasoning model"),
        Some(&card),
        &include,
    );
    let body = serde_json::to_value(&facade)?;

    // Basic fields (openai.go:563-568).
    assert_eq!(body["id"], "gpt-4.1");
    assert_eq!(body["object"], "model");
    assert_eq!(body["created"], 1_712_345_708);
    assert_eq!(body["owned_by"], "openai");

    // Optional non-card fields (openai.go:581-594).
    assert_eq!(body["name"], "GPT-4.1");
    assert_eq!(body["description"], "GPT-4.1 reasoning model");
    assert_eq!(body["type"], "chat");
    assert_eq!(body["icon"], "openai");

    // Capabilities (openai.go:612-619) — note the snake_case `tool_call` tag.
    assert_eq!(body["capabilities"]["vision"], true);
    assert_eq!(body["capabilities"]["tool_call"], true);
    assert_eq!(body["capabilities"]["reasoning"], true);

    // Context + max output tokens (openai.go:620-625).
    assert_eq!(body["context_length"], 200_000);
    assert_eq!(body["max_output_tokens"], 8_192);

    // Pricing (openai.go:626-636) — `unit`/`currency` hardcoded by Go.
    assert_eq!(body["pricing"]["input"], 2.0);
    assert_eq!(body["pricing"]["output"], 8.0);
    assert_eq!(body["pricing"]["cache_read"], 0.5);
    assert_eq!(body["pricing"]["cache_write"], 1.0);
    assert_eq!(body["pricing"]["unit"], "per_1m_tokens");
    assert_eq!(body["pricing"]["currency"], "USD");

    // Modalities (openai.go:596-611).
    assert_eq!(body["modalities"]["input"][0], "text");
    assert_eq!(body["modalities"]["input"][1], "image");
    assert_eq!(body["modalities"]["output"][0], "text");
    Ok(())
}

/// Mirrors `TestOpenAIHandlers_RetrieveModel_ReturnsEmptyModalitiesWhenZeroValue`
/// (openai_retrieve_test.go:218-280): when `ModelCard.Modalities` is the zero
/// value (empty slices), the response must STILL emit `modalities: {input: [],
/// output: []}` — never `null`. Go's `convertModelToOpenAIExtended` substitutes
/// empty slices for nil at openai.go:599-604 specifically so the JSON output
/// is `[]` rather than `null`. The Rust port must match this invariant.
#[test]
fn golden_retrieve_zero_modalities_emits_empty_arrays_not_null()
-> Result<(), Box<dyn std::error::Error>> {
    let card = ModelCard {
        // Modalities fields default to empty Vec — the Go zero value.
        modalities: ModelCardModalities {
            input: Vec::new(),
            output: Vec::new(),
        },
        vision: true,
        tool_call: true,
        reasoning: ModelCardReasoning {
            supported: false,
            default: false,
        },
        limit: ModelCardLimit {
            context: 200_000,
            output: 8_192,
        },
        cost: ModelCardCost {
            input: 2.0,
            output: 8.0,
            cache_read: 0.0,
            cache_write: 0.0,
        },
        temperature: false,
        knowledge: String::new(),
        release_date: String::new(),
        last_updated: String::new(),
    };
    let include = ModelInclude::parse("all", false);
    let facade = shape_model_facade(
        "gpt-4.1",
        "openai",
        1_712_345_708,
        "GPT-4.1",
        "openai",
        "chat",
        None,
        Some(&card),
        &include,
    );
    let body = serde_json::to_value(&facade)?;

    // Critical parity invariant (Go openai.go:599-604 + test line 275-279):
    // `modalities` must be a non-null object whose input/output are non-null
    // empty arrays.
    assert!(
        body["modalities"].is_object(),
        "modalities must be an object, got {:?}",
        body["modalities"]
    );
    assert_eq!(
        body["modalities"]["input"],
        json!([]),
        "modalities.input must serialize as [] not null"
    );
    assert_eq!(
        body["modalities"]["output"],
        json!([]),
        "modalities.output must serialize as [] not null"
    );
    Ok(())
}

/// Mirrors `TestConvertModelToOpenAIExtended_NilRemark`
/// (openai_model_test.go:78-94): when the model has `Remark == nil`, the
/// description comes out as the empty string and the rest of the basic shape
/// stays intact. Because the services `shape_model_facade` routes `None` remark
/// through `description = None` (rather than `Some("")`), the JSON drops the
/// field entirely via `omitempty` — this matches Go's `description,omitempty`
/// tag behavior.
#[test]
fn golden_retrieve_nil_remark_omits_description() -> Result<(), Box<dyn std::error::Error>> {
    let include = ModelInclude::parse("all", false);
    let facade = shape_model_facade(
        "gpt-4",
        "openai",
        1_700_000_000,
        "GPT-4",
        "openai",
        "chat",
        None, // remark == None
        None,
        &include,
    );
    let body = serde_json::to_value(&facade)?;

    // Basic fields still present.
    assert_eq!(body["id"], "gpt-4");
    assert_eq!(body["name"], "GPT-4");
    // `description` is absent under Go's `omitempty` tag when remark is nil
    // (the Rust port serializes `None` as missing rather than `""`).
    assert!(
        !body
            .as_object()
            .map_or(false, |o| o.contains_key("description"))
            || body["description"].is_null(),
        "description must be absent when remark is nil"
    );
    assert!(
        !body
            .as_object()
            .map_or(false, |o| o.contains_key("capabilities"))
            || body["capabilities"].is_null()
    );
    assert!(
        !body
            .as_object()
            .map_or(false, |o| o.contains_key("pricing"))
            || body["pricing"].is_null()
    );
    Ok(())
}

// -----------------------------------------------------------------------------
// List — `{object:"list", data:[...]}` envelope (openai.go:784-787)
// -----------------------------------------------------------------------------

/// Mirrors `TestOpenAIHandlers_ListModels_UsesBasicFieldsByDefault`
/// (openai_retrieve_test.go:299-365): GET /v1/models with no `?include=` and
/// the system default off produces the list envelope with basic facades only.
/// Extended fields (name/capabilities/pricing/modalities) MUST NOT appear in
/// the serialized JSON — Go asserts `Empty()`/`Nil()` on each.
#[test]
fn golden_list_basic_envelope_omits_extended_fields() -> Result<(), Box<dyn std::error::Error>> {
    let models = vec![
        shape_basic_facade("gpt-4.1", "openai", 1_712_345_698),
        shape_basic_facade("claude-3-opus-20240229", "anthropic", 1_712_345_000),
    ];
    let body = wrap_list(&models);

    assert_eq!(body["object"], "list");
    assert_eq!(body["data"].as_array().map(Vec::len), Some(2));

    let first = &body["data"][0];
    assert_eq!(first["object"], "model");
    assert_eq!(first["id"], "gpt-4.1");
    assert_eq!(first["owned_by"], "openai");
    assert_eq!(first["created"], 1_712_345_698);

    // Extended fields absent (Go `Empty()`/`Nil()` asserts at lines 361-364).
    for field in ["name", "capabilities", "pricing", "modalities"] {
        assert!(
            first.get(field).is_none() || first[field].is_null(),
            "extended field `{field}` must be absent in basic list mode"
        );
    }
    Ok(())
}

/// Mirrors the empty-models short-circuit in `ListModels`
/// (openai.go:741-748): when no models are visible, the response is still a
/// well-formed `{object:"list", data:[]}` envelope — `data` MUST be an empty
/// array, not `null` or absent.
#[test]
fn golden_list_empty_visible_models_emits_empty_data_array()
-> Result<(), Box<dyn std::error::Error>> {
    let body = wrap_list(&[]);

    assert_eq!(body["object"], "list");
    assert!(
        body["data"].is_array(),
        "data must be an array even when empty, got {:?}",
        body["data"]
    );
    assert_eq!(body["data"].as_array().map(Vec::len), Some(0));
    Ok(())
}

/// Mirrors `TestOpenAIHandlers_ListModels_ExtendedModeFallsBackToBasicForMissingDBModel`
/// (openai_retrieve_test.go:585-686): when `DefaultModelAPIIncludeAll` is on
/// but a visible model has no DB entry, the response mixes one extended row
/// (with capabilities/pricing) and one basic row (without). The list envelope
/// shape stays the same — only the per-row extended-field presence varies.
#[test]
fn golden_list_extended_mode_mixed_basic_and_extended_rows()
-> Result<(), Box<dyn std::error::Error>> {
    let card = gpt_41_card();
    let include_all = ModelInclude::parse("all", false);

    let extended_row = shape_model_facade(
        "gpt-4.1",
        "openai",
        1_712_345_698,
        "GPT-4.1",
        "openai",
        "chat",
        Some("GPT-4.1 reasoning model"),
        Some(&card),
        &include_all,
    );
    // Sibling model has NO DB row — host-side falls back to basic facade.
    let basic_row = shape_basic_facade("gpt-4.1-mini", "openai", 1_712_345_698);

    let body = wrap_list(&[extended_row, basic_row]);
    assert_eq!(body["object"], "list");
    assert_eq!(body["data"].as_array().map(Vec::len), Some(2));

    // Match-assign the array once so the borrow lives long enough; never use
    // `.unwrap()` (workspace lint denies it).
    let data_array = match body["data"].as_array() {
        Some(arr) => arr,
        None => return Err("data must be an array".into()),
    };
    let by_id: std::collections::BTreeMap<&str, &Value> = data_array
        .iter()
        .filter_map(|row| row["id"].as_str().map(|id| (id, row)))
        .collect();

    let extended_entry = by_id.get("gpt-4.1").ok_or("gpt-4.1 row missing")?;
    let basic_entry = by_id
        .get("gpt-4.1-mini")
        .ok_or("gpt-4.1-mini row missing")?
        .as_object()
        .ok_or("gpt-4.1-mini must be an object")?;

    // Extended row has capabilities + pricing.
    assert!(extended_entry["capabilities"].is_object());
    assert!(extended_entry["pricing"].is_object());

    // Basic row does NOT have capabilities/pricing — the parity invariant Go
    // asserts at openai_retrieve_test.go:681-685.
    assert!(
        !basic_entry.contains_key("capabilities") || basic_entry["capabilities"].is_null(),
        "basic fallback row must not carry capabilities"
    );
    assert!(
        !basic_entry.contains_key("pricing") || basic_entry["pricing"].is_null(),
        "basic fallback row must not carry pricing"
    );
    Ok(())
}

// -----------------------------------------------------------------------------
// Field-name invariants (Go JSON tags parity — no acronym rename gotchas here,
// but `tool_call` and `type` deserve dedicated pinning because serde's default
// `r#type` handling could silently regress).
// -----------------------------------------------------------------------------

/// Pins the Go JSON tag for `Capabilities.ToolCall` (openai.go:477):
/// `json:"tool_call"` — snake_case. A regression to `toolCall` (the
/// `#[serde(rename_all = "camelCase")]` default) would diverge from OpenAI's
/// wire shape and is the most common Go→Rust serde gotcha flagged in
/// `CLAUDE.md`.
#[test]
fn golden_capabilities_use_snake_case_tool_call_field() -> Result<(), Box<dyn std::error::Error>> {
    let card = gpt_41_card();
    let include = ModelInclude::parse("capabilities", false);
    let facade = shape_model_facade(
        "gpt-4.1",
        "openai",
        1,
        "",
        "",
        "",
        None,
        Some(&card),
        &include,
    );
    let body = serde_json::to_value(&facade)?;

    // MUST be `tool_call`, NOT `toolCall`.
    assert!(
        body["capabilities"].get("tool_call").is_some(),
        "capabilities must use snake_case `tool_call` field"
    );
    assert!(
        !body["capabilities"]
            .as_object()
            .map_or(false, |o| o.contains_key("toolCall")),
        "capabilities must NOT use camelCase `toolCall` field"
    );
    // The other capability fields are single-word, so they're unaffected.
    assert_eq!(body["capabilities"]["vision"], true);
    assert_eq!(body["capabilities"]["reasoning"], true);
    Ok(())
}

/// Pins the Go JSON tag for `OpenAIModel.Type` (openai.go:503): the field name
/// is the reserved word `type`, serialized verbatim. The Rust port uses
/// `r#type: Option<String>` with no explicit rename; serde strips the `r#`
/// prefix by default, producing `type` on the wire.
#[test]
fn golden_model_type_field_serializes_as_reserved_word_type()
-> Result<(), Box<dyn std::error::Error>> {
    let card = gpt_41_card();
    let include = ModelInclude::parse("type", false);
    let facade = shape_model_facade(
        "gpt-4.1",
        "openai",
        1,
        "",
        "",
        "chat",
        None,
        Some(&card),
        &include,
    );
    let body = serde_json::to_value(&facade)?;

    // MUST serialize as `type`, NOT `ty`/`type_`/`r#type`.
    assert_eq!(body["type"], "chat");
    // Make sure no leaked `r#type` key sneaks in.
    let serialized = serde_json::to_string(&facade)?;
    assert!(
        !serialized.contains("r#type"),
        "serialized payload must not contain the raw `r#type` identifier: {serialized}"
    );
    assert!(
        serialized.contains("\"type\""),
        "serialized payload must contain the `\"type\"` JSON key: {serialized}"
    );
    Ok(())
}

/// Pins the OpenAI/Go pricing hardcodes (openai.go:632-633): `unit` is always
/// `"per_1m_tokens"` and `currency` is always `"USD"`, regardless of input.
/// `TestConvertModelToOpenAIExtended_CompleteData` (openai_model_test.go:74-75)
/// asserts these literal values.
#[test]
fn golden_pricing_carries_hardcoded_unit_and_currency() -> Result<(), Box<dyn std::error::Error>> {
    let card = gpt_41_card();
    let include = ModelInclude::parse("pricing", false);
    let facade = shape_model_facade(
        "gpt-4.1",
        "openai",
        1,
        "",
        "",
        "",
        None,
        Some(&card),
        &include,
    );
    let body = serde_json::to_value(&facade)?;

    assert_eq!(body["pricing"]["unit"], "per_1m_tokens");
    assert_eq!(body["pricing"]["currency"], "USD");
    // Numeric fields preserve Go's float64 precision.
    assert_eq!(body["pricing"]["input"], 2.0);
    assert_eq!(body["pricing"]["output"], 8.0);
    assert_eq!(body["pricing"]["cache_read"], 0.5);
    assert_eq!(body["pricing"]["cache_write"], 1.0);
    Ok(())
}

// -----------------------------------------------------------------------------
// Per-channel model merge (QueryAllChannelModels) — channel-derived rows
// -----------------------------------------------------------------------------
//
// Go's `ModelService.ListEnabledModels` (model.go:644-697) merges
// configured `*ent.Model` rows with channel-derived ids when
// `QueryAllChannelModels == true`. Each channel-derived row is shaped via
// `convertModelFacadeToOpenAIModel` (basic-only). The orchestrator-side
// shaping helper `shape_basic_facade` is the direct counterpart; this test
// pins the merged-list envelope shape so the merge ordering is byte-stable.

/// Mirrors the merge shape implied by Go `ListModels` extended-mode fallback
/// (openai.go:775-781): for each visible model, if a configured `*ent.Model`
/// is found, emit the extended shape; otherwise emit the basic facade. The
/// resulting list envelope preserves visible-model ordering.
#[test]
fn golden_list_merged_channel_and_configured_rows_preserves_order()
-> Result<(), Box<dyn std::error::Error>> {
    let card = gpt_41_card();
    let include_all = ModelInclude::parse("all", false);

    // Simulate the host-side merge: configured row first (gpt-4.1), then two
    // channel-derived rows with no DB backing.
    let merged: Vec<ModelFacade> = vec![
        shape_model_facade(
            "gpt-4.1",
            "openai",
            1_712_345_698,
            "GPT-4.1",
            "openai",
            "chat",
            Some("GPT-4.1 reasoning model"),
            Some(&card),
            &include_all,
        ),
        shape_basic_facade("gpt-4.1-mini", "openai", 1_712_345_698),
        shape_basic_facade("text-embedding-3-small", "openai", 1_712_345_698),
    ];

    let body = wrap_list(&merged);
    assert_eq!(body["object"], "list");
    let data = body["data"].as_array().ok_or("data must be an array")?;
    assert_eq!(data.len(), 3);

    // Ordering preserved: configured > channel-derived sibling > embedding.
    assert_eq!(data[0]["id"], "gpt-4.1");
    assert_eq!(data[1]["id"], "gpt-4.1-mini");
    assert_eq!(data[2]["id"], "text-embedding-3-small");

    // The configured row carries extended fields.
    assert!(data[0]["capabilities"].is_object());
    // The channel-derived rows do not.
    assert!(
        !data[1]
            .as_object()
            .map_or(false, |o| o.contains_key("capabilities"))
            || data[1]["capabilities"].is_null()
    );
    assert!(
        !data[2]
            .as_object()
            .map_or(false, |o| o.contains_key("capabilities"))
            || data[2]["capabilities"].is_null()
    );
    Ok(())
}

// -----------------------------------------------------------------------------
// Include parameter — golden scenarios from parseOpenAIModelInclude
// (openai.go:515-547)
// -----------------------------------------------------------------------------

/// Mirrors the three branches of `parseOpenAIModelInclude` (openai.go:515-547):
///
/// * `""` with default=false -> basic-only (no extended fields populated)
/// * `""` with default=true  -> all extended fields populated
/// * `"all"` -> all extended fields populated regardless of default
///
/// Each branch produces a different golden wire shape; this test asserts each
/// one end-to-end via `shape_model_facade`.
#[test]
fn golden_include_parameter_branches_produce_expected_wire_shapes()
-> Result<(), Box<dyn std::error::Error>> {
    let card = gpt_41_card();

    // Branch 1: empty include, default off -> basic only.
    let basic = shape_model_facade(
        "gpt-4.1",
        "openai",
        1,
        "GPT-4.1",
        "openai",
        "chat",
        Some("remark"),
        Some(&card),
        &ModelInclude::parse("", false),
    );
    let basic_body = serde_json::to_value(&basic)?;
    assert!(
        !basic_body
            .as_object()
            .map_or(false, |o| o.contains_key("name"))
            || basic_body["name"].is_null()
    );
    assert!(
        !basic_body
            .as_object()
            .map_or(false, |o| o.contains_key("pricing"))
            || basic_body["pricing"].is_null()
    );

    // Branch 2: empty include, default on -> all extended.
    let all_default = shape_model_facade(
        "gpt-4.1",
        "openai",
        1,
        "GPT-4.1",
        "openai",
        "chat",
        Some("remark"),
        Some(&card),
        &ModelInclude::parse("", true),
    );
    let all_default_body = serde_json::to_value(&all_default)?;
    assert_eq!(all_default_body["name"], "GPT-4.1");
    assert!(all_default_body["pricing"].is_object());
    assert!(all_default_body["capabilities"].is_object());

    // Branch 3: "all" -> all extended regardless of default.
    let all_explicit = shape_model_facade(
        "gpt-4.1",
        "openai",
        1,
        "GPT-4.1",
        "openai",
        "chat",
        Some("remark"),
        Some(&card),
        &ModelInclude::parse("all", false),
    );
    let all_explicit_body = serde_json::to_value(&all_explicit)?;
    assert_eq!(all_explicit_body["name"], "GPT-4.1");
    assert!(all_explicit_body["pricing"].is_object());

    // Branch 4 (parity narrowing): a single named field selects only that
    // field — others stay absent even though the ModelCard carries data.
    let single = shape_model_facade(
        "gpt-4.1",
        "openai",
        1,
        "GPT-4.1",
        "openai",
        "chat",
        Some("remark"),
        Some(&card),
        &ModelInclude::parse("pricing", false),
    );
    let single_body = serde_json::to_value(&single)?;
    assert!(single_body["pricing"].is_object());
    assert!(
        !single_body
            .as_object()
            .map_or(false, |o| o.contains_key("capabilities"))
            || single_body["capabilities"].is_null()
    );
    assert!(
        !single_body
            .as_object()
            .map_or(false, |o| o.contains_key("context_length"))
            || single_body["context_length"].is_null()
    );
    assert!(
        !single_body
            .as_object()
            .map_or(false, |o| o.contains_key("name"))
            || single_body["name"].is_null()
    );
    Ok(())
}

// -----------------------------------------------------------------------------
// `object` field invariant — always `"model"` for entries, `"list"` for the
// envelope (openai.go:507, 784-787).
// -----------------------------------------------------------------------------

/// Pins the Go constant `openAIModelObjectType = "model"` (openai.go:507) and
/// the envelope constant `"list"` (openai.go:784-787). A regression here would
/// break OpenAI SDK clients that switch on `object`.
#[test]
fn golden_object_tag_invariants_model_and_list() -> Result<(), Box<dyn std::error::Error>> {
    let entry = shape_basic_facade("gpt-4o", "openai", 1);
    let entry_body = serde_json::to_value(&entry)?;
    assert_eq!(entry_body["object"], "model");

    let list_body = wrap_list(&[entry]);
    assert_eq!(list_body["object"], "list");
    assert_eq!(list_body["data"][0]["object"], "model");
    Ok(())
}
