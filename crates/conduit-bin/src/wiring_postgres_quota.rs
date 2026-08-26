//! PostgreSQL provider-quota admission for the request routing path.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use sqlx::PgPool;

use conduit_db::{PolicyContext, Principal, RequestContext};
use conduit_orchestrator::candidates::{
    QuotaChannelStatusView, QuotaEnforcementSettings, QuotaLimitStatusView,
};
use conduit_orchestrator::db_candidate_source::QuotaAdmissionSource;
use conduit_services::{
    ProviderQuotaEnforcementMode, SystemService as DomainSystemService, system_key,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredQuotaEnforcementSettings {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    mode: String,
}

pub(crate) struct PgQuotaAdmissionSource {
    pool: PgPool,
    system: Arc<DomainSystemService>,
}

impl PgQuotaAdmissionSource {
    pub(crate) fn new(pool: PgPool, system: Arc<DomainSystemService>) -> Self {
        Self { pool, system }
    }
}

fn system_context() -> RequestContext {
    RequestContext::new(PolicyContext::new(Principal::system()))
}

fn limits_from_quota_data(data: &serde_json::Value) -> Vec<QuotaLimitStatusView> {
    let Some(serde_json::Value::Array(entries)) = data.get("_limits") else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|entry| {
            let value = entry.as_object()?;
            Some(QuotaLimitStatusView {
                limit_type: value
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                status: value
                    .get("status")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                ready: value
                    .get("ready")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
            })
        })
        .collect()
}

#[async_trait]
impl QuotaAdmissionSource for PgQuotaAdmissionSource {
    async fn enforcement_settings(&self) -> QuotaEnforcementSettings {
        let stored: Option<StoredQuotaEnforcementSettings> = self
            .system
            .get_json(&system_context(), system_key::QUOTA_ENFORCEMENT_SETTINGS)
            .await
            .ok()
            .flatten();
        let Some(stored) = stored else {
            return QuotaEnforcementSettings::default();
        };
        QuotaEnforcementSettings {
            enabled: stored.enabled,
            mode: if stored.mode == "de_prioritize" {
                ProviderQuotaEnforcementMode::DePrioritize
            } else {
                ProviderQuotaEnforcementMode::ExhaustedOnly
            },
        }
    }

    async fn quota_statuses(
        &self,
        channel_ids: &[String],
    ) -> BTreeMap<String, QuotaChannelStatusView> {
        let wanted = channel_ids
            .iter()
            .filter_map(|raw| raw.parse::<i64>().ok().map(|id| (id, raw)))
            .collect::<Vec<_>>();
        if wanted.is_empty() {
            return BTreeMap::new();
        }
        let ids = wanted.iter().map(|(id, _)| *id).collect::<Vec<_>>();
        let rows = match sqlx::query_as::<_, (i64, String, serde_json::Value, bool)>(
            "SELECT channel_id,status,COALESCE(quota_data,'{}'::jsonb),ready \
             FROM provider_quota_status WHERE channel_id=ANY($1) AND deleted_at=0",
        )
        .bind(&ids)
        .fetch_all(&self.pool)
        .await
        {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(%error, "PostgreSQL provider quota status read failed; skipping admission");
                return BTreeMap::new();
            }
        };
        rows.into_iter()
            .map(|(channel_id, status, data, ready)| {
                let key = wanted
                    .iter()
                    .find(|(id, _)| *id == channel_id)
                    .map(|(_, raw)| (*raw).clone())
                    .unwrap_or_else(|| channel_id.to_string());
                (
                    key,
                    QuotaChannelStatusView {
                        status,
                        ready,
                        limits: limits_from_quota_data(&data),
                    },
                )
            })
            .collect()
    }
}
