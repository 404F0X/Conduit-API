//! Shared customer-usage charging contract.
//!
//! PostgreSQL owns the persistence implementation. This module keeps only the
//! database-independent contract and usage-row conversion used by the runtime.

use async_trait::async_trait;
use conduit_db::row::UsageLogRow;
use conduit_llm::Usage;
use conduit_orchestrator::orchestrator::BillingAdmissionInput;

#[async_trait]
pub trait UsageChargeSettler: Send + Sync {
    async fn settle_usage(
        &self,
        usage_log: &UsageLogRow,
        usage: &Usage,
        reservation_key: Option<&str>,
    ) -> Result<(), String>;

    /// Returns the reservation key when admission created or reused a
    /// request-scoped reservation. `None` means billing is running in a
    /// non-enforcing/no-op mode for this request.
    async fn reserve_request(
        &self,
        _input: &BillingAdmissionInput,
    ) -> Result<Option<String>, String> {
        Ok(None)
    }

    async fn release_request(&self, _reservation_key: &str, _reason: &str) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BillingEnforcementMode {
    Shadow,
    SoftEnforce,
    HardEnforce,
}

impl BillingEnforcementMode {
    pub(crate) fn from_env() -> Self {
        match std::env::var("CONDUIT_BILLING_ENFORCEMENT_MODE")
            .unwrap_or_else(|_| "shadow".to_string())
            .to_ascii_lowercase()
            .as_str()
        {
            "hard" | "hard_enforce" => Self::HardEnforce,
            "soft" | "soft_enforce" => Self::SoftEnforce,
            _ => Self::Shadow,
        }
    }

    pub(crate) fn for_project(self, project_id: i64) -> Self {
        let Ok(raw) = std::env::var("CONDUIT_BILLING_ENFORCEMENT_PROJECT_IDS") else {
            return self;
        };
        if raw
            .split(',')
            .filter_map(|value| value.trim().parse::<i64>().ok())
            .any(|value| value == project_id)
        {
            self
        } else {
            Self::Shadow
        }
    }
}

pub(crate) fn usage_from_row(row: &UsageLogRow) -> Usage {
    Usage {
        prompt_tokens: row.prompt_tokens.max(0) as u64,
        completion_tokens: row.completion_tokens.max(0) as u64,
        total_tokens: row.total_tokens.max(0) as u64,
        prompt_details: conduit_llm::TokenDetails {
            cached_tokens: row.prompt_cached_tokens.max(0) as u64,
            write_cached_tokens: row.prompt_write_cached_tokens.max(0) as u64,
            write_cached_tokens_5m: row.prompt_write_cached_tokens_5m.max(0) as u64,
            write_cached_tokens_1h: row.prompt_write_cached_tokens_1h.max(0) as u64,
            audio_tokens: row.prompt_audio_tokens.max(0) as u64,
            ..Default::default()
        },
        completion_details: conduit_llm::TokenDetails {
            audio_tokens: row.completion_audio_tokens.max(0) as u64,
            reasoning_tokens: row.completion_reasoning_tokens.max(0) as u64,
            accepted_prediction_tokens: row.completion_accepted_prediction_tokens.max(0) as u64,
            rejected_prediction_tokens: row.completion_rejected_prediction_tokens.max(0) as u64,
            ..Default::default()
        },
        ..Default::default()
    }
}
