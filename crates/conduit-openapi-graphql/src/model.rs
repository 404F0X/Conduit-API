//! GraphQL model types for the OpenAPI surface — a 1:1 port of
//! `conduit/internal/server/gql/openapi/openapi.graphql` (frozen snapshot:
//! `tests/contracts/openapi_graphql_schema.graphql`).
//!
//! Every type/field/enum-value name below is verbatim from the snapshot; the
//! SDL contract test in [`crate::contract`] fails on any drift. Naming gotchas
//! handled explicitly (async-graphql's camelCase rename mangles all-caps
//! acronym tags exactly like serde does):
//!
//! * `channelIDs` / `modelIDs` (`channelIds`/`modelIds` would be wrong);
//! * `templateID` / `apiKeyID` on [`LoadApiKeyProfileTemplateInput`]
//!   (note: the `apiKeyQuotaUsages` QUERY argument is `apiKeyId` — lowercase
//!   `d` — which the default rename produces correctly);
//! * enum values are lowercase (`any`, `all_time`, …), not SCREAMING_SNAKE.
//!
//! Where the snapshot's input and output shapes are field-for-field identical
//! (`ModelMapping`, the quota period/duration types) one Rust struct derives
//! both `SimpleObject` and `InputObject` — mirroring gqlgen's binding of both
//! GraphQL types onto a single `objects.*` Go type (`gqlgen.yml`). `APIKeyQuota`
//! needs a separate input struct because the snapshot types `cost` as `Decimal`
//! on output but `DecimalInput` on input; `APIKeyProfiles` vs
//! `UpdateAPIKeyProfilesInput` differ in `profiles` nullability.
//!
//! Intentionally NO `///` doc-comments on GraphQL-visible items except the five
//! description blocks the snapshot carries (two input fields here, plus three
//! root fields in [`crate::resolver`]) — async-graphql exports doc-comments as
//! SDL descriptions and the contract test compares those too.

use async_graphql::{ID, InputObject, SimpleObject};

use crate::scalars::{GqlDecimal, GqlDecimalInput, GqlTime};

// ---------------------------------------------------------------------------
// Enums — snapshot values are lowercase (Go binds them to string newtypes in
// `objects/apikey.go`; the SDL value IS the Go string value).
// ---------------------------------------------------------------------------

// `enum ChannelTagsMatchMode { any all none }`
#[derive(Debug, Clone, Copy, PartialEq, Eq, async_graphql::Enum)]
pub enum ChannelTagsMatchMode {
    #[graphql(name = "any")]
    Any,
    #[graphql(name = "all")]
    All,
    #[graphql(name = "none")]
    None,
}

impl ChannelTagsMatchMode {
    // The Go string value (`objects.ChannelTagsMatchMode*`) — identical to the
    // SDL value on this surface.
    pub const fn as_go_str(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::All => "all",
            Self::None => "none",
        }
    }
}

// `enum APIKeyQuotaPeriodType { all_time past_duration calendar_duration }`
// async-graphql pascal-cases the Rust ident (`Apikey...`), so every
// acronym-bearing type name below is pinned explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, async_graphql::Enum)]
#[graphql(name = "APIKeyQuotaPeriodType")]
pub enum APIKeyQuotaPeriodType {
    #[graphql(name = "all_time")]
    AllTime,
    #[graphql(name = "past_duration")]
    PastDuration,
    #[graphql(name = "calendar_duration")]
    CalendarDuration,
}

impl APIKeyQuotaPeriodType {
    // Go `objects.APIKeyQuotaPeriodType*` string values.
    pub const fn as_go_str(self) -> &'static str {
        match self {
            Self::AllTime => "all_time",
            Self::PastDuration => "past_duration",
            Self::CalendarDuration => "calendar_duration",
        }
    }
}

