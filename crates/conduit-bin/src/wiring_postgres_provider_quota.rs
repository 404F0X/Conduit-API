//! PostgreSQL adapter for upstream quota and price observations.
//!
//! This module owns administrator-facing probing and observation persistence.
//! Request-routing admission remains in `wiring_postgres_quota`; keeping the
//! two concerns separate prevents an upstream probe from silently becoming a
//! runtime credit limit.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, PgPool, types::Json};

use conduit_admin_graphql::mutation::{
    ChannelQuotaProbeResult, NewApiModelPricingProbe, NewApiPricingProbeResult,
    QuotaEnforcementSettings, QuotaMutationError, QuotaMutationServices,
};
use conduit_admin_graphql::provider_quota_ext::{
    ProviderQuotaStatus as GqlProviderQuotaStatus, ProviderQuotaStatusProviderType,
    ProviderQuotaStatusServices, ProviderQuotaStatusStatus,
};
use conduit_admin_graphql::quota_ext::{
    ApiKeyProfileQuotaUsage, ApiKeyQuotaUsage, ApiKeyQuotaWindow, QuotaQueryError,
    QuotaQueryServices,
};
use conduit_admin_graphql::scalars::{DecimalScalar, MapScalar, QuotaEnforcementMode, TimeScalar};
use conduit_core::objects::apikey::{APIKeyProfiles, APIKeyQuota as CoreApiKeyQuota};
use conduit_core::objects::channel_settings::ChannelCredentials;
use conduit_db::{
    ApiKeyRepo, PgApiKeyRepo, PgUsageRepo, PolicyContext, Principal, RequestContext,
    UsageAggregateQuery, UsageRepo,
};
use conduit_services::{
    QuotaService, QuotaUsageAggregate, QuotaUsageRepo, QuotaWindow,
    SystemService as DomainSystemService, check_target_url, checker_for, detect_provider_from_url,
    provider_quota_checker_http_timeout, system_key,
};

use crate::wiring_quota_common::{core_quota_to_gql, micros_to_decimal, numeric_id_from_gql};

