//! Shared helpers for the PostgreSQL quota adapters.

use conduit_admin_graphql::apikey::{
    APIKeyQuota as GqlApiKeyQuota, APIKeyQuotaCalendarDuration as GqlCalendarDuration,
    APIKeyQuotaCalendarDurationUnit as GqlCalendarUnit, APIKeyQuotaPastDuration as GqlPastDuration,
    APIKeyQuotaPastDurationUnit as GqlPastUnit, APIKeyQuotaPeriod as GqlQuotaPeriod,
    APIKeyQuotaPeriodType as GqlPeriodType,
};
use conduit_admin_graphql::node::parse_guid;
use conduit_admin_graphql::scalars::DecimalScalar;
use conduit_core::objects::apikey::{
    APIKeyQuota as CoreApiKeyQuota, api_key_quota_calendar_duration_unit,
    api_key_quota_past_duration_unit, api_key_quota_period_type,
};
use rust_decimal::Decimal;

/// Accept both the GraphQL GUID wire form and the bare numeric form used by
/// internal callers.
pub(crate) fn numeric_id_from_gql(raw: &str) -> Result<i64, String> {
    if let Ok(guid) = parse_guid(raw) {
        return Ok(guid.id);
    }
    raw.parse::<i64>()
        .map_err(|_| format!("invalid id (not a GUID or integer): {raw}"))
}

/// Usage repositories expose cost as integer accounting-currency micros.
pub(crate) fn micros_to_decimal(micros: i64) -> Decimal {
    Decimal::new(micros, 6)
}

/// Map the persisted API-key quota object into the admin GraphQL shape.
pub(crate) fn core_quota_to_gql(core: CoreApiKeyQuota) -> GqlApiKeyQuota {
    let period_type = match core.period.r#type.as_str() {
        api_key_quota_period_type::PAST_DURATION => GqlPeriodType::PastDuration,
        api_key_quota_period_type::CALENDAR_DURATION => GqlPeriodType::CalendarDuration,
        _ => GqlPeriodType::AllTime,
    };

    let past_duration = core.period.past_duration.map(|duration| GqlPastDuration {
        value: duration.value,
        unit: match duration.unit.as_str() {
            api_key_quota_past_duration_unit::HOUR => GqlPastUnit::Hour,
            api_key_quota_past_duration_unit::DAY => GqlPastUnit::Day,
            _ => GqlPastUnit::Minute,
        },
    });

    let calendar_duration = core
        .period
        .calendar_duration
        .map(|duration| GqlCalendarDuration {
            unit: match duration.unit.as_str() {
                api_key_quota_calendar_duration_unit::MONTH => GqlCalendarUnit::Month,
                _ => GqlCalendarUnit::Day,
            },
        });

    GqlApiKeyQuota {
        requests: core.requests,
        total_tokens: core.total_tokens,
        cost: core.cost.map(DecimalScalar),
        period: GqlQuotaPeriod {
            period_type,
            past_duration,
            calendar_duration,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_id_accepts_guid_and_integer() {
        assert_eq!(numeric_id_from_gql("42"), Ok(42));
        assert_eq!(numeric_id_from_gql("gid://conduit/APIKey/42"), Ok(42));
        assert!(numeric_id_from_gql("not-an-id").is_err());
    }
}