// `enum APIKeyQuotaPastDurationUnit { minute hour day }`
#[derive(Debug, Clone, Copy, PartialEq, Eq, async_graphql::Enum)]
#[graphql(name = "APIKeyQuotaPastDurationUnit")]
pub enum APIKeyQuotaPastDurationUnit {
    #[graphql(name = "minute")]
    Minute,
    #[graphql(name = "hour")]
    Hour,
    #[graphql(name = "day")]
    Day,
}

// `enum APIKeyQuotaCalendarDurationUnit { day month }`
#[derive(Debug, Clone, Copy, PartialEq, Eq, async_graphql::Enum)]
#[graphql(name = "APIKeyQuotaCalendarDurationUnit")]
pub enum APIKeyQuotaCalendarDurationUnit {
    #[graphql(name = "day")]
    Day,
    #[graphql(name = "month")]
    Month,
}

// ---------------------------------------------------------------------------
// Shared object/input types (identical wire shape on both sides, like Go's
// single objects.* binding).
// ---------------------------------------------------------------------------

// `type ModelMapping { from: String! to: String! }` +
// `input ModelMappingInput { from: String! to: String! }`
#[derive(Debug, Clone, PartialEq, SimpleObject, InputObject)]
#[graphql(name = "ModelMapping", input_name = "ModelMappingInput")]
pub struct ModelMapping {
    pub from: String,
    pub to: String,
}

// `type/input APIKeyQuotaPastDuration(Input) { value: Int! unit: ...! }`
#[derive(Debug, Clone, Copy, PartialEq, Eq, SimpleObject, InputObject)]
#[graphql(
    name = "APIKeyQuotaPastDuration",
    input_name = "APIKeyQuotaPastDurationInput"
)]
pub struct APIKeyQuotaPastDuration {
    pub value: i64,
    pub unit: APIKeyQuotaPastDurationUnit,
}

// `type/input APIKeyQuotaCalendarDuration(Input) { unit: ...! }`
#[derive(Debug, Clone, Copy, PartialEq, Eq, SimpleObject, InputObject)]
#[graphql(
    name = "APIKeyQuotaCalendarDuration",
    input_name = "APIKeyQuotaCalendarDurationInput"
)]
pub struct APIKeyQuotaCalendarDuration {
    pub unit: APIKeyQuotaCalendarDurationUnit,
}

// `type/input APIKeyQuotaPeriod(Input) { type: ...! pastDuration calendarDuration }`
// The `r#type` ident unraws to the exact `type` field name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, SimpleObject, InputObject)]
#[graphql(name = "APIKeyQuotaPeriod", input_name = "APIKeyQuotaPeriodInput")]
pub struct APIKeyQuotaPeriod {
    pub r#type: APIKeyQuotaPeriodType,
    pub past_duration: Option<APIKeyQuotaPastDuration>,
    pub calendar_duration: Option<APIKeyQuotaCalendarDuration>,
}

// ---------------------------------------------------------------------------
// Output object types.
// ---------------------------------------------------------------------------

// `type APIKeyQuota { requests totalTokens cost: Decimal period: ...! }`
#[derive(Debug, Clone, Copy, PartialEq, SimpleObject)]
#[graphql(name = "APIKeyQuota")]
pub struct APIKeyQuota {
    pub requests: Option<i64>,
    pub total_tokens: Option<i64>,
    pub cost: Option<GqlDecimal>,
    pub period: APIKeyQuotaPeriod,
}

// `type APIKeyProfile` — Go `objects.APIKeyProfile`. All selector fields are
// nullable in the SDL (Go slices/pointers with omitempty).
#[derive(Debug, Clone, PartialEq, Default, SimpleObject)]
#[graphql(name = "APIKeyProfile")]
pub struct APIKeyProfile {
    pub name: String,
    pub model_mappings: Option<Vec<ModelMapping>>,
    // All-caps acronym tag: the default rename would emit `channelIds`.
    #[graphql(name = "channelIDs")]
    pub channel_ids: Option<Vec<i64>>,
    pub channel_tags: Option<Vec<String>>,
    pub channel_tags_match_mode: Option<ChannelTagsMatchMode>,
    // All-caps acronym tag: the default rename would emit `modelIds`.
    #[graphql(name = "modelIDs")]
    pub model_ids: Option<Vec<String>>,
    pub valid_from: Option<GqlTime>,
    pub valid_until: Option<GqlTime>,
    pub quota: Option<APIKeyQuota>,
    pub load_balance_strategy: Option<String>,
}