pub(crate) const NEW_API_PROBE_ADAPTER: &str = "new_api";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NewApiQuotaSnapshot {
    pub currency: String,
    pub total: Option<Decimal>,
    pub used: Decimal,
    pub remaining: Option<Decimal>,
    pub balance_source: String,
    pub requires_pat: bool,
    pub unlimited: bool,
    pub unlimited_key_count: usize,
    pub key_count: usize,
    pub verified_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NewApiModelPricingSnapshot {
    pub model_id: String,
    pub billing_kind: String,
    pub quality: String,
    pub group_ratio: Option<Decimal>,
    pub input_per_million: Option<Decimal>,
    pub output_per_million: Option<Decimal>,
    pub cache_read_per_million: Option<Decimal>,
    pub cache_write_per_million: Option<Decimal>,
    pub flat_per_request: Option<Decimal>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NewApiPricingSnapshot {
    pub fetched_at: DateTime<Utc>,
    pub source_endpoint: String,
    pub pricing_version: Option<String>,
    pub account_group: Option<String>,
    pub effective_groups: Vec<String>,
    pub key_count: usize,
    pub matched_key_count: usize,
    pub warnings: Vec<String>,
    pub models: Vec<NewApiModelPricingSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NewApiKeyQuotaSnapshot {
    total: Option<Decimal>,
    used: Decimal,
    remaining: Option<Decimal>,
    unlimited: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NewApiQuotaUnit {
    currency: String,
    multiplier: Decimal,
}

#[derive(Debug, Clone, FromRow)]
struct ProviderChannelRow {
    id: i64,
    name: String,
    channel_type: String,
    base_url: Option<String>,
    credentials: Json<Value>,
    settings: Option<Json<Value>>,
    probe_adapter: Option<String>,
    probe_verified_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredQuotaEnforcementSettings {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    mode: String,
}

#[derive(Debug)]
struct NormalizedQuota {
    status: String,
    quota_data: Value,
    next_reset_at: Option<DateTime<Utc>>,
    ready: bool,
}

pub(crate) struct PgProviderQuotaAdapter {
    pool: PgPool,
    client: reqwest::Client,
    check_interval: Duration,
    system: Arc<DomainSystemService>,
}

impl PgProviderQuotaAdapter {
    pub(crate) fn new(pool: PgPool, system: Arc<DomainSystemService>) -> Self {
        Self {
            pool,
            client: reqwest::Client::new(),
            check_interval: Duration::minutes(5),
            system,
        }
    }

    pub(crate) fn with_interval(mut self, interval: std::time::Duration) -> Self {
        self.check_interval = Duration::from_std(interval).unwrap_or_else(|_| Duration::minutes(5));
        self
    }

    pub(crate) async fn check(&self, force: bool) -> Result<(), String> {
        let rows = sqlx::query_as::<_, ProviderChannelRow>(
            "SELECT c.id,c.name,c.\"type\" AS channel_type,c.base_url,c.credentials,c.settings, \
                    q.probe_adapter,q.probe_verified_at \
             FROM channels c LEFT JOIN provider_quota_status q \
               ON q.channel_id=c.id AND q.deleted_at=0 \
             WHERE c.status='enabled' AND c.deleted_at=0 \
               AND (c.\"type\" IN ('claudecode','codex','github_copilot','nanogpt', \
                    'nanogpt_responses') \
                    OR (q.probe_adapter='new_api' AND q.probe_verified_at IS NOT NULL)) \
               AND ($1 OR q.id IS NULL OR q.next_check_at<=now()) \
             ORDER BY c.id",
        )
        .bind(force)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| error.to_string())?;

        let now = Utc::now();
        for row in rows {
            let verified_adapter = row
                .probe_adapter
                .clone()
                .filter(|_| row.probe_verified_at.is_some());
            let provider = verified_adapter
                .or_else(|| provider_type(&row.channel_type, row.base_url.as_deref()));
            let Some(provider) = provider else { continue };
            match self.check_one(&row, &provider).await {
                Ok(result) => {
                    self.save_status(row.id, &provider, result, now).await?;
                    if provider == NEW_API_PROBE_ADAPTER
                        && crate::wiring_postgres_provider_pricing::observation_due(
                            &self.pool, row.id,
                        )
                        .await
                        .unwrap_or(false)
                        && let Err(error) = self.probe_new_api_pricing(row.id, None, None).await
                    {
                        tracing::warn!(
                            channel_id = row.id,
                            channel_name = %row.name,
                            %error,
                            "scheduled PostgreSQL provider pricing observation failed"
                        );
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        channel_id = row.id,
                        channel_name = %row.name,
                        provider = %provider,
                        %error,
                        "PostgreSQL provider quota check failed"
                    );
                    self.save_error(row.id, &provider, &error, now).await?;
                }
            }
        }
        Ok(())
    }

    async fn check_one(
        &self,
        row: &ProviderChannelRow,
        provider: &str,
    ) -> Result<NormalizedQuota, String> {
        let credentials: ChannelCredentials = serde_json::from_value(row.credentials.0.clone())
            .map_err(|error| format!("invalid credentials: {error}"))?;
        let token =
            access_token(&credentials).ok_or_else(|| "channel has no credentials".to_string())?;
        let mut request = match provider {
            NEW_API_PROBE_ADAPTER => {
                let snapshot = self
                    .probe_new_api_row(
                        row,
                        stored_new_api_pat(&row.credentials.0).as_deref(),
                        stored_new_api_user_id(&row.credentials.0),
                    )
                    .await?;
                return Ok(normalize_new_api_snapshot(&snapshot));
            }
            "claudecode" => {
                let base = row
                    .base_url
                    .as_deref()
                    .unwrap_or("https://api.anthropic.com")
                    .trim_end_matches('/');
                let url = if base.ends_with("/v1") {
                    format!("{base}/messages")
                } else {
                    format!("{base}/v1/messages")
                };
                self.client.post(url).bearer_auth(&token)
                    .header("anthropic-beta", "claude-code-20250219,interleaved-thinking-2025-05-14,redact-thinking-2026-02-12,context-management-2025-06-27,prompt-caching-scope-2026-01-05,mid-conversation-system-2026-04-07,effort-2025-11-24")
                    .header("anthropic-version", "2023-06-01")
                    .header("anthropic-dangerous-direct-browser-access", "true")
                    .header("x-app", "cli")
                    .json(&json!({"model":"claude-haiku-4-5","messages":[{"role":"user","content":"limit"}],"max_tokens":1}))
            }
            "codex" => self
                .client
                .get("https://chatgpt.com/backend-api/wham/usage")
                .bearer_auth(&token),
            "github_copilot" => self
                .client
                .get("https://api.github.com/copilot_internal/user")
                .header("Authorization", format!("token {token}"))
                .header("Accept", "application/json")
                .header("User-Agent", "GitHubCopilotChat/0.26.7"),
            _ => {
                let target = check_target_url(
                    &row.channel_type,
                    row.base_url.as_deref(),
                    row.settings.as_ref().map(|value| &value.0),
                )
                .ok_or_else(|| format!("no quota endpoint for provider {provider}"))?;
                self.client.get(target.url).bearer_auth(&token)
            }
        };
        request = request.timeout(provider_quota_checker_http_timeout());
        let response = request
            .send()
            .await
            .map_err(|error| format!("quota request failed: {error}"))?;
        if !response.status().is_success() {
            return Err(response_error(response).await);
        }
        if provider == "claudecode" {
            return normalize_claude(response.headers());
        }
        let body = response
            .json::<Value>()
            .await
            .map_err(|error| format!("invalid quota response: {error}"))?;
        normalize_json(provider, body)
    }

    pub(crate) async fn probe_new_api_channel(
        &self,
        channel_id: i64,
        verify: bool,
        supplied_pat: Option<&str>,
        supplied_user_id: Option<&str>,
    ) -> Result<NewApiQuotaSnapshot, String> {
        let row = self.load_channel(channel_id).await?;
        let submitted_pat = supplied_pat.map(normalize_new_api_pat).transpose()?;
        let submitted_user_id = supplied_user_id
            .map(normalize_new_api_user_id)
            .transpose()?;
        let stored_pat = stored_new_api_pat(&row.credentials.0);
        let stored_user_id = stored_new_api_user_id(&row.credentials.0);
        let pat = submitted_pat.as_deref().or(stored_pat.as_deref());
        let user_id = submitted_user_id.or(stored_user_id);

        let mut snapshot = match self.probe_new_api_row(&row, pat, user_id).await {
            Ok(snapshot) => snapshot,
            Err(_) if !verify && submitted_pat.is_none() && stored_pat.is_some() => {
                // A revoked stored PAT must expose the PAT-entry state while
                // retaining the independently verified unlimited-key signal.
                match self.probe_new_api_row(&row, None, None).await {
                    Ok(snapshot) => snapshot,
                    Err(error) => {
                        self.persist_probe_error(channel_id, &error).await?;
                        return Err(error);
                    }
                }
            }
            Err(error) => {
                self.persist_probe_error(channel_id, &error).await?;
                return Err(error);
            }
        };
        if verify && snapshot.requires_pat {
            let error = "检测到无限额度 KEY；请先填写 NEW API 用户 PAT 查询账户余额";
            self.persist_probe_error(channel_id, error).await?;
            return Err(error.to_string());
        }
        if submitted_pat.is_some() && snapshot.balance_source == "account" {
            self.save_new_api_account_credentials(channel_id, pat.unwrap_or_default(), user_id)
                .await?;
        }
        let now = Utc::now();
        if verify {
            snapshot.verified_at = Some(now);
        }
        self.save_new_api_snapshot(channel_id, &snapshot, now)
            .await?;
        Ok(snapshot)
    }

    pub(crate) async fn probe_new_api_pricing(
        &self,
        channel_id: i64,
        supplied_pat: Option<&str>,
        supplied_user_id: Option<&str>,
    ) -> Result<NewApiPricingSnapshot, String> {
        let row = self.load_channel(channel_id).await?;
        let result = self
            .probe_new_api_pricing_row(&row, supplied_pat, supplied_user_id)
            .await;
        if let Err(error) = &result
            && let Err(db_error) = crate::wiring_postgres_provider_pricing::record_pricing_failure(
                &self.pool,
                channel_id,
                &[
                    "/api/user/self",
                    "/api/pricing",
                    "/api/prices",
                    "/price",
                    "/api/available_model",
                    "/v1/models",
                ],
                error,
            )
            .await
        {
            return Err(format!(
                "{error}; failed to persist pricing probe failure: {db_error}"
            ));
        }
        result
    }

    async fn probe_new_api_pricing_row(
        &self,
        row: &ProviderChannelRow,
        supplied_pat: Option<&str>,
        supplied_user_id: Option<&str>,
    ) -> Result<NewApiPricingSnapshot, String> {
        if !matches!(row.channel_type.as_str(), "openai" | "openai_responses") {
            return Err("NEW API 价格探测仅支持已确认的 OpenAI 兼容 NEW API 渠道".to_string());
        }
        let base_url = row
            .base_url
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| "channel has no base URL".to_string())?;
        let root = new_api_root(base_url)?;
        let submitted_pat = supplied_pat.map(normalize_new_api_pat).transpose()?;
        let submitted_user_id = supplied_user_id
            .map(normalize_new_api_user_id)
            .transpose()?;
        let stored_pat = stored_new_api_pat(&row.credentials.0);
        let stored_user_id = stored_new_api_user_id(&row.credentials.0);
        let pat = submitted_pat
            .as_deref()
            .or(stored_pat.as_deref())
            .ok_or_else(|| {
                "此渠道尚未保存 NEW API 用户 PAT；请填写 PAT 后再探测实际采购价".to_string()
            })?;
        let user_id = submitted_user_id.or(stored_user_id);

        let credentials: ChannelCredentials = serde_json::from_value(row.credentials.0.clone())
            .map_err(|error| format!("invalid credentials: {error}"))?;
        let keys = credentials
            .get_all_api_keys()
            .filter(|keys| !keys.is_empty())
            .ok_or_else(|| "channel has no API keys".to_string())?;

        let account = self
            .get_new_api_json(&format!("{root}/api/user/self"), pat, user_id)
            .await
            .map_err(|error| format!("NEW API 用户 PAT 验证失败：{error}"))?;
        if account["success"].as_bool() != Some(true) || !account["data"].is_object() {
            return Err("NEW API 用户 PAT 验证失败：上游未返回用户账户数据".to_string());
        }
        let account_group = account["data"]["group"]
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);

        let mut endpoint_errors = Vec::new();
        let mut priced_payload = None;
        for endpoint in ["/api/pricing", "/api/prices", "/price"] {
            match self
                .get_new_api_json(&format!("{root}{endpoint}"), pat, user_id)
                .await
            {
                Ok(payload) if new_api_pricing_shape(&payload) => {
                    priced_payload = Some((payload, endpoint.to_string()));
                    break;
                }
                Ok(_) => endpoint_errors.push(format!("{endpoint}: 返回内容不是 NEW API 价格表")),
                Err(error) => endpoint_errors.push(format!("{endpoint}: {error}")),
            }
        }

        if priced_payload.is_none() {
            let (source_endpoint, model_ids, mut catalog_warnings) = self
                .fetch_unpriced_model_catalog(&root, pat, user_id, &keys[0])
                .await
                .map_err(|error| {
                    format!(
                        "上游价格与模型目录接口均不可用：{}; {error}",
                        endpoint_errors.join("; ")
                    )
                })?;
            catalog_warnings.extend(endpoint_errors);
            catalog_warnings
                .push("上游只返回模型目录，未返回可验证价格；这些模型不会被当作零成本".into());
            let snapshot = NewApiPricingSnapshot {
                fetched_at: Utc::now(),
                source_endpoint: source_endpoint.clone(),
                pricing_version: None,
                account_group: account_group.clone(),
                effective_groups: account_group.clone().into_iter().collect(),
                key_count: keys.len(),
                matched_key_count: 0,
                warnings: catalog_warnings,
                models: model_ids
                    .into_iter()
                    .map(|model_id| NewApiModelPricingSnapshot {
                        model_id,
                        billing_kind: "unknown".into(),
                        quality: "unavailable".into(),
                        group_ratio: None,
                        input_per_million: None,
                        output_per_million: None,
                        cache_read_per_million: None,
                        cache_write_per_million: None,
                        flat_per_request: None,
                        reason: Some("模型存在，但上游未提供可验证价格".into()),
                    })
                    .collect(),
            };
            if submitted_pat.is_some() {
                self.save_new_api_account_credentials(row.id, pat, user_id)
                    .await?;
            }
            crate::wiring_postgres_provider_pricing::record_new_api_pricing_snapshot(
                &self.pool,
                row.id,
                &source_endpoint,
                &snapshot,
            )
            .await?;
            return Ok(snapshot);
        }
        let (pricing, source_endpoint) = priced_payload
            .ok_or_else(|| "upstream pricing payload became unavailable".to_string())?;

        let token_rows = self.list_new_api_tokens(&root, pat, user_id).await?;
        let mut effective_groups = BTreeSet::new();
        let mut matched_key_count = 0usize;
        let mut group_resolution_estimated = false;
        let mut warnings = Vec::new();
        for key in &keys {
            let expected_mask = mask_new_api_token(key);
            let matches = token_rows
                .iter()
                .filter(|token| token["key"].as_str() == Some(expected_mask.as_str()))
                .collect::<Vec<_>>();
            if matches.len() != 1 {
                group_resolution_estimated = true;
                warnings.push(if matches.is_empty() {
                    "有渠道 KEY 无法在 PAT 账户中匹配，已按账户默认分组估算".to_string()
                } else {
                    "PAT 账户中存在相同掩码的 KEY，无法唯一确定分组，已按账户默认分组估算"
                        .to_string()
                });
                if let Some(group) = account_group.as_ref() {
                    effective_groups.insert(group.clone());
                }
                continue;
            }
            matched_key_count += 1;
            let token = matches[0];
            let group = token["group"].as_str().unwrap_or_default().trim();
            if group == "auto" {
                group_resolution_estimated = true;
                if let Some(groups) = token["auto_groups"].as_array() {
                    for group in groups.iter().filter_map(Value::as_str) {
                        if !group.trim().is_empty() {
                            effective_groups.insert(group.trim().to_string());
                        }
                    }
                }
                warnings.push(
                    "渠道包含 NEW API auto 分组 KEY；价格按其可用分组中的最高倍率估算".into(),
                );
            } else if !group.is_empty() {
                effective_groups.insert(group.to_string());
            } else if let Some(group) = account_group.as_ref() {
                effective_groups.insert(group.clone());
            }
        }
        if effective_groups.is_empty()
            && let Some(group) = account_group.as_ref()
        {
            effective_groups.insert(group.clone());
            group_resolution_estimated = true;
            warnings.push("未解析到渠道 KEY 分组，已使用账户默认分组".into());
        }
        warnings.sort();
        warnings.dedup();

        let group_ratios = pricing["group_ratio"]
            .as_object()
            .map(|object| {
                object
                    .iter()
                    .filter_map(|(group, value)| {
                        json_decimal(value).map(|ratio| (group.clone(), ratio))
                    })
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default();
        let models = pricing["data"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|item| {
                normalize_new_api_pricing_item(
                    item,
                    &effective_groups,
                    &group_ratios,
                    group_resolution_estimated,
                )
            })
            .collect();

        if submitted_pat.is_some() {
            self.save_new_api_account_credentials(row.id, pat, user_id)
                .await?;
        }
        let snapshot = NewApiPricingSnapshot {
            fetched_at: Utc::now(),
            source_endpoint: source_endpoint.clone(),
            pricing_version: pricing["pricing_version"].as_str().map(ToOwned::to_owned),
            account_group,
            effective_groups: effective_groups.into_iter().collect(),
            key_count: keys.len(),
            matched_key_count,
            warnings,
            models,
        };
        crate::wiring_postgres_provider_pricing::record_new_api_pricing_snapshot(
            &self.pool,
            row.id,
            &source_endpoint,
            &snapshot,
        )
        .await?;
        Ok(snapshot)
    }

    async fn load_channel(&self, channel_id: i64) -> Result<ProviderChannelRow, String> {
        sqlx::query_as::<_, ProviderChannelRow>(
            "SELECT c.id,c.name,c.\"type\" AS channel_type,c.base_url,c.credentials,c.settings, \
                    q.probe_adapter,q.probe_verified_at \
             FROM channels c LEFT JOIN provider_quota_status q \
               ON q.channel_id=c.id AND q.deleted_at=0 \
             WHERE c.id=$1 AND c.deleted_at=0 LIMIT 1",
        )
        .bind(channel_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("channel {channel_id} not found"))
    }

    async fn fetch_unpriced_model_catalog(
        &self,
        root: &str,
        pat: &str,
        user_id: Option<i64>,
        channel_key: &str,
    ) -> Result<(String, Vec<String>, Vec<String>), String> {
        let mut warnings = Vec::new();
        for endpoint in ["/api/available_model", "/api/prices", "/price"] {
            match self
                .get_new_api_json(&format!("{root}{endpoint}"), pat, user_id)
                .await
            {
                Ok(payload) => {
                    let models = model_ids_from_catalog(&payload);
                    if !models.is_empty() {
                        return Ok((endpoint.to_string(), models, warnings));
                    }
                    warnings.push(format!("{endpoint}: 未发现模型条目"));
                }
                Err(error) => warnings.push(format!("{endpoint}: {error}")),
            }
        }
        let endpoint = "/v1/models";
        let payload = self
            .get_new_api_json(&format!("{root}{endpoint}"), channel_key, None)
            .await
            .map_err(|error| format!("{endpoint}: {error}"))?;
        let models = model_ids_from_catalog(&payload);
        if models.is_empty() {
            return Err(format!("{endpoint}: 未发现模型条目"));
        }
        Ok((endpoint.to_string(), models, warnings))
    }

    async fn list_new_api_tokens(
        &self,
        root: &str,
        pat: &str,
        user_id: Option<i64>,
    ) -> Result<Vec<Value>, String> {
        let mut rows = Vec::new();
        for page in 1..=20 {
            let response = self
                .get_new_api_json(
                    &format!("{root}/api/token/?p={page}&size=100"),
                    pat,
                    user_id,
                )
                .await
                .map_err(|error| format!("无法读取 NEW API KEY 分组：{error}"))?;
            if response["success"].as_bool() != Some(true) {
                return Err("NEW API KEY 列表接口返回失败".to_string());
            }
            let data = &response["data"];
            let items = data["items"]
                .as_array()
                .ok_or_else(|| "NEW API KEY 列表缺少 items".to_string())?;
            rows.extend(items.iter().cloned());
            let total = data["total"].as_u64().unwrap_or(rows.len() as u64) as usize;
            if rows.len() >= total || items.is_empty() {
                break;
            }
        }
        Ok(rows)
    }

    async fn probe_new_api_row(
        &self,
        row: &ProviderChannelRow,
        new_api_pat: Option<&str>,
        new_api_user_id: Option<i64>,
    ) -> Result<NewApiQuotaSnapshot, String> {
        if !matches!(row.channel_type.as_str(), "openai" | "openai_responses") {
            return Err("暂时只支持查询 NEW API 的 KEY 额度；该渠道不是 OpenAI 兼容渠道".into());
        }
        let base_url = row
            .base_url
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| "channel has no base URL".to_string())?;
        let credentials: ChannelCredentials = serde_json::from_value(row.credentials.0.clone())
            .map_err(|error| format!("invalid credentials: {error}"))?;
        let keys = credentials
            .get_all_api_keys()
            .filter(|keys| !keys.is_empty())
            .ok_or_else(|| "channel has no API keys".to_string())?;
        let root = new_api_root(base_url)?;
        let quota_unit = self.probe_new_api_quota_unit(&root).await?;
        let mut total = Decimal::ZERO;
        let mut used = Decimal::ZERO;
        let mut remaining = Decimal::ZERO;
        let mut unlimited_key_count = 0;
        for key in &keys {
            let key_snapshot = self
                .probe_new_api_key(&root, key, quota_unit.multiplier)
                .await?;
            used += key_snapshot.used;
            if key_snapshot.unlimited {
                unlimited_key_count += 1;
            } else {
                total += key_snapshot.total.unwrap_or_default();
                remaining += key_snapshot.remaining.unwrap_or_default();
            }
        }
        let unlimited = unlimited_key_count > 0;
        let mut balance_source = "key".to_string();
        let mut requires_pat = unlimited;
        if unlimited && let Some(pat) = new_api_pat {
            let account = self
                .probe_new_api_account(&root, pat, new_api_user_id, quota_unit.multiplier)
                .await?;
            total = account.0;
            used = account.1;
            remaining = account.2;
            balance_source = "account".to_string();
            requires_pat = false;
        }
        Ok(NewApiQuotaSnapshot {
            currency: quota_unit.currency,
            total: (!requires_pat).then(|| total.normalize()),
            used: used.normalize(),
            remaining: (!requires_pat).then(|| remaining.normalize()),
            balance_source,
            requires_pat,
            unlimited,
            unlimited_key_count,
            key_count: keys.len(),
            verified_at: None,
        })
    }

    async fn probe_new_api_account(
        &self,
        root: &str,
        pat: &str,
        user_id: Option<i64>,
        multiplier: Decimal,
    ) -> Result<(Decimal, Decimal, Decimal), String> {
        let response = self
            .get_new_api_json(&format!("{root}/api/user/self"), pat, user_id)
            .await
            .map_err(|error| format!("NEW API 用户 PAT 验证失败：{error}"))?;
        if response["success"].as_bool() != Some(true) || !response["data"].is_object() {
            return Err("NEW API 用户 PAT 验证失败：上游未返回用户账户数据".to_string());
        }
        let data = &response["data"];
        let remaining = json_decimal(&data["quota"])
            .ok_or_else(|| "NEW API 用户数据缺少 quota".to_string())?
            * multiplier;
        let used = json_decimal(&data["used_quota"])
            .ok_or_else(|| "NEW API 用户数据缺少 used_quota".to_string())?
            * multiplier;
        Ok((
            (remaining + used).normalize(),
            used.normalize(),
            remaining.normalize(),
        ))
    }

    async fn save_new_api_account_credentials(
        &self,
        channel_id: i64,
        pat: &str,
        user_id: Option<i64>,
    ) -> Result<(), String> {
        let mut tx = self.pool.begin().await.map_err(|error| error.to_string())?;
        let Json(mut credentials) = sqlx::query_scalar::<_, Json<Value>>(
            "SELECT credentials FROM channels WHERE id=$1 AND deleted_at=0 FOR UPDATE",
        )
        .bind(channel_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("channel {channel_id} not found"))?;
        let object = credentials
            .as_object_mut()
            .ok_or_else(|| "invalid channel credentials object".to_string())?;
        object.insert("newApiPat".into(), Value::String(pat.to_string()));
        if let Some(user_id) = user_id {
            object.insert("newApiUserID".into(), Value::String(user_id.to_string()));
        }
        sqlx::query("UPDATE channels SET credentials=$1,updated_at=now() WHERE id=$2")
            .bind(Json(credentials))
            .bind(channel_id)
            .execute(&mut *tx)
            .await
            .map_err(|error| error.to_string())?;
        tx.commit().await.map_err(|error| error.to_string())?;
        Ok(())
    }

    async fn probe_new_api_quota_unit(&self, root: &str) -> Result<NewApiQuotaUnit, String> {
        let status = self
            .get_new_api_json(&format!("{root}/api/status"), "", None)
            .await
            .map_err(|error| {
                format!(
                    "暂时只支持查询 NEW API 的 KEY 额度；上游未提供 NEW API /api/status：{error}"
                )
            })?;
        let data = status
            .get("data")
            .filter(|value| value.is_object())
            .ok_or_else(|| {
                "暂时只支持查询 NEW API 的 KEY 额度；/api/status 缺少 data".to_string()
            })?;
        let quota_per_unit = json_decimal(&data["quota_per_unit"])
            .filter(|value| *value > Decimal::ZERO)
            .ok_or_else(|| "NEW API status response has invalid quota_per_unit".to_string())?;
        let display_type = data["quota_display_type"]
            .as_str()
            .unwrap_or("USD")
            .trim()
            .to_ascii_uppercase();
        let (currency, multiplier) = match display_type.as_str() {
            "TOKENS" => ("TOKENS".to_string(), Decimal::ONE),
            "CNY" => {
                let rate = json_decimal(&data["usd_exchange_rate"])
                    .filter(|value| *value > Decimal::ZERO)
                    .ok_or_else(|| {
                        "NEW API status response has invalid usd_exchange_rate".to_string()
                    })?;
                ("CNY".to_string(), rate / quota_per_unit)
            }
            "CUSTOM" => {
                let rate = json_decimal(&data["custom_currency_exchange_rate"])
                    .filter(|value| *value > Decimal::ZERO)
                    .unwrap_or(Decimal::ONE);
                let symbol = data["custom_currency_symbol"]
                    .as_str()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .unwrap_or("CUSTOM");
                (symbol.to_string(), rate / quota_per_unit)
            }
            _ => ("USD".to_string(), Decimal::ONE / quota_per_unit),
        };
        Ok(NewApiQuotaUnit {
            currency,
            multiplier,
        })
    }

    async fn probe_new_api_key(
        &self,
        root: &str,
        key: &str,
        multiplier: Decimal,
    ) -> Result<NewApiKeyQuotaSnapshot, String> {
        let response = self
            .get_new_api_json(&format!("{root}/api/usage/token/"), key, None)
            .await?;
        let data = &response["data"];
        if response["code"].as_bool() != Some(true) || !data.is_object() {
            return Err(
                "上游不支持 NEW API KEY 额度接口；暂时只支持查询 NEW API 的 KEY 额度".to_string(),
            );
        }
        let unlimited = data["unlimited_quota"].as_bool() == Some(true);
        let used = json_decimal(&data["total_used"])
            .ok_or_else(|| "NEW API token-usage response has no total_used".to_string())?
            * multiplier;
        if unlimited {
            return Ok(NewApiKeyQuotaSnapshot {
                total: None,
                used: used.normalize(),
                remaining: None,
                unlimited: true,
            });
        }
        let total = json_decimal(&data["total_granted"])
            .ok_or_else(|| "NEW API token-usage response has no total_granted".to_string())?
            * multiplier;
        let remaining = json_decimal(&data["total_available"])
            .ok_or_else(|| "NEW API token-usage response has no total_available".to_string())?
            * multiplier;
        Ok(NewApiKeyQuotaSnapshot {
            total: Some(total.normalize()),
            used: used.normalize(),
            remaining: Some(remaining.normalize()),
            unlimited: false,
        })
    }

    async fn get_new_api_json(
        &self,
        url: &str,
        key: &str,
        user_id: Option<i64>,
    ) -> Result<Value, String> {
        let mut request = self
            .client
            .get(url)
            .timeout(provider_quota_checker_http_timeout());
        if !key.is_empty() {
            request = request.bearer_auth(key);
        }
        if let Some(user_id) = user_id {
            request = request.header("New-Api-User", user_id.to_string());
        }
        let response = request
            .send()
            .await
            .map_err(|error| format!("quota request failed: {error}"))?;
        if !response.status().is_success() {
            return Err(response_error(response).await);
        }
        response
            .json::<Value>()
            .await
            .map_err(|error| format!("invalid NEW API quota response: {error}"))
    }

    async fn persist_probe_error(&self, channel_id: i64, error: &str) -> Result<(), String> {
        self.save_error(channel_id, NEW_API_PROBE_ADAPTER, error, Utc::now())
            .await
    }

    async fn save_new_api_snapshot(
        &self,
        channel_id: i64,
        snapshot: &NewApiQuotaSnapshot,
        now: DateTime<Utc>,
    ) -> Result<(), String> {
        let normalized = normalize_new_api_snapshot(snapshot);
        let mut tx = self.pool.begin().await.map_err(|error| error.to_string())?;
        sqlx::query(
            "INSERT INTO provider_quota_status \
             (channel_id,provider_type,status,quota_data,ready,next_check_at, \
              probe_adapter,probe_verified_at) \
             VALUES($1,'new_api',$2,$3,$4,$5,'new_api',$6) \
             ON CONFLICT(channel_id) DO UPDATE SET \
               provider_type='new_api',status=EXCLUDED.status,quota_data=EXCLUDED.quota_data, \
               ready=EXCLUDED.ready,next_check_at=EXCLUDED.next_check_at, \
               probe_adapter='new_api',probe_verified_at=EXCLUDED.probe_verified_at, \
               updated_at=now(),deleted_at=0",
        )
        .bind(channel_id)
        .bind(&normalized.status)
        .bind(Json(normalized.quota_data.clone()))
        .bind(normalized.ready)
        .bind(now + self.check_interval)
        .bind(snapshot.verified_at)
        .execute(&mut *tx)
        .await
        .map_err(|error| error.to_string())?;
        sqlx::query(
            "INSERT INTO provider_quota_observations \
             (channel_id,provider_type,probe_adapter,status,success,currency,total,used, \
              remaining,unlimited,balance_source,quota_data,observed_at) \
             VALUES($1,'new_api','new_api',$2,TRUE,$3,$4,$5,$6,$7,$8,$9,$10)",
        )
        .bind(channel_id)
        .bind(&normalized.status)
        .bind(&snapshot.currency)
        .bind(snapshot.total.map(|value| value.normalize().to_string()))
        .bind(snapshot.used.normalize().to_string())
        .bind(
            snapshot
                .remaining
                .map(|value| value.normalize().to_string()),
        )
        .bind(snapshot.unlimited)
        .bind(&snapshot.balance_source)
        .bind(Json(normalized.quota_data))
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(|error| error.to_string())?;
        tx.commit().await.map_err(|error| error.to_string())?;
        Ok(())
    }

    async fn save_status(
        &self,
        channel_id: i64,
        provider: &str,
        result: NormalizedQuota,
        now: DateTime<Utc>,
    ) -> Result<(), String> {
        let interval = if result.status == "warning" {
            self.check_interval * 4
        } else {
            self.check_interval
        };
        let mut tx = self.pool.begin().await.map_err(|error| error.to_string())?;
        sqlx::query(
            "INSERT INTO provider_quota_status \
             (channel_id,provider_type,status,quota_data,next_reset_at,ready,next_check_at) \
             VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT(channel_id) DO UPDATE SET \
               provider_type=EXCLUDED.provider_type,status=EXCLUDED.status, \
               quota_data=EXCLUDED.quota_data,next_reset_at=EXCLUDED.next_reset_at, \
               ready=EXCLUDED.ready,next_check_at=EXCLUDED.next_check_at, \
               updated_at=now(),deleted_at=0",
        )
        .bind(channel_id)
        .bind(provider)
        .bind(&result.status)
        .bind(Json(result.quota_data.clone()))
        .bind(result.next_reset_at)
        .bind(result.ready)
        .bind(now + interval)
        .execute(&mut *tx)
        .await
        .map_err(|error| error.to_string())?;
        sqlx::query(
            "INSERT INTO provider_quota_observations \
             (channel_id,provider_type,status,success,quota_data,observed_at) \
             VALUES($1,$2,$3,TRUE,$4,$5)",
        )
        .bind(channel_id)
        .bind(provider)
        .bind(&result.status)
        .bind(Json(result.quota_data))
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(|error| error.to_string())?;
        tx.commit().await.map_err(|error| error.to_string())?;
        Ok(())
    }

    async fn save_error(
        &self,
        channel_id: i64,
        provider: &str,
        error: &str,
        now: DateTime<Utc>,
    ) -> Result<(), String> {
        let error_data = json!({"error": error});
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|db_error| db_error.to_string())?;
        sqlx::query(
            "INSERT INTO provider_quota_status \
             (channel_id,provider_type,status,quota_data,ready,next_check_at) \
             VALUES($1,$2,'unknown',$3,FALSE,$4) ON CONFLICT(channel_id) DO UPDATE SET \
               provider_type=EXCLUDED.provider_type,status='unknown', \
               quota_data=COALESCE(provider_quota_status.quota_data,'{}'::jsonb)||EXCLUDED.quota_data, \
               ready=FALSE,next_check_at=EXCLUDED.next_check_at,updated_at=now(),deleted_at=0",
        )
        .bind(channel_id)
        .bind(provider)
        .bind(Json(error_data.clone()))
        .bind(now + self.check_interval)
        .execute(&mut *tx)
        .await
        .map_err(|db_error| db_error.to_string())?;
        sqlx::query(
            "INSERT INTO provider_quota_observations \
             (channel_id,provider_type,probe_adapter,status,success,error_message,quota_data,observed_at) \
             VALUES($1,$2,$3,'unknown',FALSE,$4,$5,$6)",
        )
        .bind(channel_id)
        .bind(provider)
        .bind((provider == NEW_API_PROBE_ADAPTER).then_some(NEW_API_PROBE_ADAPTER))
        .bind(error)
        .bind(Json(error_data))
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(|db_error| db_error.to_string())?;
        tx.commit().await.map_err(|db_error| db_error.to_string())?;
        Ok(())
    }

    async fn read_enforcement_settings(&self) -> Result<QuotaEnforcementSettings, String> {
        let stored = self
            .system
            .get_json::<StoredQuotaEnforcementSettings>(
                &system_context(),
                system_key::QUOTA_ENFORCEMENT_SETTINGS,
            )
            .await
            .map_err(|error| error.to_string())?;
        Ok(
            stored.map_or_else(QuotaEnforcementSettings::default, |stored| {
                QuotaEnforcementSettings {
                    enabled: stored.enabled,
                    mode: mode_from_wire(&stored.mode),
                }
            }),
        )
    }

    async fn run_new_api_probe(
        &self,
        channel_id: &str,
        verify: bool,
        new_api_pat: Option<&str>,
        new_api_user_id: Option<&str>,
    ) -> Result<ChannelQuotaProbeResult, QuotaMutationError> {
        let numeric = numeric_id_from_gql(channel_id).map_err(QuotaMutationError::Check)?;
        match self
            .probe_new_api_channel(numeric, verify, new_api_pat, new_api_user_id)
            .await
        {
            Ok(snapshot) => Ok(ChannelQuotaProbeResult {
                success: true,
                adapter: Some(NEW_API_PROBE_ADAPTER.to_string()),
                message: if snapshot.requires_pat {
                    "检测到 NEW API 无限额度 KEY；1 亿 USD 是兼容占位值，请填写用户 PAT 查询真实账户余额".into()
                } else if verify {
                    "已二次探测并启用自动额度刷新".into()
                } else if snapshot.balance_source == "account" {
                    "已通过独立 PAT 读取 NEW API 用户账户余额，请与上游后台核对后再确认".into()
                } else {
                    "已读取 NEW API KEY 额度，请与上游后台核对后再确认".into()
                },
                currency: Some(snapshot.currency),
                total: snapshot.total.map(|value| value.normalize().to_string()),
                used: Some(snapshot.used.normalize().to_string()),
                remaining: snapshot
                    .remaining
                    .map(|value| value.normalize().to_string()),
                balance_source: Some(snapshot.balance_source),
                requires_pat: snapshot.requires_pat,
                unlimited: snapshot.unlimited,
                unlimited_key_count: i32::try_from(snapshot.unlimited_key_count)
                    .unwrap_or(i32::MAX),
                key_count: i32::try_from(snapshot.key_count).unwrap_or(i32::MAX),
                verified: snapshot.verified_at.is_some(),
                verified_at: snapshot.verified_at.map(|value| value.to_rfc3339()),
            }),
            Err(error) => Ok(ChannelQuotaProbeResult {
                success: false,
                adapter: None,
                message: format!("NEW API 额度探测失败：{error}"),
                currency: None,
                total: None,
                used: None,
                remaining: None,
                balance_source: None,
                requires_pat: false,
                unlimited: false,
                unlimited_key_count: 0,
                key_count: 0,
                verified: false,
                verified_at: None,
            }),
        }
    }
}

/// PostgreSQL bridge used by `QuotaService` for API-key profile usage. Request
/// counts come from the pre-admission ledger so concurrent requests cannot all
/// observe the same stale count; token and cost totals come from usage logs.
struct PgQuotaUsageBridge {
    usage_repo: Arc<PgUsageRepo>,
    pool: PgPool,
    context: RequestContext,
    project_id: String,
}

impl PgQuotaUsageBridge {
    fn lower_query(&self, api_key_id: &str, window: &QuotaWindow) -> UsageAggregateQuery {
        let start_at = window.start.map(|value| value.to_rfc3339());
        let end_at = window.end.map(|value| {
            let bound = if window.end_inclusive {
                value
            } else {
                value - Duration::microseconds(1)
            };
            bound.to_rfc3339()
        });
        UsageAggregateQuery {
            project_id: self.project_id.clone(),
            start_at,
            end_at,
            api_key_id: Some(api_key_id.to_string()),
            channel_id: None,
            model_id: None,
            source: None,
        }
    }
}

#[async_trait]
impl QuotaUsageRepo for PgQuotaUsageBridge {
    async fn request_count(
        &self,
        api_key_id: &str,
        window: &QuotaWindow,
    ) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
        let project_id = self
            .project_id
            .parse::<i64>()
            .map_err(|error| Box::<dyn std::error::Error + Send + Sync>::from(error.to_string()))?;
        let api_key_id = api_key_id
            .parse::<i64>()
            .map_err(|error| Box::<dyn std::error::Error + Send + Sync>::from(error.to_string()))?;
        let count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*)::BIGINT FROM api_key_quota_admissions \
             WHERE project_id=$1 AND api_key_id=$2 \
               AND ($3::timestamptz IS NULL OR created_at >= $3) \
               AND ($4::timestamptz IS NULL \
                    OR ($5 AND created_at <= $4) \
                    OR (NOT $5 AND created_at < $4))",
        )
        .bind(project_id)
        .bind(api_key_id)
        .bind(window.start)
        .bind(window.end)
        .bind(window.end_inclusive)
        .fetch_one(&self.pool)
        .await
        .map_err(|error| Box::<dyn std::error::Error + Send + Sync>::from(error.to_string()))?;
        Ok(u64::try_from(count).unwrap_or(0))
    }

    async fn usage_aggregate(
        &self,
        api_key_id: &str,
        window: &QuotaWindow,
    ) -> Result<QuotaUsageAggregate, Box<dyn std::error::Error + Send + Sync>> {
        let aggregate = self
            .usage_repo
            .aggregate_usage_unchecked(&self.context, self.lower_query(api_key_id, window))
            .await
            .map_err(|error| Box::<dyn std::error::Error + Send + Sync>::from(error.to_string()))?;
        Ok(QuotaUsageAggregate {
            requests: aggregate.request_count,
            tokens: aggregate.total_tokens,
            cost: micros_to_decimal(aggregate.total_cost_micros),
        })
    }
}

