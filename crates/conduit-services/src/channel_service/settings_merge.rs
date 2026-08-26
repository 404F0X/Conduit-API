//! S15 settings-layer merge.
//!
//! Ported from Go `internal/server/biz/channel_override.go` — the field-wise
//! override merge the channel-build pipeline runs when computing the effective
//! [`ChannelSettings`] for a request. The merge precedence is fixed:
//!
//! `system_default < model_setting < channel_setting < request_override`
//!
//! Scalars / `Option`s take the highest-priority non-default value; `Vec` /
//! `BTreeMap`-like fields concatenate in the same order so later layers extend
//! (not replace) earlier ones; boolean toggles fold via logical OR (a layer
//! can only flip them on).

use conduit_core::objects::channel_settings::ChannelSettings;

/// Merge four [`ChannelSettings`] layers in the fixed precedence
/// `system_default < model_setting < channel_setting < request_override` (S15).
///
/// Each scalar/`Option` field takes the highest-priority `Some`/non-default
/// value; `Vec`/`BTreeMap`-like fields are concatenated in the same order so
/// later layers extend (not replace) earlier ones. Mirrors Go's field-wise
/// override merge performed at channel-build time.
pub fn merge_settings_layers(
    system_default: &ChannelSettings,
    model_setting: &ChannelSettings,
    channel_setting: &ChannelSettings,
    request_override: &ChannelSettings,
) -> ChannelSettings {
    // Start from the system default and fold each layer in priority order.
    let mut merged = system_default.clone();
    merge_into(&mut merged, model_setting);
    merge_into(&mut merged, channel_setting);
    merge_into(&mut merged, request_override);
    merged
}

/// Fold `override_settings` into `base`, with `override_settings` winning for
/// scalars/`Option`s and extending `Vec`s.
fn merge_into(base: &mut ChannelSettings, override_settings: &ChannelSettings) {
    // Control-plane metadata follows normal Option precedence. It does not
    // affect request routing, but retaining it keeps effective settings
    // round-trips lossless for callers that inspect the merged object.
    if override_settings.management_adapter.is_some() {
        base.management_adapter = override_settings.management_adapter.clone();
    }
    if !override_settings.billing_currency.is_empty() {
        base.billing_currency = override_settings.billing_currency.clone();
    }
    if override_settings.recharge_multiplier.is_some() {
        base.recharge_multiplier = override_settings.recharge_multiplier;
    }

    // String scalars: non-empty wins.
    if !override_settings.extra_model_prefix.is_empty() {
        base.extra_model_prefix = override_settings.extra_model_prefix.clone();
    }
    if !override_settings.override_parameters.is_empty() {
        base.override_parameters = override_settings.override_parameters.clone();
    }

    // Vecs extend (later layers add entries; dedup is the caller's job, matching
    // Go which concatenates override operation slices).
    if !override_settings.auto_trimed_model_prefixes.is_empty() {
        base.auto_trimed_model_prefixes
            .extend(override_settings.auto_trimed_model_prefixes.iter().cloned());
    }
    if !override_settings.model_mappings.is_empty() {
        base.model_mappings
            .extend(override_settings.model_mappings.iter().cloned());
    }
    if !override_settings.body_override_operations.is_empty() {
        base.body_override_operations
            .extend(override_settings.body_override_operations.iter().cloned());
    }
    if !override_settings.header_override_operations.is_empty() {
        base.header_override_operations
            .extend(override_settings.header_override_operations.iter().cloned());
    }
    if !override_settings.override_headers.is_empty() {
        base.override_headers
            .extend(override_settings.override_headers.iter().cloned());
    }
    if !override_settings.retryable_status_codes.is_empty() {
        base.retryable_status_codes
            .extend(override_settings.retryable_status_codes.iter().cloned());
    }
    if !override_settings.retryable_error_patterns.is_empty() {
        base.retryable_error_patterns
            .extend(override_settings.retryable_error_patterns.iter().cloned());
    }

    // bool flags: a layer can only flip these on; once set they stay set
    // (mirrors Go's logical-OR fold for these toggles).
    base.hide_original_models |= override_settings.hide_original_models;
    base.hide_mapped_models |= override_settings.hide_mapped_models;
    base.lowercase_model_id |= override_settings.lowercase_model_id;
    base.transform_options.force_array_instructions |=
        override_settings.transform_options.force_array_instructions;
    base.transform_options.force_array_inputs |=
        override_settings.transform_options.force_array_inputs;
    base.transform_options.replace_developer_role_with_system |= override_settings
        .transform_options
        .replace_developer_role_with_system;

    // Options: Some wins.
    if override_settings.proxy.is_some() {
        base.proxy = override_settings.proxy.clone();
    }
    if override_settings.pass_through_user_agent.is_some() {
        base.pass_through_user_agent = override_settings.pass_through_user_agent;
    }
    if override_settings.pass_through_body.is_some() {
        base.pass_through_body = override_settings.pass_through_body;
    }
    if override_settings.rate_limit.is_some() {
        base.rate_limit = override_settings.rate_limit.clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn management_adapter_follows_layer_precedence() {
        let system = ChannelSettings {
            management_adapter: Some("generic".to_string()),
            ..Default::default()
        };
        let channel = ChannelSettings {
            management_adapter: Some("new_api".to_string()),
            ..Default::default()
        };

        let merged = merge_settings_layers(
            &system,
            &ChannelSettings::default(),
            &channel,
            &ChannelSettings::default(),
        );
        assert_eq!(merged.management_adapter.as_deref(), Some("new_api"));
    }
}