// `type APIKeyProfiles { activeProfile: String! profiles: [APIKeyProfile!] }`
// Note `profiles` is a NULLABLE list here, but NON-NULL on
// `UpdateAPIKeyProfilesInput` — hence two Rust types.
#[derive(Debug, Clone, PartialEq, Default, SimpleObject)]
#[graphql(name = "APIKeyProfiles")]
pub struct APIKeyProfiles {
    pub active_profile: String,
    pub profiles: Option<Vec<APIKeyProfile>>,
}

// `type APIKey { id: ID! key name scopes: [String!] profiles: APIKeyProfiles }`
// — the projection Go builds in `toOpenAPIAPIKey` (`openapi/helper.go:57-69`).
#[derive(Debug, Clone, PartialEq, SimpleObject)]
#[graphql(name = "APIKey")]
pub struct APIKey {
    pub id: ID,
    pub key: String,
    pub name: String,
    pub scopes: Option<Vec<String>>,
    pub profiles: Option<APIKeyProfiles>,
}

// `type APIKeyQuotaUsage { requestCount totalTokens totalCost: Decimal! }`
#[derive(Debug, Clone, Copy, PartialEq, SimpleObject)]
#[graphql(name = "APIKeyQuotaUsage")]
pub struct APIKeyQuotaUsage {
    pub request_count: i64,
    pub total_tokens: i64,
    pub total_cost: GqlDecimal,
}

// `type APIKeyQuotaWindow { start: Time end: Time }`
#[derive(Debug, Clone, Copy, PartialEq, SimpleObject)]
#[graphql(name = "APIKeyQuotaWindow")]
pub struct APIKeyQuotaWindow {
    pub start: Option<GqlTime>,
    pub end: Option<GqlTime>,
}

// `type APIKeyProfileQuotaUsage { profileName quota! window! usage! }`
#[derive(Debug, Clone, PartialEq, SimpleObject)]
#[graphql(name = "APIKeyProfileQuotaUsage")]
pub struct APIKeyProfileQuotaUsage {
    pub profile_name: String,
    pub quota: APIKeyQuota,
    pub window: APIKeyQuotaWindow,
    pub usage: APIKeyQuotaUsage,
}

// ---------------------------------------------------------------------------
// Input-only types.
// ---------------------------------------------------------------------------

// `input APIKeyQuotaInput` — differs from the output `APIKeyQuota` only in the
// `cost` scalar (`DecimalInput` vs `Decimal`); `period` reuses the combined
// type, which renders as `APIKeyQuotaPeriodInput!` in input position.
#[derive(Debug, Clone, Copy, PartialEq, InputObject)]
#[graphql(name = "APIKeyQuotaInput")]
pub struct APIKeyQuotaInput {
    pub requests: Option<i64>,
    pub total_tokens: Option<i64>,
    pub cost: Option<GqlDecimalInput>,
    pub period: APIKeyQuotaPeriod,
}

// `input APIKeyProfileInput` — mirrors `APIKeyProfile` with the input quota.
#[derive(Debug, Clone, PartialEq, InputObject)]
#[graphql(name = "APIKeyProfileInput")]
pub struct APIKeyProfileInput {
    pub name: String,
    pub model_mappings: Option<Vec<ModelMapping>>,
    #[graphql(name = "channelIDs")]
    pub channel_ids: Option<Vec<i64>>,
    pub channel_tags: Option<Vec<String>>,
    pub channel_tags_match_mode: Option<ChannelTagsMatchMode>,
    #[graphql(name = "modelIDs")]
    pub model_ids: Option<Vec<String>>,
    pub valid_from: Option<GqlTime>,
    pub valid_until: Option<GqlTime>,
    pub quota: Option<APIKeyQuotaInput>,
    pub load_balance_strategy: Option<String>,
}