#[async_trait]
impl QuotaQueryServices for PgProviderQuotaAdapter {
    async fn quota_enforcement_settings(
        &self,
    ) -> Result<QuotaEnforcementSettings, QuotaQueryError> {
        self.read_enforcement_settings()
            .await
            .map_err(QuotaQueryError::EnforcementSettings)
    }

    async fn api_key_quota_usages(
        &self,
        api_key_id: &str,
    ) -> Result<Vec<ApiKeyProfileQuotaUsage>, QuotaQueryError> {
        let numeric = numeric_id_from_gql(api_key_id).map_err(QuotaQueryError::GetApiKey)?;
        let numeric_string = numeric.to_string();
        let context = system_context();
        let row = PgApiKeyRepo::new(self.pool.clone())
            .find_api_key_by_id(&context, &numeric_string)
            .await
            .map_err(|error| QuotaQueryError::GetApiKey(error.to_string()))?
            .ok_or_else(|| QuotaQueryError::GetApiKey(format!("api key {numeric} not found")))?;
        let profiles: APIKeyProfiles = serde_json::from_value(row.profiles)
            .map_err(|error| QuotaQueryError::ProfileQuotaUsages(error.to_string()))?;
        let profile_quotas: Vec<(String, Option<CoreApiKeyQuota>)> = profiles
            .profiles
            .into_iter()
            .map(|profile| (profile.name, profile.quota))
            .collect();
        let bridge = PgQuotaUsageBridge {
            usage_repo: Arc::new(PgUsageRepo::new(self.pool.clone())),
            pool: self.pool.clone(),
            context,
            project_id: row.project_id,
        };
        let offset = crate::wiring::resolve_timezone_offset(&self.system).await;
        let usages = QuotaService::new()
            .profile_quota_usages(
                &bridge,
                &numeric_string,
                &profile_quotas,
                Utc::now(),
                offset,
            )
            .await
            .map_err(|error| QuotaQueryError::ProfileQuotaUsages(error.to_string()))?;

        Ok(usages
            .into_iter()
            .map(|usage| ApiKeyProfileQuotaUsage {
                profile_name: usage.profile_name,
                quota: core_quota_to_gql(usage.quota),
                window: ApiKeyQuotaWindow {
                    start: usage.window.start.map(TimeScalar),
                    end: usage.window.end.map(TimeScalar),
                },
                usage: ApiKeyQuotaUsage {
                    request_count: i64::try_from(usage.usage.requests).unwrap_or(i64::MAX),
                    total_tokens: i64::try_from(usage.usage.tokens).unwrap_or(i64::MAX),
                    total_cost: DecimalScalar(usage.usage.cost),
                },
            })
            .collect())
    }
}

