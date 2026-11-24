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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BillingEnforcementConfig {
    pub(crate) mode: BillingEnforcementMode,
    project_scope: Option<std::collections::BTreeSet<i64>>,
}

impl BillingEnforcementConfig {
    pub(crate) fn parse(mode: Option<&str>, projects: Option<&str>) -> Result<Self, String> {
        let mode = match mode.unwrap_or("shadow").to_ascii_lowercase().as_str() {
            "shadow" => BillingEnforcementMode::Shadow,
            "soft" | "soft_enforce" => BillingEnforcementMode::SoftEnforce,
            "hard" | "hard_enforce" => BillingEnforcementMode::HardEnforce,
            _ => return Err("invalid CONDUIT_BILLING_ENFORCEMENT_MODE".into()),
        };
        let project_scope = projects.map(|raw| {
            raw.split(',').map(|value| {
                value.trim().parse::<i64>().ok().filter(|id| *id > 0)
                    .ok_or_else(|| "invalid CONDUIT_BILLING_ENFORCEMENT_PROJECT_IDS: expected positive integer list".to_string())
            }).collect::<Result<std::collections::BTreeSet<_>, _>>()
        }).transpose()?;
        Ok(Self {
            mode,
            project_scope,
        })
    }

    pub(crate) fn from_env() -> Result<Self, String> {
        fn read(name: &str) -> Result<Option<String>, String> {
            match std::env::var(name) {
                Ok(value) => Ok(Some(value)),
                Err(std::env::VarError::NotPresent) => Ok(None),
                Err(std::env::VarError::NotUnicode(_)) => {
                    Err(format!("invalid {name}: expected Unicode"))
                }
            }
        }
        let config = Self::parse(
            read("CONDUIT_BILLING_ENFORCEMENT_MODE")?.as_deref(),
            read("CONDUIT_BILLING_ENFORCEMENT_PROJECT_IDS")?.as_deref(),
        )?;
        tracing::info!(
            mode = match config.mode {
                BillingEnforcementMode::Shadow => "shadow",
                BillingEnforcementMode::SoftEnforce => "soft_enforce",
                BillingEnforcementMode::HardEnforce => "hard_enforce",
            },
            project_scope = if config.project_scope.is_some() {
                "list"
            } else {
                "all"
            },
            project_count = config.project_scope.as_ref().map(|ids| ids.len()),
            outside_scope_mode = "shadow",
            "billing enforcement configuration validated"
        );
        Ok(config)
    }

    pub(crate) fn for_project(&self, project_id: i64) -> BillingEnforcementMode {
        if self
            .project_scope
            .as_ref()
            .is_none_or(|ids| ids.contains(&project_id))
        {
            self.mode
        } else {
            BillingEnforcementMode::Shadow
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

#[cfg(test)]
mod enforcement_config_tests {
    use super::*;

    #[test]
    fn defaults_aliases_and_project_scope_are_explicit() {
        let default = BillingEnforcementConfig::parse(None, None).unwrap();
        assert_eq!(default.for_project(9), BillingEnforcementMode::Shadow);
        for (alias, mode) in [
            ("shadow", BillingEnforcementMode::Shadow),
            ("soft", BillingEnforcementMode::SoftEnforce),
            ("SOFT_ENFORCE", BillingEnforcementMode::SoftEnforce),
            ("hard", BillingEnforcementMode::HardEnforce),
            ("HARD_ENFORCE", BillingEnforcementMode::HardEnforce),
        ] {
            let all = BillingEnforcementConfig::parse(Some(alias), None).unwrap();
            assert_eq!(all.for_project(9), mode);
            let scoped = BillingEnforcementConfig::parse(Some(alias), Some(" 9, 2,9 ")).unwrap();
            assert_eq!(scoped.project_scope.as_ref().unwrap().len(), 2);
            assert_eq!(scoped.for_project(9), mode);
            assert_eq!(scoped.for_project(3), BillingEnforcementMode::Shadow);
        }
    }

    #[test]
    fn invalid_configuration_never_silently_disables_enforcement() {
        for mode in ["", " ", "had", "hard "] {
            assert!(BillingEnforcementConfig::parse(Some(mode), None).is_err());
        }
        for projects in [
            "",
            " ",
            ",",
            "1,",
            ",1",
            "1,,2",
            "0",
            "-1",
            "1,no",
            "9223372036854775808",
        ] {
            assert!(BillingEnforcementConfig::parse(Some("hard"), Some(projects)).is_err());
        }
    }
}