// `input UpdateAPIKeyProfilesInput { activeProfile: String!
//  profiles: [APIKeyProfileInput!]! }` — the list is NON-NULL here.
#[derive(Debug, Clone, PartialEq, InputObject)]
#[graphql(name = "UpdateAPIKeyProfilesInput")]
pub struct UpdateAPIKeyProfilesInput {
    pub active_profile: String,
    pub profiles: Vec<APIKeyProfileInput>,
}

// `input LoadApiKeyProfileTemplateInput` — the two ID fields carry the only
// input-field descriptions in the snapshot, reproduced verbatim below.
#[derive(Debug, Clone, PartialEq, InputObject)]
pub struct LoadApiKeyProfileTemplateInput {
    /// Template to load. Provide exactly one of templateID or templateName;
    /// templateName resolves within the caller's own project.
    #[graphql(name = "templateID")]
    pub template_id: Option<ID>,
    pub template_name: Option<String>,
    /// Target API key. Provide exactly one of apiKeyID or apiKeyName;
    /// apiKeyName resolves within the caller's own project.
    #[graphql(name = "apiKeyID")]
    pub api_key_id: Option<ID>,
    pub api_key_name: Option<String>,
}

// ---------------------------------------------------------------------------
// Input → domain conversions (gqlgen binds inputs directly onto objects.*; the
// Rust port converts explicitly at the resolver boundary).
// ---------------------------------------------------------------------------

impl From<APIKeyQuotaInput> for APIKeyQuota {
    fn from(input: APIKeyQuotaInput) -> Self {
        Self {
            requests: input.requests,
            total_tokens: input.total_tokens,
            // DecimalInput and Decimal share the same underlying value.
            cost: input.cost.map(|c| GqlDecimal(c.0)),
            period: input.period,
        }
    }
}

impl From<APIKeyProfileInput> for APIKeyProfile {
    fn from(input: APIKeyProfileInput) -> Self {
        Self {
            name: input.name,
            model_mappings: input.model_mappings,
            channel_ids: input.channel_ids,
            channel_tags: input.channel_tags,
            channel_tags_match_mode: input.channel_tags_match_mode,
            model_ids: input.model_ids,
            valid_from: input.valid_from,
            valid_until: input.valid_until,
            quota: input.quota.map(APIKeyQuota::from),
            load_balance_strategy: input.load_balance_strategy,
        }
    }
}

impl UpdateAPIKeyProfilesInput {
    /// Coerce omitted `modelMappings` to `[]` on every profile.
    ///
    /// Verbatim port of the loop at the top of Go
    /// `UpdateAPIKeyProfiles` (`openapi/openapi.resolvers.go:38-42`): the admin
    /// UI's Zod schema rejects null for this specific field, so OpenAPI clients
    /// omitting `modelMappings` would otherwise produce rows the UI can't
    /// render. Scoped to the OpenAPI surface only.
    pub fn normalize_model_mappings(&mut self) {
        for profile in &mut self.profiles {
            if profile.model_mappings.is_none() {
                profile.model_mappings = Some(Vec::new());
            }
        }
    }