impl conduit_scheduler::ProviderQuotaCheckExecutor for PgProviderQuotaAdapter {
    fn check_due_channels(&self) -> Result<(), String> {
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(self.check(false))
        })
    }
}

#[async_trait]
impl QuotaMutationServices for PgProviderQuotaAdapter {
    async fn manual_check(&self) -> Result<(), QuotaMutationError> {
        self.check(true).await.map_err(QuotaMutationError::Check)
    }

    async fn probe_channel_quota(
        &self,
        channel_id: &str,
        new_api_pat: Option<&str>,
        new_api_user_id: Option<&str>,
    ) -> Result<ChannelQuotaProbeResult, QuotaMutationError> {
        self.run_new_api_probe(channel_id, false, new_api_pat, new_api_user_id)
            .await
    }

    async fn confirm_channel_quota_probe(
        &self,
        channel_id: &str,
    ) -> Result<ChannelQuotaProbeResult, QuotaMutationError> {
        self.run_new_api_probe(channel_id, true, None, None).await
    }

    async fn probe_new_api_pricing(
        &self,
        channel_id: &str,
        new_api_pat: Option<&str>,
        new_api_user_id: Option<&str>,
    ) -> Result<NewApiPricingProbeResult, QuotaMutationError> {
        let channel_id = numeric_id_from_gql(channel_id).map_err(QuotaMutationError::Check)?;
        let snapshot = PgProviderQuotaAdapter::probe_new_api_pricing(
            self,
            channel_id,
            new_api_pat,
            new_api_user_id,
        )
        .await
        .map_err(QuotaMutationError::Check)?;
        let decimal = |value: Option<Decimal>| value.map(|value| value.normalize().to_string());
        Ok(NewApiPricingProbeResult {
            source: "new_api_pricing".to_string(),
            source_endpoint: snapshot.source_endpoint,
            fetched_at: snapshot.fetched_at.to_rfc3339(),
            pricing_version: snapshot.pricing_version,
            account_group: snapshot.account_group,
            effective_groups: snapshot.effective_groups,
            key_count: i32::try_from(snapshot.key_count).unwrap_or(i32::MAX),
            matched_key_count: i32::try_from(snapshot.matched_key_count).unwrap_or(i32::MAX),
            warnings: snapshot.warnings,
            models: snapshot
                .models
                .into_iter()
                .map(|model| NewApiModelPricingProbe {
                    model_id: model.model_id,
                    billing_kind: model.billing_kind,
                    quality: model.quality,
                    group_ratio: decimal(model.group_ratio),
                    input_per_million: decimal(model.input_per_million),
                    output_per_million: decimal(model.output_per_million),
                    cache_read_per_million: decimal(model.cache_read_per_million),
                    cache_write_per_million: decimal(model.cache_write_per_million),
                    flat_per_request: decimal(model.flat_per_request),
                    reason: model.reason,
                })
                .collect(),
        })
    }