    /// Convert into the domain shape the api-key service persists — the Rust
    /// analogue of gqlgen decoding `UpdateAPIKeyProfilesInput` straight into
    /// `objects.APIKeyProfiles`.
    pub fn into_profiles(self) -> APIKeyProfiles {
        APIKeyProfiles {
            active_profile: self.active_profile,
            profiles: Some(self.profiles.into_iter().map(APIKeyProfile::from).collect()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Enum literals mirror the Go string constants — the SDL value IS the Go
    /// wire value on this surface (`objects/apikey.go` const blocks).
    #[test]
    fn enum_literals_match_go_constants() {
        assert_eq!(ChannelTagsMatchMode::Any.as_go_str(), "any");
        assert_eq!(ChannelTagsMatchMode::All.as_go_str(), "all");
        assert_eq!(ChannelTagsMatchMode::None.as_go_str(), "none");
        assert_eq!(APIKeyQuotaPeriodType::AllTime.as_go_str(), "all_time");
        assert_eq!(
            APIKeyQuotaPeriodType::PastDuration.as_go_str(),
            "past_duration"
        );
        assert_eq!(
            APIKeyQuotaPeriodType::CalendarDuration.as_go_str(),
            "calendar_duration"
        );
    }

    /// Mirrors the coercion contract exercised by Go
    /// `TestOpenAPIResolver_UpdateAPIKeyProfiles_NormalizesNilModelMappings`:
    /// omitted modelMappings become `[]`, existing ones are untouched.
    #[test]
    fn normalize_model_mappings_coerces_none_to_empty() {
        let mut input = UpdateAPIKeyProfilesInput {
            active_profile: "test".to_string(),
            profiles: vec![
                APIKeyProfileInput {
                    name: "bare".to_string(),
                    model_mappings: None,
                    channel_ids: None,
                    channel_tags: None,
                    channel_tags_match_mode: None,
                    model_ids: None,
                    valid_from: None,
                    valid_until: None,
                    quota: None,
                    load_balance_strategy: None,
                },
                APIKeyProfileInput {
                    name: "mapped".to_string(),
                    model_mappings: Some(vec![ModelMapping {
                        from: "gpt-4".to_string(),
                        to: "gpt-4o".to_string(),
                    }]),
                    channel_ids: None,
                    channel_tags: None,
                    channel_tags_match_mode: None,
                    model_ids: None,
                    valid_from: None,
                    valid_until: None,
                    quota: None,
                    load_balance_strategy: None,
                },
            ],
        };

        input.normalize_model_mappings();

        assert_eq!(input.profiles[0].model_mappings, Some(Vec::new()));
        match &input.profiles[1].model_mappings {
            Some(mappings) => {
                assert_eq!(mappings.len(), 1);
                assert_eq!(mappings[0].from, "gpt-4");
            }
            None => panic!("existing mappings must be preserved"),
        }
    }

    /// into_profiles keeps the non-null list contract (`[APIKeyProfileInput!]!`
    /// → `profiles: Some(...)`) and converts quota cost across the two scalar
    /// newtypes.
    #[test]
    fn into_profiles_converts_quota_cost() -> Result<(), rust_decimal::Error> {
        let cost = "1.5".parse::<rust_decimal::Decimal>()?;
        let input = UpdateAPIKeyProfilesInput {
            active_profile: "P".to_string(),
            profiles: vec![APIKeyProfileInput {
                name: "P".to_string(),
                model_mappings: Some(Vec::new()),
                channel_ids: Some(vec![1, 2]),
                channel_tags: None,
                channel_tags_match_mode: Some(ChannelTagsMatchMode::All),
                model_ids: None,
                valid_from: None,
                valid_until: None,
                quota: Some(APIKeyQuotaInput {
                    requests: Some(10),
                    total_tokens: None,
                    cost: Some(crate::scalars::GqlDecimalInput(cost)),
                    period: APIKeyQuotaPeriod {
                        r#type: APIKeyQuotaPeriodType::AllTime,
                        past_duration: None,
                        calendar_duration: None,
                    },
                }),
                load_balance_strategy: None,
            }],
        };

        let domain = input.into_profiles();
        assert_eq!(domain.active_profile, "P");
        let profiles = domain.profiles.unwrap_or_default();
        assert_eq!(profiles.len(), 1);
        match &profiles[0].quota {
            Some(q) => {
                assert_eq!(q.requests, Some(10));
                assert_eq!(q.cost, Some(GqlDecimal(cost)));
                assert_eq!(q.period.r#type, APIKeyQuotaPeriodType::AllTime);
            }
            None => panic!("quota must survive conversion"),
        }
        assert_eq!(profiles[0].channel_ids, Some(vec![1, 2]));
        Ok(())
    }
}