    async fn reset_channel_quota_now(&self, channel_id: &str) -> Result<(), QuotaMutationError> {
        let channel_id = numeric_id_from_gql(channel_id).map_err(QuotaMutationError::Reset)?;
        sqlx::query("DELETE FROM provider_quota_status WHERE channel_id=$1")
            .bind(channel_id)
            .execute(&self.pool)
            .await
            .map_err(|error| QuotaMutationError::Reset(error.to_string()))?;
        Ok(())
    }

    async fn quota_enforcement_settings(
        &self,
    ) -> Result<QuotaEnforcementSettings, QuotaMutationError> {
        self.read_enforcement_settings()
            .await
            .map_err(QuotaMutationError::ReadCurrent)
    }

    async fn set_quota_enforcement_settings(
        &self,
        settings: QuotaEnforcementSettings,
    ) -> Result<(), QuotaMutationError> {
        let stored = StoredQuotaEnforcementSettings {
            enabled: settings.enabled,
            mode: mode_to_wire(settings.mode).to_string(),
        };
        self.system
            .set_json(
                &system_context(),
                system_key::QUOTA_ENFORCEMENT_SETTINGS,
                &stored,
            )
            .await
            .map(|_| ())
            .map_err(|error| QuotaMutationError::Update(error.to_string()))
    }
}

#[async_trait]
impl ProviderQuotaStatusServices for PgProviderQuotaAdapter {
    async fn provider_quota_status_for_channel(
        &self,
        channel_id: &str,
    ) -> Result<Option<GqlProviderQuotaStatus>, String> {
        let channel_id = numeric_id_from_gql(channel_id)?;
        let row = sqlx::query_as::<
            _,
            (
                i64,
                i64,
                String,
                String,
                Value,
                Option<DateTime<Utc>>,
                bool,
                DateTime<Utc>,
                DateTime<Utc>,
                DateTime<Utc>,
                Option<String>,
                Option<DateTime<Utc>>,
            ),
        >(
            "SELECT id,channel_id,provider_type,status,COALESCE(quota_data,'{}'::jsonb), \
                    next_reset_at,ready,next_check_at,created_at,updated_at, \
                    probe_adapter,probe_verified_at \
             FROM provider_quota_status WHERE channel_id=$1 AND deleted_at=0 LIMIT 1",
        )
        .bind(channel_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| format!("provider quota status query failed: {error}"))?;
        let Some((
            id,
            channel_id,
            provider_type,
            status,
            quota_data,
            next_reset_at,
            ready,
            next_check_at,
            created_at,
            updated_at,
            probe_adapter,
            probe_verified_at,
        )) = row
        else {
            return Ok(None);
        };
        let Some(provider_type) = provider_type_from_wire(&provider_type) else {
            return Ok(None);
        };
        Ok(Some(GqlProviderQuotaStatus {
            id: async_graphql::ID::from(format!("gid://conduit/ProviderQuotaStatus/{id}")),
            created_at: TimeScalar(created_at),
            updated_at: TimeScalar(updated_at),
            channel_id: async_graphql::ID::from(format!("gid://conduit/Channel/{channel_id}")),
            provider_type,
            status: status_from_wire(&status),
            quota_data: MapScalar(quota_data),
            next_reset_at: next_reset_at.map(TimeScalar),
            ready,
            next_check_at: TimeScalar(next_check_at),
            probe_adapter,
            probe_verified_at: probe_verified_at.map(TimeScalar),
        }))
    }
}

fn system_context() -> RequestContext {
    RequestContext::new(PolicyContext::new(Principal::system()))
}

fn mode_to_wire(mode: QuotaEnforcementMode) -> &'static str {
    match mode {
        QuotaEnforcementMode::ExhaustedOnly => "exhausted_only",
        QuotaEnforcementMode::DePrioritize => "de_prioritize",
    }
}

fn mode_from_wire(raw: &str) -> QuotaEnforcementMode {
    match raw {
        "de_prioritize" | "DE_PRIORITIZE" => QuotaEnforcementMode::DePrioritize,
        _ => QuotaEnforcementMode::ExhaustedOnly,
    }
}

fn provider_type_from_wire(raw: &str) -> Option<ProviderQuotaStatusProviderType> {
    Some(match raw {
        "claudecode" => ProviderQuotaStatusProviderType::Claudecode,
        "codex" => ProviderQuotaStatusProviderType::Codex,
        "github_copilot" => ProviderQuotaStatusProviderType::GithubCopilot,
        "nanogpt" => ProviderQuotaStatusProviderType::Nanogpt,
        "wafer" => ProviderQuotaStatusProviderType::Wafer,
        "synthetic" => ProviderQuotaStatusProviderType::Synthetic,
        "neuralwatt" => ProviderQuotaStatusProviderType::Neuralwatt,
        "apertis" => ProviderQuotaStatusProviderType::Apertis,
        "new_api" => ProviderQuotaStatusProviderType::NewApi,
        _ => return None,
    })
}

fn status_from_wire(raw: &str) -> ProviderQuotaStatusStatus {
    match raw {
        "available" => ProviderQuotaStatusStatus::Available,
        "warning" => ProviderQuotaStatusStatus::Warning,
        "exhausted" => ProviderQuotaStatusStatus::Exhausted,
        _ => ProviderQuotaStatusStatus::Unknown,
    }
}

fn provider_type(channel_type: &str, base_url: Option<&str>) -> Option<String> {
    checker_for(channel_type)
        .map(|kind| kind.as_provider_type().to_string())
        .or_else(|| detect_provider_from_url(base_url.unwrap_or_default()))
}

fn access_token(credentials: &ChannelCredentials) -> Option<String> {
    credentials
        .oauth
        .as_ref()
        .and_then(|value| value.get("access_token"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            serde_json::from_str::<Value>(&credentials.api_key)
                .ok()
                .and_then(|value| value.get("access_token")?.as_str().map(ToOwned::to_owned))
        })
        .or_else(|| credentials.get_all_api_keys()?.into_iter().next())
}

async fn response_error(response: reqwest::Response) -> String {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    let detail = serde_json::from_str::<Value>(&body).ok().and_then(|json| {
        let code = json["code"].as_str().unwrap_or_default().trim();
        let message = json["message"].as_str().unwrap_or_default().trim();
        match (code.is_empty(), message.is_empty()) {
            (false, false) => Some(format!("{code}: {message}")),
            (false, true) => Some(code.to_string()),
            (true, false) => Some(message.to_string()),
            (true, true) => None,
        }
    });
    match detail {
        Some(detail) => format!("quota endpoint returned HTTP {status} ({detail})"),
        None => format!("quota endpoint returned HTTP {status}"),
    }
}

fn new_api_root(base_url: &str) -> Result<String, String> {
    let mut url = reqwest::Url::parse(base_url.trim())
        .map_err(|error| format!("invalid channel base URL: {error}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("channel base URL must use http or https".to_string());
    }
    url.set_query(None);
    url.set_fragment(None);
    let mut path = url.path().trim_end_matches('/').to_string();
    if path.ends_with("/v1") {
        path.truncate(path.len() - 3);
    }
    url.set_path(path.trim_end_matches('/'));
    Ok(url.as_str().trim_end_matches('/').to_string())
}

fn json_decimal(value: &Value) -> Option<Decimal> {
    match value {
        Value::Number(number) => Decimal::from_str(&number.to_string()).ok(),
        Value::String(text) => Decimal::from_str(text.trim()).ok(),
        _ => None,
    }
}

fn mask_new_api_token(key: &str) -> String {
    let key = key.trim().strip_prefix("sk-").unwrap_or(key.trim());
    let chars = key.chars().collect::<Vec<_>>();
    match chars.len() {
        0 => String::new(),
        1..=4 => "*".repeat(chars.len()),
        5..=8 => format!(
            "{}****{}",
            chars[..2].iter().collect::<String>(),
            chars[chars.len() - 2..].iter().collect::<String>()
        ),
        _ => format!(
            "{}**********{}",
            chars[..4].iter().collect::<String>(),
            chars[chars.len() - 4..].iter().collect::<String>()
        ),
    }
}

fn normalize_new_api_pricing_item(
    item: &Value,
    effective_groups: &BTreeSet<String>,
    group_ratios: &BTreeMap<String, Decimal>,
    group_resolution_estimated: bool,
) -> Option<NewApiModelPricingSnapshot> {
    let model_id = item["model_name"].as_str()?.trim().to_string();
    if model_id.is_empty() {
        return None;
    }
    let enabled_groups = item["enable_groups"]
        .as_array()
        .map(|groups| {
            groups
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    let group_applies = |group: &&String| {
        enabled_groups.is_empty()
            || enabled_groups.contains("all")
            || enabled_groups.contains(group.as_str())
    };
    let ratios = effective_groups
        .iter()
        .filter(group_applies)
        .filter_map(|group| group_ratios.get(group).copied())
        .collect::<BTreeSet<_>>();
    let Some(group_ratio) = ratios.iter().next_back().copied() else {
        return Some(NewApiModelPricingSnapshot {
            model_id,
            billing_kind: "unsupported".into(),
            quality: "unsupported".into(),
            group_ratio: None,
            input_per_million: None,
            output_per_million: None,
            cache_read_per_million: None,
            cache_write_per_million: None,
            flat_per_request: None,
            reason: Some("该模型不对渠道 KEY 的有效分组开放，或上游未返回分组倍率".into()),
        });
    };
    if item["billing_mode"].as_str() == Some("tiered_expr") {
        return Some(NewApiModelPricingSnapshot {
            model_id,
            billing_kind: "tiered_expr".into(),
            quality: "unsupported".into(),
            group_ratio: Some(group_ratio),
            input_per_million: None,
            output_per_million: None,
            cache_read_per_million: None,
            cache_write_per_million: None,
            flat_per_request: None,
            reason: Some("NEW API tiered_expr 暂不能无损转换为渠道采购价".into()),
        });
    }
    let quality = if group_resolution_estimated || ratios.len() > 1 {
        "estimated"
    } else {
        "exact"
    }
    .to_string();
    let reason = (quality == "estimated")
        .then(|| "多个 KEY/分组价格不一致，采用最高分组倍率以避免低估成本".into());
    if item["quota_type"].as_i64() == Some(1) {
        let flat = json_decimal(&item["model_price"]).map(|value| value * group_ratio);
        return Some(NewApiModelPricingSnapshot {
            model_id,
            billing_kind: "per_request".into(),
            quality,
            group_ratio: Some(group_ratio),
            input_per_million: None,
            output_per_million: None,
            cache_read_per_million: None,
            cache_write_per_million: None,
            flat_per_request: flat.map(|value| value.normalize()),
            reason,
        });
    }
    let Some(model_ratio) = json_decimal(&item["model_ratio"]) else {
        return Some(NewApiModelPricingSnapshot {
            model_id,
            billing_kind: "token".into(),
            quality: "unsupported".into(),
            group_ratio: Some(group_ratio),
            input_per_million: None,
            output_per_million: None,
            cache_read_per_million: None,
            cache_write_per_million: None,
            flat_per_request: None,
            reason: Some("NEW API 价格数据缺少 model_ratio".into()),
        });
    };
    let input = model_ratio * Decimal::from(2u32) * group_ratio;
    let output = json_decimal(&item["completion_ratio"]).map(|ratio| input * ratio);
    let cache_read = json_decimal(&item["cache_ratio"]).map(|ratio| input * ratio);
    let cache_write = json_decimal(&item["create_cache_ratio"]).map(|ratio| input * ratio);
    Some(NewApiModelPricingSnapshot {
        model_id,
        billing_kind: "token".into(),
        quality,
        group_ratio: Some(group_ratio.normalize()),
        input_per_million: Some(input.normalize()),
        output_per_million: output.map(|value| value.normalize()),
        cache_read_per_million: cache_read.map(|value| value.normalize()),
        cache_write_per_million: cache_write.map(|value| value.normalize()),
        flat_per_request: None,
        reason,
    })
}

fn new_api_pricing_shape(payload: &Value) -> bool {
    payload["success"].as_bool() == Some(true)
        && payload["data"].as_array().is_some_and(|rows| {
            rows.iter().any(|row| {
                row["model_name"].as_str().is_some()
                    && (row.get("model_ratio").is_some() || row.get("model_price").is_some())
            })
        })
}

fn model_ids_from_catalog(payload: &Value) -> Vec<String> {
    let data = payload.get("data").unwrap_or(payload);
    let mut models = BTreeSet::new();
    match data {
        Value::Array(rows) => {
            for row in rows {
                let id = row
                    .get("id")
                    .or_else(|| row.get("model"))
                    .or_else(|| row.get("model_name"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty());
                if let Some(id) = id {
                    models.insert(id.to_string());
                }
            }
        }
        Value::Object(rows) => {
            for (key, row) in rows {
                let id = row
                    .get("id")
                    .or_else(|| row.get("model"))
                    .or_else(|| row.get("model_name"))
                    .and_then(Value::as_str)
                    .unwrap_or(key)
                    .trim();
                if !id.is_empty() {
                    models.insert(id.to_string());
                }
            }
        }
        _ => {}
    }
    models.into_iter().collect()
}

fn stored_new_api_pat(credentials: &Value) -> Option<String> {
    credentials["newApiPat"]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn stored_new_api_user_id(credentials: &Value) -> Option<i64> {
    credentials["newApiUserID"]
        .as_str()
        .and_then(|value| value.trim().parse::<i64>().ok())
        .or_else(|| credentials["newApiUserID"].as_i64())
        .filter(|value| *value > 0)
}

fn normalize_new_api_pat(value: &str) -> Result<String, String> {
    let trimmed = value.trim();
    let pat = trimmed
        .strip_prefix("Bearer ")
        .or_else(|| trimmed.strip_prefix("bearer "))
        .unwrap_or(trimmed)
        .trim();
    if pat.is_empty() {
        return Err("NEW API 用户 PAT 不能为空".to_string());
    }
    if pat.len() > 4096 || pat.chars().any(char::is_whitespace) {
        return Err("NEW API 用户 PAT 格式无效".to_string());
    }
    Ok(pat.to_string())
}

fn normalize_new_api_user_id(value: &str) -> Result<i64, String> {
    value
        .trim()
        .parse::<i64>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| "NEW API 用户 ID 必须是正整数".to_string())
}

fn normalize_new_api_snapshot(snapshot: &NewApiQuotaSnapshot) -> NormalizedQuota {
    let ratio = if snapshot.requires_pat {
        0.0
    } else if snapshot.total.unwrap_or_default() > Decimal::ZERO {
        (snapshot.used / snapshot.total.unwrap_or_default())
            .to_string()
            .parse::<f64>()
            .unwrap_or(0.0)
    } else if snapshot.remaining.unwrap_or_default() <= Decimal::ZERO {
        1.0
    } else {
        0.0
    };
    let status = if snapshot.requires_pat {
        "available"
    } else if snapshot.remaining.unwrap_or_default() <= Decimal::ZERO {
        "exhausted"
    } else if ratio >= 0.8 {
        "warning"
    } else {
        "available"
    };
    NormalizedQuota {
        status: status.to_string(),
        quota_data: json!({
            "adapter": NEW_API_PROBE_ADAPTER,
            "currency": snapshot.currency,
            "total": snapshot.total.map(|value| value.normalize().to_string()),
            "used": snapshot.used.normalize().to_string(),
            "remaining": snapshot.remaining.map(|value| value.normalize().to_string()),
            "balance_source": snapshot.balance_source,
            "requires_pat": snapshot.requires_pat,
            "unlimited": snapshot.unlimited,
            "unlimited_key_count": snapshot.unlimited_key_count,
            "key_count": snapshot.key_count,
            "limits": [limit("balance", status, ratio, None)],
        }),
        next_reset_at: None,
        ready: status != "exhausted",
    }
}

fn normalize_claude(headers: &reqwest::header::HeaderMap) -> Result<NormalizedQuota, String> {
    let get = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
    };
    let unified = get("anthropic-ratelimit-unified-status");
    if unified.is_empty() {
        return Err("missing quota headers".to_string());
    }
    let five = get("anthropic-ratelimit-unified-5h-utilization")
        .parse::<f64>()
        .unwrap_or(0.0);
    let seven = get("anthropic-ratelimit-unified-7d-utilization")
        .parse::<f64>()
        .unwrap_or(0.0);
    let status = match unified {
        "throttled" | "rejected" => "exhausted",
        "allowed" if five >= 0.8 || seven >= 0.8 => "warning",
        "allowed" => "available",
        _ => "unknown",
    };
    let reset = [
        "anthropic-ratelimit-unified-5h-reset",
        "anthropic-ratelimit-unified-7d-reset",
    ]
    .iter()
    .filter_map(|name| get(name).parse::<i64>().ok())
    .filter_map(|seconds| DateTime::from_timestamp(seconds, 0))
    .min();
    Ok(NormalizedQuota {
        status: status.to_string(),
        quota_data: json!({
            "unified_status": unified,
            "limits": [
                limit("token", status_for_ratio(five), five, reset),
                limit("token", status_for_ratio(seven), seven, reset),
            ],
        }),
        next_reset_at: reset,
        ready: matches!(status, "available" | "warning"),
    })
}

fn normalize_json(provider: &str, body: Value) -> Result<NormalizedQuota, String> {
    let (status, ratio, reset) = match provider {
        "codex" => normalize_codex(&body),
        "github_copilot" => normalize_copilot(&body),
        "nanogpt" => normalize_nanogpt(&body),
        "wafer" => normalize_wafer(&body),
        "synthetic" => normalize_synthetic(&body),
        "neuralwatt" => normalize_neuralwatt(&body),
        "apertis" => normalize_apertis(&body),
        _ => return Err(format!("unsupported quota provider {provider}")),
    };
    let mut quota_data = body;
    if !quota_data.is_object() {
        quota_data = json!({"raw_data": quota_data});
    }
    if let Some(object) = quota_data.as_object_mut() {
        object.insert(
            "limits".to_string(),
            json!([limit("token", &status, ratio, reset)]),
        );
    }
    Ok(NormalizedQuota {
        ready: matches!(status.as_str(), "available" | "warning"),
        status,
        quota_data,
        next_reset_at: reset,
    })
}

fn normalize_codex(value: &Value) -> (String, f64, Option<DateTime<Utc>>) {
    let rate = &value["rate_limit"];
    let used = rate["primary_window"]["used_percent"]
        .as_f64()
        .unwrap_or(0.0)
        / 100.0;
    let exhausted =
        rate["limit_reached"].as_bool() == Some(true) || rate["allowed"].as_bool() == Some(false);
    let status = if exhausted {
        "exhausted"
    } else {
        status_for_ratio(used)
    };
    let reset = rate["primary_window"]["reset_at"]
        .as_i64()
        .and_then(|seconds| DateTime::from_timestamp(seconds, 0));
    (
        status.to_string(),
        if exhausted { used.max(1.0) } else { used },
        reset,
    )
}

fn normalize_copilot(value: &Value) -> (String, f64, Option<DateTime<Utc>>) {
    let mut lowest = 100.0_f64;
    if let Some(quotas) = value["limited_user_quotas"].as_object() {
        for (key, remaining) in quotas {
            let remaining = remaining.as_f64().unwrap_or(0.0);
            let total = value["monthly_quotas"][key].as_f64().unwrap_or(remaining);
            if total > 0.0 {
                lowest = lowest.min(remaining / total * 100.0);
            }
        }
    }
    if let Some(snapshots) = value["quota_snapshots"].as_object() {
        for snapshot in snapshots.values() {
            if snapshot["unlimited"].as_bool() != Some(true)
                && let Some(percent) = snapshot["percent_remaining"].as_f64()
            {
                lowest = lowest.min(percent);
            }
        }
    }
    let status = if lowest <= 0.0 {
        "exhausted"
    } else if lowest < 20.0 {
        "warning"
    } else {
        "available"
    };
    let reset = parse_time_fields(
        value,
        &[
            "quota_reset_date_utc",
            "quota_reset_date",
            "limited_user_reset_date",
        ],
    );
    (status.to_string(), 1.0 - lowest / 100.0, reset)
}

fn normalize_nanogpt(value: &Value) -> (String, f64, Option<DateTime<Utc>>) {
    let mut ratio = 0.0_f64;
    let mut reset = None;
    for key in ["dailyImages", "dailyInputTokens", "weeklyInputTokens"] {
        ratio = ratio.max(value[key]["percentUsed"].as_f64().unwrap_or(0.0));
        if let Some(candidate) = value[key]["resetAt"]
            .as_i64()
            .and_then(|seconds| DateTime::from_timestamp(seconds, 0))
        {
            reset = Some(reset.map_or(candidate, |old: DateTime<Utc>| old.min(candidate)));
        }
    }
    let status = match value["state"].as_str() {
        Some("inactive") => "exhausted",
        Some("grace") => "warning",
        Some("active") => status_for_ratio(ratio),
        _ => "unknown",
    };
    (
        status.to_string(),
        ratio,
        reset.or_else(|| parse_time_fields(value, &["graceUntil"])),
    )
}

fn normalize_wafer(value: &Value) -> (String, f64, Option<DateTime<Utc>>) {
    let ratio = value["current_period_used_percent"].as_f64().unwrap_or(0.0) / 100.0;
    let status = if value["remaining_included_requests"]
        .as_i64()
        .is_some_and(|remaining| remaining <= 0)
    {
        "exhausted"
    } else {
        status_for_ratio(ratio)
    };
    (
        status.to_string(),
        ratio,
        parse_time_fields(value, &["window_end"]),
    )
}

fn normalize_synthetic(value: &Value) -> (String, f64, Option<DateTime<Utc>>) {
    let five = &value["rollingFiveHourLimit"];
    let five_ratio = five["tickPercent"].as_f64().unwrap_or(0.0);
    let weekly_ratio = 1.0
        - value["weeklyTokenLimit"]["percentRemaining"]
            .as_f64()
            .unwrap_or(100.0)
            / 100.0;
    let ratio = five_ratio.max(weekly_ratio);
    let status = if five["limited"].as_bool() == Some(true) {
        "exhausted"
    } else {
        status_for_ratio(ratio)
    };
    (
        status.to_string(),
        ratio,
        parse_time_fields_recursive(value, &["renewsAt", "nextRegenAt", "nextTickAt"]),
    )
}

fn normalize_neuralwatt(value: &Value) -> (String, f64, Option<DateTime<Utc>>) {
    let subscription = &value["subscription"];
    let included = subscription["kwh_included"].as_f64().unwrap_or(0.0);
    let remaining = subscription["kwh_remaining"].as_f64();
    let used = subscription["kwh_used"].as_f64();
    let ratio = if included > 0.0 {
        used.map(|amount| amount / included)
            .or_else(|| remaining.map(|amount| 1.0 - amount / included))
            .unwrap_or(0.0)
    } else {
        0.0
    };
    let status = if subscription["in_overage"].as_bool() == Some(true)
        || remaining.is_some_and(|amount| amount <= 0.0)
    {
        "exhausted"
    } else {
        status_for_ratio(ratio)
    };
    (
        status.to_string(),
        ratio,
        parse_time_fields(subscription, &["kwh_reset_date"]),
    )
}

fn normalize_apertis(value: &Value) -> (String, f64, Option<DateTime<Utc>>) {
    let subscription = &value["subscription"];
    let limit = subscription["cycle_quota_limit"].as_f64().unwrap_or(0.0);
    let used = subscription["cycle_quota_used"].as_f64().unwrap_or(0.0);
    let remaining = subscription["cycle_quota_remaining"]
        .as_f64()
        .unwrap_or(0.0);
    let ratio = if limit > 0.0 { used / limit } else { 0.0 };
    let subscription_available =
        subscription["status"].as_str() == Some("active") && remaining > 0.0;
    let credits = value["payg"]["account_credits"].as_f64().unwrap_or(0.0);
    let unlimited = value["payg"]["token_is_unlimited"].as_bool() == Some(true);
    let status = if subscription_available || unlimited || credits > 0.0 {
        status_for_ratio(ratio)
    } else {
        "exhausted"
    };
    (
        status.to_string(),
        ratio,
        parse_time_fields(subscription, &["cycle_end"]),
    )
}

fn status_for_ratio(ratio: f64) -> &'static str {
    if ratio >= 1.0 {
        "exhausted"
    } else if ratio >= 0.8 {
        "warning"
    } else {
        "available"
    }
}

fn limit(kind: &str, status: &str, ratio: f64, reset: Option<DateTime<Utc>>) -> Value {
    json!({
        "type": kind,
        "status": status,
        "usage_ratio": ratio,
        "ready": matches!(status, "available" | "warning"),
        "next_reset_at": reset,
    })
}

fn parse_time_fields(value: &Value, fields: &[&str]) -> Option<DateTime<Utc>> {
    fields
        .iter()
        .find_map(|field| parse_time(value[*field].as_str()?))
}

fn parse_time_fields_recursive(value: &Value, fields: &[&str]) -> Option<DateTime<Utc>> {
    if let Some(time) = parse_time_fields(value, fields) {
        return Some(time);
    }
    value
        .as_object()?
        .values()
        .filter_map(|child| parse_time_fields_recursive(child, fields))
        .min()
}

fn parse_time(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|time| time.with_timezone(&Utc))
        .or_else(|| {
            chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d")
                .ok()?
                .and_hms_opt(0, 0, 0)
                .map(|time| time.and_utc())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    use conduit_cache::{Cache, NoopCache};
    use sqlx::types::Json;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_system(pool: &PgPool) -> Arc<DomainSystemService> {
        let cache: Arc<dyn Cache> = Arc::new(NoopCache::new());
        Arc::new(DomainSystemService::from_system_repo(
            Arc::new(conduit_db::PgSystemRepo::new(pool.clone())),
            cache,
        ))
    }

    async fn insert_channel(
        pool: &PgPool,
        suffix: &str,
        label: &str,
        base_url: &str,
        key: &str,
    ) -> Result<i64, sqlx::Error> {
        sqlx::query_scalar::<_, i64>(
            "INSERT INTO channels \
             (\"type\",base_url,name,status,credentials,supported_models,default_test_model,settings) \
             VALUES('openai',$1,$2,'enabled',$3,$4,'same-model','{}'::jsonb) RETURNING id",
        )
        .bind(format!("{base_url}/v1"))
        .bind(format!("PG provider quota {label} {suffix}"))
        .bind(Json(json!({"apiKey": key})))
        .bind(Json(json!(["same-model"])))
        .fetch_one(pool)
        .await
    }

    async fn mount_status(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path("/api/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "success": true,
                "data": {"quota_per_unit": 20, "quota_display_type": "USD"}
            })))
            .mount(server)
            .await;
    }

    async fn mount_key_quota(
        server: &MockServer,
        key: &str,
        unlimited: bool,
        granted: i64,
        used: i64,
        available: i64,
    ) {
        Mock::given(method("GET"))
            .and(path("/api/usage/token/"))
            .and(header("authorization", format!("Bearer {key}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": true,
                "data": {
                    "total_granted": granted,
                    "total_used": used,
                    "total_available": available,
                    "unlimited_quota": unlimited
                }
            })))
            .mount(server)
            .await;
    }

    async fn mount_account_and_pricing(server: &MockServer, model_ratio: i64) {
        Mock::given(method("GET"))
            .and(path("/api/user/self"))
            .and(header("authorization", "Bearer user-pat"))
            .and(header("new-api-user", "19301"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "success": true,
                "data": {
                    "group": "default",
                    "quota": 750,
                    "used_quota": 250
                }
            })))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/user/self"))
            .and(header("authorization", "Bearer invalid-pat"))
            .and(header("new-api-user", "19301"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "success": false,
                "code": "AUTH_UNAUTHORIZED",
                "message": "Access token is invalid"
            })))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/token/"))
            .and(header("authorization", "Bearer user-pat"))
            .and(header("new-api-user", "19301"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "success": true,
                "data": {
                    "total": 1,
                    "items": [{
                        "key": "abcd**********5678",
                        "group": "default"
                    }]
                }
            })))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/pricing"))
            .and(header("authorization", "Bearer user-pat"))
            .and(header("new-api-user", "19301"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "success": true,
                "pricing_version": format!("v{model_ratio}"),
                "group_ratio": {"default": 1},
                "data": [{
                    "model_name": "same-model",
                    "quota_type": 0,
                    "model_ratio": model_ratio,
                    "completion_ratio": 2,
                    "cache_ratio": 0.25,
                    "enable_groups": ["all"]
                }]
            })))
            .mount(server)
            .await;
    }

    #[test]
    fn unsupported_catalog_never_becomes_zero_price() {
        let ids = model_ids_from_catalog(&json!({
            "object": "list",
            "data": [{"id": "same-model"}, {"id": "same-model"}]
        }));
        assert_eq!(ids, vec!["same-model"]);
        assert!(!new_api_pricing_shape(&json!({
            "success": true,
            "data": [{"id": "same-model"}]
        })));
    }

    #[test]
    fn token_mask_and_price_group_match_new_api_contract() {
        assert_eq!(
            mask_new_api_token("sk-abcdefgh12345678"),
            "abcd**********5678"
        );
        let groups = BTreeSet::from(["default".to_string()]);
        let ratios = BTreeMap::from([("default".to_string(), Decimal::ONE)]);
        let price = normalize_new_api_pricing_item(
            &json!({
                "model_name": "same-model",
                "quota_type": 0,
                "model_ratio": 2,
                "completion_ratio": 3,
                "enable_groups": ["all"]
            }),
            &groups,
            &ratios,
            false,
        )
        .expect("normalized price");
        assert_eq!(price.quality, "exact");
        assert_eq!(price.input_per_million, Some(Decimal::from(4)));
        assert_eq!(price.output_per_million, Some(Decimal::from(12)));
    }

    #[tokio::test]
    async fn postgres_provider_observations_cover_success_pat_401_unsupported_and_price_changes_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let pool = PgPool::connect(&dsn).await?;
        conduit_db::connection::migrate_postgres_with_flag(&pool, false).await?;

        let first = MockServer::start().await;
        mount_status(&first).await;
        mount_key_quota(&first, "sk-abcdefgh12345678", false, 1000, 250, 750).await;
        mount_key_quota(&first, "unlimited-key", true, 0, 40, 0).await;
        Mock::given(method("GET"))
            .and(path("/api/usage/token/"))
            .and(header("authorization", "Bearer rejected-key"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "code": "AUTH_UNAUTHORIZED",
                "message": "API key is invalid"
            })))
            .mount(&first)
            .await;
        mount_account_and_pricing(&first, 1).await;

        let second = MockServer::start().await;
        mount_status(&second).await;
        mount_key_quota(&second, "sk-abcdefgh12345678", false, 1000, 300, 700).await;
        mount_account_and_pricing(&second, 2).await;

        let unsupported = MockServer::start().await;
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let priced_channel = insert_channel(
            &pool,
            &suffix,
            "priced",
            &first.uri(),
            "sk-abcdefgh12345678",
        )
        .await?;
        sqlx::query("UPDATE channels SET settings=$1,updated_at=now() WHERE id=$2")
            .bind(Json(json!({
                "billingCurrency": "CNY",
                "rechargeMultiplier": "1"
            })))
            .bind(priced_channel)
            .execute(&pool)
            .await?;
        let unlimited_channel =
            insert_channel(&pool, &suffix, "unlimited", &first.uri(), "unlimited-key").await?;
        let rejected_channel =
            insert_channel(&pool, &suffix, "rejected", &first.uri(), "rejected-key").await?;
        let unsupported_channel = insert_channel(
            &pool,
            &suffix,
            "unsupported",
            &unsupported.uri(),
            "plain-openai-key",
        )
        .await?;
        let adapter = PgProviderQuotaAdapter::new(pool.clone(), test_system(&pool));

        let preview = QuotaMutationServices::probe_channel_quota(
            &adapter,
            &priced_channel.to_string(),
            None,
            None,
        )
        .await?;
        assert!(preview.success);
        assert_eq!(preview.total.as_deref(), Some("50"));
        assert_eq!(preview.used.as_deref(), Some("12.5"));
        assert_eq!(preview.remaining.as_deref(), Some("37.5"));
        assert!(!preview.verified);
        let confirmed = QuotaMutationServices::confirm_channel_quota_probe(
            &adapter,
            &priced_channel.to_string(),
        )
        .await?;
        assert!(confirmed.success);
        assert!(confirmed.verified);

        let unlimited_preview = QuotaMutationServices::probe_channel_quota(
            &adapter,
            &unlimited_channel.to_string(),
            None,
            None,
        )
        .await?;
        assert!(unlimited_preview.success);
        assert!(unlimited_preview.unlimited);
        assert!(unlimited_preview.requires_pat);
        assert!(unlimited_preview.total.is_none());
        let refused_confirmation = QuotaMutationServices::confirm_channel_quota_probe(
            &adapter,
            &unlimited_channel.to_string(),
        )
        .await?;
        assert!(!refused_confirmation.success);
        assert!(refused_confirmation.message.contains("PAT"));
        let account_preview = QuotaMutationServices::probe_channel_quota(
            &adapter,
            &unlimited_channel.to_string(),
            Some("user-pat"),
            Some("19301"),
        )
        .await?;
        assert!(account_preview.success);
        assert_eq!(account_preview.balance_source.as_deref(), Some("account"));
        assert_eq!(account_preview.total.as_deref(), Some("50"));
        let account_confirmed = QuotaMutationServices::confirm_channel_quota_probe(
            &adapter,
            &unlimited_channel.to_string(),
        )
        .await?;
        assert!(account_confirmed.verified);

        let rejected = QuotaMutationServices::probe_channel_quota(
            &adapter,
            &rejected_channel.to_string(),
            None,
            None,
        )
        .await?;
        assert!(!rejected.success);
        assert!(rejected.message.contains("401 Unauthorized"));
        assert!(rejected.message.contains("AUTH_UNAUTHORIZED"));
        assert!(rejected.total.is_none());

        let unsupported_result = QuotaMutationServices::probe_channel_quota(
            &adapter,
            &unsupported_channel.to_string(),
            None,
            None,
        )
        .await?;
        assert!(!unsupported_result.success);
        assert!(
            unsupported_result
                .message
                .contains("暂时只支持查询 NEW API")
        );
        assert!(unsupported_result.total.is_none());

        let first_price = QuotaMutationServices::probe_new_api_pricing(
            &adapter,
            &priced_channel.to_string(),
            Some("user-pat"),
            Some("19301"),
        )
        .await?;
        assert_eq!(first_price.models.len(), 1);
        assert_eq!(first_price.models[0].quality, "exact");
        assert_eq!(
            first_price.models[0].input_per_million.as_deref(),
            Some("2")
        );

        sqlx::query("UPDATE channels SET base_url=$1,updated_at=now() WHERE id=$2")
            .bind(format!("{}/v1", second.uri()))
            .bind(priced_channel)
            .execute(&pool)
            .await?;
        let second_price = QuotaMutationServices::probe_new_api_pricing(
            &adapter,
            &priced_channel.to_string(),
            None,
            None,
        )
        .await?;
        assert_eq!(
            second_price.models[0].input_per_million.as_deref(),
            Some("4")
        );

        let pat_error = QuotaMutationServices::probe_new_api_pricing(
            &adapter,
            &priced_channel.to_string(),
            Some("invalid-pat"),
            None,
        )
        .await
        .expect_err("invalid PAT must fail");
        let pat_error = pat_error.to_string();
        assert!(pat_error.contains("401 Unauthorized"));
        assert!(pat_error.contains("AUTH_UNAUTHORIZED"));

        let status = ProviderQuotaStatusServices::provider_quota_status_for_channel(
            &adapter,
            &priced_channel.to_string(),
        )
        .await?
        .expect("stored provider status");
        assert_eq!(
            status.provider_type,
            ProviderQuotaStatusProviderType::NewApi
        );
        assert!(status.probe_verified_at.is_some());

        let increased = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM provider_price_change_events \
             WHERE channel_id=$1 AND event_type='increased'",
        )
        .bind(priced_channel)
        .fetch_one(&pool)
        .await?;
        assert_eq!(increased, 1);
        let price_snapshots = sqlx::query_as::<_, (i64, i64)>(
            "SELECT COUNT(*) FILTER (WHERE status='success'), \
                    COUNT(*) FILTER (WHERE status='failed') \
             FROM provider_price_snapshots WHERE channel_id=$1",
        )
        .bind(priced_channel)
        .fetch_one(&pool)
        .await?;
        assert_eq!(price_snapshots, (2, 1));

        let failure_observations = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM provider_quota_observations \
             WHERE channel_id=ANY($1) AND success=FALSE AND error_message IS NOT NULL",
        )
        .bind(vec![rejected_channel, unsupported_channel])
        .fetch_one(&pool)
        .await?;
        assert_eq!(failure_observations, 2);
        let fabricated = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM provider_quota_observations \
             WHERE channel_id=$1 AND (total IS NOT NULL OR remaining IS NOT NULL \
               OR quota_data::text LIKE '%100000000%')",
        )
        .bind(unsupported_channel)
        .fetch_one(&pool)
        .await?;
        assert_eq!(fabricated, 0);
        let leaked_pat = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM provider_quota_observations \
             WHERE channel_id=ANY($1) AND quota_data::text LIKE '%user-pat%'",
        )
        .bind(vec![priced_channel, unlimited_channel])
        .fetch_one(&pool)
        .await?;
        assert_eq!(leaked_pat, 0);

        let previous_enforcement_value = sqlx::query_scalar::<_, String>(
            "SELECT value FROM systems WHERE key=$1 AND deleted_at=0",
        )
        .bind(system_key::QUOTA_ENFORCEMENT_SETTINGS)
        .fetch_optional(&pool)
        .await?;
        QuotaMutationServices::set_quota_enforcement_settings(
            &adapter,
            QuotaEnforcementSettings {
                enabled: true,
                mode: QuotaEnforcementMode::DePrioritize,
            },
        )
        .await?;
        let settings = QuotaMutationServices::quota_enforcement_settings(&adapter).await?;
        assert!(settings.enabled);
        assert_eq!(settings.mode, QuotaEnforcementMode::DePrioritize);
        match previous_enforcement_value {
            Some(value) => {
                sqlx::query("UPDATE systems SET value=$1,updated_at=now() WHERE key=$2")
                    .bind(value)
                    .bind(system_key::QUOTA_ENFORCEMENT_SETTINGS)
                    .execute(&pool)
                    .await?;
            }
            None => {
                sqlx::query("DELETE FROM systems WHERE key=$1")
                    .bind(system_key::QUOTA_ENFORCEMENT_SETTINGS)
                    .execute(&pool)
                    .await?;
            }
        }
        let channel_ids = vec![
            priced_channel,
            unlimited_channel,
            rejected_channel,
            unsupported_channel,
        ];
        for statement in [
            "DELETE FROM provider_price_change_events WHERE channel_id=ANY($1)",
            "DELETE FROM provider_price_rows WHERE channel_id=ANY($1)",
            "DELETE FROM provider_price_snapshots WHERE channel_id=ANY($1)",
            "DELETE FROM provider_quota_observations WHERE channel_id=ANY($1)",
            "DELETE FROM provider_quota_status WHERE channel_id=ANY($1)",
            "DELETE FROM channels WHERE id=ANY($1)",
        ] {
            sqlx::query(statement)
                .bind(&channel_ids)
                .execute(&pool)
                .await?;
        }
        Ok(())
    }

    #[tokio::test]
    async fn postgres_api_key_quota_query_uses_admission_and_usage_ledgers_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let pool = PgPool::connect(&dsn).await?;
        conduit_db::connection::migrate_postgres_with_flag(&pool, false).await?;
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let project_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO projects(name,description) VALUES($1,'quota query test') RETURNING id",
        )
        .bind(format!("PG quota project {suffix}"))
        .fetch_one(&pool)
        .await?;
        let api_key_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO api_keys(project_id,key,name,profiles) \
             VALUES($1,$2,'quota-key',$3) RETURNING id",
        )
        .bind(project_id)
        .bind(format!("conduit-pg-quota-{suffix}"))
        .bind(Json(json!({
            "activeProfile": "paid",
            "profiles": [{
                "name": "paid",
                "quota": {
                    "requests": 10,
                    "totalTokens": 1000,
                    "period": {"type": "all_time"}
                }
            }]
        })))
        .fetch_one(&pool)
        .await?;
        let request_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO requests(api_key_id,project_id,model_id,request_body,status) \
             VALUES($1,$2,'quota-model','{}'::jsonb,'completed') RETURNING id",
        )
        .bind(api_key_id)
        .bind(project_id)
        .fetch_one(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO usage_logs \
             (request_id,api_key_id,project_id,model_id,prompt_tokens,completion_tokens,total_tokens,total_cost) \
             VALUES($1,$2,$3,'quota-model',100,25,125,0.5)",
        )
        .bind(request_id)
        .bind(api_key_id)
        .bind(project_id)
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO api_key_quota_admissions(api_key_id,project_id,profile_name,created_at) \
             VALUES($1,$2,'paid',now()),($1,$2,'paid',now())",
        )
        .bind(api_key_id)
        .bind(project_id)
        .execute(&pool)
        .await?;

        let adapter = PgProviderQuotaAdapter::new(pool.clone(), test_system(&pool));
        let usages = QuotaQueryServices::api_key_quota_usages(
            &adapter,
            &format!("gid://conduit/APIKey/{api_key_id}"),
        )
        .await?;
        assert_eq!(usages.len(), 1);
        assert_eq!(usages[0].profile_name, "paid");
        assert_eq!(usages[0].usage.request_count, 2);
        assert_eq!(usages[0].usage.total_tokens, 125);
        assert_eq!(usages[0].usage.total_cost.0, Decimal::new(500_000, 6));

        sqlx::query("DELETE FROM api_key_quota_admissions WHERE api_key_id=$1")
            .bind(api_key_id)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM usage_logs WHERE request_id=$1")
            .bind(request_id)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM requests WHERE id=$1")
            .bind(request_id)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM api_keys WHERE id=$1")
            .bind(api_key_id)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM projects WHERE id=$1")
            .bind(project_id)
            .execute(&pool)
            .await?;
        Ok(())
    }
}
