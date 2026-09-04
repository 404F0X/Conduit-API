#[cfg(test)]
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use conduit_core::objects::channel_settings::{ChannelCredentials, DisabledAPIKey};
use conduit_db::{PolicyContext, Principal, RequestContext};
use conduit_pipeline::{AttemptObservation, AttemptObservationOutcome, AttemptObserver};
use conduit_services::{
    EVENT_CHANNEL_AUTO_DISABLED, SystemService, WebhookNotifierConfig, WebhookTarget,
    credential_fingerprint,
};
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde::Deserialize;
use serde_json::Value;
use sqlx::PgPool;
use tokio::sync::mpsc;

const DEFAULT_WEBHOOK_TIMEOUT_MS: u64 = 3_000;
const ATTEMPT_EVENT_QUEUE_CAPACITY: usize = 4_096;

pub(crate) fn start_auto_disable_runtime(
    pool: PgPool,
    system: Arc<SystemService>,
) -> Arc<dyn AttemptObserver> {
    let (tx, rx) = mpsc::channel(ATTEMPT_EVENT_QUEUE_CAPACITY);
    let notifier = Arc::new(WebhookNotifierRuntime::new(system.clone()));
    tokio::spawn(run_worker(rx, pool, system, notifier));
    Arc::new(QueuedAttemptObserver { tx })
}

struct QueuedAttemptObserver {
    tx: mpsc::Sender<AttemptObservation>,
}

impl AttemptObserver for QueuedAttemptObserver {
    fn observe(&self, observation: AttemptObservation) {
        if let Err(error) = self.tx.try_send(observation) {
            tracing::warn!(%error, "auto-disable attempt event was not queued");
        }
    }
}

async fn run_worker(
    mut rx: mpsc::Receiver<AttemptObservation>,
    pool: PgPool,
    system: Arc<SystemService>,
    notifier: Arc<WebhookNotifierRuntime>,
) {
    while let Some(observation) = rx.recv().await {
        let Ok(channel_id) = observation.channel_id.parse::<i64>() else {
            tracing::warn!(
                channel_id = %observation.channel_id,
                "cannot auto-disable a channel with a non-numeric id"
            );
            continue;
        };

        match observation.outcome {
            AttemptObservationOutcome::Succeeded => {
                // Successful executions are persisted and naturally terminate
                // the consecutive-error query used by every process.
            }
            AttemptObservationOutcome::Failed {
                provider_status: Some(status),
            } => {
                let config = match load_auto_disable_config(&system).await {
                    Ok(config) => config,
                    Err(error) => {
                        tracing::warn!(%error, "failed to load auto-disable policy");
                        continue;
                    }
                };
                let action = match persistent_disable_action(
                    &pool,
                    channel_id,
                    status,
                    &observation,
                    &config,
                )
                .await
                {
                    Ok(Some(action)) => action,
                    Ok(None) => continue,
                    Err(error) => {
                        tracing::warn!(channel_id, %error, "failed to evaluate persistent auto-disable counter");
                        continue;
                    }
                };
                match apply_disable_action(&pool, action).await {
                    Ok(Some(event)) => {
                        let notifier = notifier.clone();
                        tokio::spawn(async move {
                            notifier.notify_channel_auto_disabled(event).await;
                        });
                    }
                    Ok(None) => {}
                    Err(error) => {
                        tracing::error!(channel_id, %error, "failed to apply auto-disable action");
                    }
                }
            }
            AttemptObservationOutcome::Failed {
                provider_status: None,
            } => {}
        }
    }
}

async fn persistent_disable_action(
    pool: &PgPool,
    channel_id: i64,
    status: u16,
    observation: &AttemptObservation,
    config: &AutoDisableConfig,
) -> Result<Option<DisableAction>, String> {
    if !config.enabled {
        return Ok(None);
    }
    let Some(threshold) = config
        .statuses
        .iter()
        .find(|rule| rule.status == status)
        .map(|rule| rule.times)
    else {
        return Ok(None);
    };
    let credential = observation
        .credential
        .as_deref()
        .filter(|credential| !credential.is_empty());
    let identity = credential.map(|credential| {
        observation
            .credential_identity
            .clone()
            .unwrap_or_else(|| credential_fingerprint(credential))
    });
    let rows: Vec<(String, Option<i64>)> = sqlx::query_as(
        "SELECT status,response_status_code FROM request_executions \
         WHERE channel_id=$1 AND ($2::text IS NULL OR credential_identity=$2) \
         ORDER BY created_at DESC,id DESC LIMIT $3",
    )
    .bind(channel_id)
    .bind(identity.as_deref())
    .bind(threshold)
    .fetch_all(pool)
    .await
    .map_err(|error| error.to_string())?;
    let actual_count = rows
        .iter()
        .take_while(|(execution_status, response_status)| {
            execution_status == "failed" && *response_status == Some(i64::from(status))
        })
        .count() as i64;
    if actual_count < threshold {
        return Ok(None);
    }
    Ok(Some(match credential {
        Some(credential) => DisableAction::Credential {
            channel_id,
            credential: credential.to_string(),
            status,
            threshold,
            actual_count,
        },
        None => DisableAction::Channel {
            channel_id,
            status,
            threshold,
            actual_count,
        },
    }))
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct AutoDisableConfig {
    enabled: bool,
    statuses: Vec<AutoDisableStatus>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(default)]
struct AutoDisableStatus {
    status: u16,
    times: i64,
}

async fn load_auto_disable_config(system: &SystemService) -> Result<AutoDisableConfig, String> {
    let ctx = RequestContext::new(PolicyContext::new(Principal::system()));
    let Some(policy) = system
        .retry_policy(&ctx)
        .await
        .map_err(|error| error.to_string())?
    else {
        return Ok(AutoDisableConfig::default());
    };
    let config = policy
        .extra
        .get("auto_disable_channel")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map(|value| value.unwrap_or_default())
        .map_err(|error| format!("invalid auto-disable policy: {error}"))?;
    validate_auto_disable_config(&config)?;
    Ok(config)
}

fn validate_auto_disable_config(config: &AutoDisableConfig) -> Result<(), String> {
    let mut seen = std::collections::HashSet::with_capacity(config.statuses.len());
    for (index, rule) in config.statuses.iter().enumerate() {
        if !(400..=599).contains(&rule.status) {
            return Err(format!(
                "invalid auto-disable policy: statuses[{index}].status must be between 400 and 599"
            ));
        }
        if !(1..=100).contains(&rule.times) {
            return Err(format!(
                "invalid auto-disable policy: statuses[{index}].times must be between 1 and 100"
            ));
        }
        if !seen.insert(rule.status) {
            return Err(format!(
                "invalid auto-disable policy: duplicate status {}",
                rule.status
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ErrorCounterKey {
    Channel {
        channel_id: i64,
        status: u16,
    },
    Credential {
        channel_id: i64,
        identity: String,
        status: u16,
    },
}

#[cfg(test)]
#[derive(Default)]
struct ConsecutiveErrorTracker {
    counts: HashMap<ErrorCounterKey, i64>,
}

#[cfg(test)]
impl ConsecutiveErrorTracker {
    fn record_success(
        &mut self,
        channel_id: i64,
        credential_identity: Option<&str>,
        credential: Option<&str>,
    ) {
        self.counts.retain(|key, _| match key {
            ErrorCounterKey::Channel {
                channel_id: tracked,
                ..
            } => *tracked != channel_id,
            ErrorCounterKey::Credential {
                channel_id: tracked,
                identity,
                ..
            } => {
                if *tracked != channel_id {
                    return true;
                }
                let successful_identity = credential_identity
                    .map(str::to_string)
                    .or_else(|| credential.map(credential_fingerprint));
                successful_identity.as_deref() != Some(identity.as_str())
            }
        });
    }

    fn record_failure(
        &mut self,
        channel_id: i64,
        status: u16,
        observation: &AttemptObservation,
        config: &AutoDisableConfig,
    ) -> Option<DisableAction> {
        if !config.enabled {
            self.counts.clear();
            return None;
        }
        self.reset_scope_except_status(channel_id, observation, Some(status));
        let threshold = config
            .statuses
            .iter()
            .find(|rule| rule.status == status)
            .map(|rule| rule.times)?;

        let credential = observation
            .credential
            .as_deref()
            .filter(|credential| !credential.is_empty());
        let key = match credential {
            Some(credential) => ErrorCounterKey::Credential {
                channel_id,
                identity: observation
                    .credential_identity
                    .clone()
                    .unwrap_or_else(|| credential_fingerprint(credential)),
                status,
            },
            None => ErrorCounterKey::Channel { channel_id, status },
        };
        let count = self.counts.entry(key.clone()).or_default();
        *count += 1;
        if *count < threshold {
            return None;
        }
        let actual_count = *count;
        match &key {
            ErrorCounterKey::Channel { .. } => {
                self.counts.retain(|tracked, _| {
                    !matches!(tracked, ErrorCounterKey::Channel { channel_id: tracked_id, .. } if *tracked_id == channel_id)
                });
                Some(DisableAction::Channel {
                    channel_id,
                    status,
                    threshold,
                    actual_count,
                })
            }
            ErrorCounterKey::Credential { identity, .. } => {
                let identity = identity.clone();
                self.counts.retain(|tracked, _| {
                    !matches!(tracked, ErrorCounterKey::Credential { channel_id: tracked_id, identity: tracked_identity, .. } if *tracked_id == channel_id && tracked_identity == &identity)
                });
                Some(DisableAction::Credential {
                    channel_id,
                    credential: credential.unwrap_or_default().to_string(),
                    status,
                    threshold,
                    actual_count,
                })
            }
        }
    }

    fn record_interruption(&mut self, channel_id: i64, observation: &AttemptObservation) {
        self.reset_scope_except_status(channel_id, observation, None);
    }

    fn reset_scope_except_status(
        &mut self,
        channel_id: i64,
        observation: &AttemptObservation,
        matching_status: Option<u16>,
    ) {
        let credential_identity = observation
            .credential
            .as_deref()
            .filter(|credential| !credential.is_empty())
            .map(|credential| {
                observation
                    .credential_identity
                    .clone()
                    .unwrap_or_else(|| credential_fingerprint(credential))
            });
        self.counts.retain(|tracked, _| match tracked {
            ErrorCounterKey::Channel {
                channel_id: tracked_id,
                status,
            } => {
                credential_identity.is_some()
                    || *tracked_id != channel_id
                    || matching_status == Some(*status)
            }
            ErrorCounterKey::Credential {
                channel_id: tracked_id,
                identity,
                status,
            } => {
                credential_identity.as_deref() != Some(identity.as_str())
                    || *tracked_id != channel_id
                    || matching_status == Some(*status)
            }
        });
    }
}

enum DisableAction {
    Channel {
        channel_id: i64,
        status: u16,
        threshold: i64,
        actual_count: i64,
    },
    Credential {
        channel_id: i64,
        credential: String,
        status: u16,
        threshold: i64,
        actual_count: i64,
    },
}

#[derive(Clone)]
struct ChannelDisabledEvent {
    channel_id: i64,
    channel_name: String,
    channel_provider: String,
    channel_base_url: String,
    status_code: u16,
    threshold: i64,
    actual_count: i64,
    reason: String,
    occurred_at: DateTime<Utc>,
}

async fn apply_disable_action(
    pool: &PgPool,
    action: DisableAction,
) -> Result<Option<ChannelDisabledEvent>, String> {
    match action {
        DisableAction::Channel {
            channel_id,
            status,
            threshold,
            actual_count,
        } => disable_channel(pool, channel_id, status, threshold, actual_count).await,
        DisableAction::Credential {
            channel_id,
            credential,
            status,
            threshold,
            actual_count,
        } => {
            disable_credential(
                pool,
                channel_id,
                &credential,
                status,
                threshold,
                actual_count,
            )
            .await
        }
    }
}

async fn disable_channel(
    pool: &PgPool,
    channel_id: i64,
    status: u16,
    threshold: i64,
    actual_count: i64,
) -> Result<Option<ChannelDisabledEvent>, String> {
    let reason = derive_error_message(status);
    let row = sqlx::query_as::<_, (String, String, Option<String>)>(
        "UPDATE channels SET status='disabled',error_message=$2,updated_at=now() \
         WHERE id=$1 AND status='enabled' AND deleted_at=0 \
         RETURNING name,\"type\",base_url",
    )
    .bind(channel_id)
    .bind(&reason)
    .fetch_optional(pool)
    .await
    .map_err(|error| error.to_string())?;
    let event = row.map(|(name, provider, base_url)| ChannelDisabledEvent {
        channel_id,
        channel_name: name,
        channel_provider: provider,
        channel_base_url: base_url.unwrap_or_default(),
        status_code: status,
        threshold,
        actual_count,
        reason,
        occurred_at: Utc::now(),
    });
    if event.is_some() {
        tracing::warn!(channel_id, status, "channel auto-disabled");
    }
    Ok(event)
}

async fn disable_credential(
    pool: &PgPool,
    channel_id: i64,
    credential: &str,
    status: u16,
    threshold: i64,
    actual_count: i64,
) -> Result<Option<ChannelDisabledEvent>, String> {
    let mut tx = pool.begin().await.map_err(|error| error.to_string())?;
    let row = sqlx::query_as::<
        _,
        (
            String,
            String,
            Option<String>,
            String,
            sqlx::types::Json<Value>,
            sqlx::types::Json<Value>,
        ),
    >(
        "SELECT name,\"type\",base_url,status,credentials, \
         COALESCE(disabled_api_keys,'[]'::jsonb) FROM channels \
         WHERE id=$1 AND deleted_at=0 FOR UPDATE",
    )
    .bind(channel_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|error| error.to_string())?;
    let Some((name, provider, base_url, channel_status, credentials, disabled_keys)) = row else {
        return Ok(None);
    };
    if channel_status != "enabled" {
        return Ok(None);
    }

    let credentials: ChannelCredentials =
        serde_json::from_value(credentials.0).map_err(|error| error.to_string())?;
    let all_keys = credentials.get_all_api_keys().unwrap_or_default();
    if !all_keys.iter().any(|key| key == credential) {
        return Ok(None);
    }
    let mut disabled: Vec<DisabledAPIKey> =
        serde_json::from_value(disabled_keys.0).map_err(|error| error.to_string())?;
    if disabled.iter().any(|item| item.key == credential) {
        return Ok(None);
    }
    disabled.push(DisabledAPIKey {
        key: credential.to_string(),
        disabled_at: Utc::now(),
        error_code: i64::from(status),
        reason: format!(
            "Auto-disabled after {actual_count} consecutive errors with status {status}"
        ),
    });
    let channel_exhausted = credentials
        .get_enabled_api_keys(&disabled)
        .unwrap_or_default()
        .is_empty();
    let disabled_json = serde_json::to_value(&disabled).map_err(|error| error.to_string())?;
    let event_reason = format!("All API keys disabled (last error: {status})");
    if channel_exhausted {
        sqlx::query(
            "UPDATE channels SET disabled_api_keys=$2,status='disabled',error_message=$3, \
             updated_at=now() WHERE id=$1",
        )
        .bind(channel_id)
        .bind(sqlx::types::Json(disabled_json))
        .bind(&event_reason)
        .execute(&mut *tx)
        .await
        .map_err(|error| error.to_string())?;
    } else {
        sqlx::query("UPDATE channels SET disabled_api_keys=$2,updated_at=now() WHERE id=$1")
            .bind(channel_id)
            .bind(sqlx::types::Json(disabled_json))
            .execute(&mut *tx)
            .await
            .map_err(|error| error.to_string())?;
    }
    tx.commit().await.map_err(|error| error.to_string())?;

    tracing::info!(
        channel_id,
        status,
        channel_exhausted,
        "channel credential auto-disabled"
    );

    if channel_exhausted {
        Ok(Some(ChannelDisabledEvent {
            channel_id,
            channel_name: name,
            channel_provider: provider,
            channel_base_url: base_url.unwrap_or_default(),
            status_code: status,
            threshold,
            actual_count,
            reason: event_reason,
            occurred_at: Utc::now(),
        }))
    } else {
        Ok(None)
    }
}

fn derive_error_message(status: u16) -> String {
    conduit_services::channel_service::derive_error_message(i64::from(status))
}

struct WebhookNotifierRuntime {
    system: Arc<SystemService>,
    client: reqwest::Client,
}

impl WebhookNotifierRuntime {
    fn new(system: Arc<SystemService>) -> Self {
        Self {
            system,
            client: reqwest::Client::new(),
        }
    }

    async fn notify_channel_auto_disabled(&self, event: ChannelDisabledEvent) {
        let ctx = RequestContext::new(PolicyContext::new(Principal::system()));
        let config = match self.system.webhook_notifier_config(&ctx).await {
            Ok(config) => config,
            Err(error) => {
                tracing::warn!(%error, "failed to load webhook notifier config");
                return;
            }
        };
        for target in select_targets(&config, EVENT_CHANNEL_AUTO_DISABLED) {
            if let Err(error) = self.send(target, &event).await {
                tracing::warn!(
                    event = EVENT_CHANNEL_AUTO_DISABLED,
                    target = %target.name,
                    %error,
                    "failed to send webhook notification"
                );
            }
        }
    }

    async fn send(
        &self,
        target: &WebhookTarget,
        event: &ChannelDisabledEvent,
    ) -> Result<(), String> {
        let body = render_webhook_template(&target.body, event);
        let headers = render_webhook_headers(target, event)?;
        let client = client_for_target(&self.client, target)?;
        let timeout_ms = u64::try_from(target.timeout_ms)
            .ok()
            .filter(|timeout| *timeout > 0)
            .unwrap_or(DEFAULT_WEBHOOK_TIMEOUT_MS);
        client
            .post(target.url.trim())
            .headers(headers)
            .timeout(Duration::from_millis(timeout_ms))
            .body(body)
            .send()
            .await
            .map_err(|error| error.to_string())?;
        Ok(())
    }
}

fn select_targets<'a>(config: &'a WebhookNotifierConfig, event: &str) -> Vec<&'a WebhookTarget> {
    let Some(subscription) = config
        .subscriptions
        .iter()
        .find(|subscription| subscription.event == event)
    else {
        return Vec::new();
    };
    subscription
        .target_names
        .iter()
        .filter_map(|name| {
            config.targets.iter().find(|target| {
                target.name == *name && target.enabled && !target.url.trim().is_empty()
            })
        })
        .collect()
}

fn render_webhook_headers(
    target: &WebhookTarget,
    event: &ChannelDisabledEvent,
) -> Result<HeaderMap, String> {
    let mut headers = HeaderMap::new();
    for entry in &target.headers {
        let key = entry.key.trim();
        if key.is_empty() {
            continue;
        }
        let name = HeaderName::from_bytes(key.as_bytes()).map_err(|error| error.to_string())?;
        let value = HeaderValue::from_str(&render_webhook_template(&entry.value, event))
            .map_err(|error| error.to_string())?;
        headers.insert(name, value);
    }
    if !headers.contains_key(CONTENT_TYPE) {
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    }
    Ok(headers)
}

fn render_webhook_template(template: &str, event: &ChannelDisabledEvent) -> String {
    let occurred_at = event.occurred_at.to_rfc3339_opts(SecondsFormat::Secs, true);
    let replacements = [
        ("{{.Event}}", EVENT_CHANNEL_AUTO_DISABLED.to_string()),
        ("{{.Severity}}", "warning".to_string()),
        ("{{.OccurredAt}}", occurred_at),
        ("{{.Channel.ID}}", event.channel_id.to_string()),
        ("{{.Channel.Name}}", event.channel_name.clone()),
        ("{{.Channel.Provider}}", event.channel_provider.clone()),
        ("{{.Channel.BaseURL}}", event.channel_base_url.clone()),
        ("{{.Channel.Status}}", "disabled".to_string()),
        ("{{.Trigger.Type}}", "error_status_rule".to_string()),
        ("{{.Trigger.StatusCode}}", event.status_code.to_string()),
        ("{{.Trigger.Threshold}}", event.threshold.to_string()),
        ("{{.Trigger.ActualCount}}", event.actual_count.to_string()),
        ("{{.Trigger.Reason}}", event.reason.clone()),
    ];
    replacements
        .into_iter()
        .fold(template.to_string(), |rendered, (key, value)| {
            rendered.replace(key, &value)
        })
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct WebhookProxyConfig {
    #[serde(rename = "type")]
    proxy_type: String,
    url: String,
    username: String,
    password: String,
}

fn client_for_target(
    default_client: &reqwest::Client,
    target: &WebhookTarget,
) -> Result<reqwest::Client, String> {
    let Some(proxy_value) = target.extra.get("proxy") else {
        return Ok(default_client.clone());
    };
    let proxy: WebhookProxyConfig =
        serde_json::from_value(proxy_value.clone()).map_err(|error| error.to_string())?;
    match proxy.proxy_type.as_str() {
        "DISABLED" | "disabled" => reqwest::Client::builder()
            .no_proxy()
            .build()
            .map_err(|error| error.to_string()),
        "URL" | "url" if !proxy.url.trim().is_empty() => {
            let mut configured =
                reqwest::Proxy::all(proxy.url.trim()).map_err(|error| error.to_string())?;
            if !proxy.username.is_empty() {
                configured = configured.basic_auth(&proxy.username, &proxy.password);
            }
            reqwest::Client::builder()
                .proxy(configured)
                .build()
                .map_err(|error| error.to_string())
        }
        "ENVIRONMENT" | "environment" | "" => Ok(default_client.clone()),
        _ => Ok(default_client.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation(outcome: AttemptObservationOutcome) -> AttemptObservation {
        AttemptObservation {
            channel_id: "7".to_string(),
            credential: Some("secret".to_string()),
            credential_identity: Some("sha256:secret".to_string()),
            outcome,
        }
    }

    fn config() -> AutoDisableConfig {
        AutoDisableConfig {
            enabled: true,
            statuses: vec![AutoDisableStatus {
                status: 401,
                times: 2,
            }],
        }
    }

    #[test]
    fn tracker_requires_consecutive_matching_failures() {
        let mut tracker = ConsecutiveErrorTracker::default();
        let failed = observation(AttemptObservationOutcome::Failed {
            provider_status: Some(401),
        });
        assert!(tracker.record_failure(7, 401, &failed, &config()).is_none());
        tracker.record_success(7, Some("sha256:secret"), Some("secret"));
        assert!(tracker.record_failure(7, 401, &failed, &config()).is_none());
        assert!(matches!(
            tracker.record_failure(7, 401, &failed, &config()),
            Some(DisableAction::Credential {
                actual_count: 2,
                ..
            })
        ));
    }

    #[test]
    fn tracker_ignores_unconfigured_statuses() {
        let mut tracker = ConsecutiveErrorTracker::default();
        let failed = observation(AttemptObservationOutcome::Failed {
            provider_status: Some(503),
        });
        assert!(tracker.record_failure(7, 503, &failed, &config()).is_none());
        assert!(tracker.counts.is_empty());
    }

    #[test]
    fn tracker_resets_when_the_failure_status_changes() {
        let mut tracker = ConsecutiveErrorTracker::default();
        let config = AutoDisableConfig {
            enabled: true,
            statuses: vec![
                AutoDisableStatus {
                    status: 401,
                    times: 2,
                },
                AutoDisableStatus {
                    status: 500,
                    times: 2,
                },
            ],
        };
        let unauthorized = observation(AttemptObservationOutcome::Failed {
            provider_status: Some(401),
        });
        let server_error = observation(AttemptObservationOutcome::Failed {
            provider_status: Some(500),
        });

        assert!(
            tracker
                .record_failure(7, 401, &unauthorized, &config)
                .is_none()
        );
        assert!(
            tracker
                .record_failure(7, 500, &server_error, &config)
                .is_none()
        );
        assert!(
            tracker
                .record_failure(7, 401, &unauthorized, &config)
                .is_none()
        );
        assert!(matches!(
            tracker.record_failure(7, 401, &unauthorized, &config),
            Some(DisableAction::Credential {
                actual_count: 2,
                ..
            })
        ));
    }

    #[test]
    fn tracker_resets_when_a_failure_has_no_provider_status() {
        let mut tracker = ConsecutiveErrorTracker::default();
        let failed = observation(AttemptObservationOutcome::Failed {
            provider_status: Some(401),
        });
        let interrupted = observation(AttemptObservationOutcome::Failed {
            provider_status: None,
        });

        assert!(tracker.record_failure(7, 401, &failed, &config()).is_none());
        tracker.record_interruption(7, &interrupted);
        assert!(tracker.record_failure(7, 401, &failed, &config()).is_none());
    }

    #[test]
    fn auto_disable_config_rejects_invalid_or_duplicate_rules() {
        for statuses in [
            vec![AutoDisableStatus {
                status: 399,
                times: 1,
            }],
            vec![AutoDisableStatus {
                status: 500,
                times: 0,
            }],
            vec![
                AutoDisableStatus {
                    status: 500,
                    times: 1,
                },
                AutoDisableStatus {
                    status: 500,
                    times: 2,
                },
            ],
        ] {
            assert!(
                validate_auto_disable_config(&AutoDisableConfig {
                    enabled: true,
                    statuses,
                })
                .is_err()
            );
        }
        assert!(validate_auto_disable_config(&config()).is_ok());
    }

    #[test]
    fn webhook_template_uses_go_variable_names() {
        let event = ChannelDisabledEvent {
            channel_id: 7,
            channel_name: "BOHE".to_string(),
            channel_provider: "openai".to_string(),
            channel_base_url: "https://example.com".to_string(),
            status_code: 401,
            threshold: 2,
            actual_count: 2,
            reason: "Unauthorized".to_string(),
            occurred_at: DateTime::parse_from_rfc3339("2026-08-25T10:00:00Z")
                .expect("valid timestamp")
                .with_timezone(&Utc),
        };
        let rendered = render_webhook_template(
            "{{.Event}} {{.Channel.ID}} {{.Channel.Name}} {{.Trigger.Reason}}",
            &event,
        );
        assert_eq!(rendered, "channel.auto_disabled 7 BOHE Unauthorized");
    }

    #[tokio::test]
    async fn postgres_disables_channel_only_after_last_credential()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let channel_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO channels (\"type\",name,status,credentials,supported_models,default_test_model) \
             VALUES ('openai','auto-disable-test','enabled',$1,'[]'::jsonb,'test-model') \
             RETURNING id",
        )
        .bind(sqlx::types::Json(serde_json::json!({
            "apiKeys": ["key-a", "key-b"]
        })))
        .fetch_one(&database.pool)
        .await?;

        assert!(
            disable_credential(&database.pool, channel_id, "key-a", 401, 1, 1)
                .await?
                .is_none()
        );
        let (status, disabled_count) = sqlx::query_as::<_, (String, i32)>(
            "SELECT status,jsonb_array_length(disabled_api_keys) FROM channels WHERE id=$1",
        )
        .bind(channel_id)
        .fetch_one(&database.pool)
        .await?;
        assert_eq!(status, "enabled");
        assert_eq!(disabled_count, 1);

        let event = disable_credential(&database.pool, channel_id, "key-b", 401, 1, 1)
            .await?
            .ok_or("last credential should disable the channel")?;
        assert_eq!(event.channel_id, channel_id);
        assert_eq!(event.status_code, 401);
        let status = sqlx::query_scalar::<_, String>("SELECT status FROM channels WHERE id=$1")
            .bind(channel_id)
            .fetch_one(&database.pool)
            .await?;
        assert_eq!(status, "disabled");

        database.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn postgres_counter_requires_consecutive_persisted_failures()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let channel_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO channels (\"type\",name,status,credentials,supported_models,default_test_model) \
             VALUES ('openai','persistent-counter-test','enabled','{}'::jsonb,'[]'::jsonb,'test-model') \
             RETURNING id",
        )
        .fetch_one(&database.pool)
        .await?;
        let observation = AttemptObservation {
            channel_id: channel_id.to_string(),
            credential: Some("secret".to_string()),
            credential_identity: Some("sha256:secret".to_string()),
            outcome: AttemptObservationOutcome::Failed {
                provider_status: Some(401),
            },
        };

        for request_id in [1_i64, 2] {
            sqlx::query(
                "INSERT INTO request_executions \
                 (request_id,channel_id,credential_identity,model_id,request_body,status,response_status_code) \
                 VALUES ($1,$2,'sha256:secret','test-model','{}'::jsonb,'failed',401)",
            )
            .bind(request_id)
            .bind(channel_id)
            .execute(&database.pool)
            .await?;
            let action =
                persistent_disable_action(&database.pool, channel_id, 401, &observation, &config())
                    .await?;
            if request_id == 1 {
                assert!(action.is_none());
            } else {
                assert!(matches!(
                    action,
                    Some(DisableAction::Credential {
                        actual_count: 2,
                        ..
                    })
                ));
            }
        }

        sqlx::query(
            "INSERT INTO request_executions \
             (request_id,channel_id,credential_identity,model_id,request_body,status,response_status_code) \
             VALUES (3,$1,'sha256:secret','test-model','{}'::jsonb,'completed',200), \
                    (4,$1,'sha256:secret','test-model','{}'::jsonb,'failed',401)",
        )
        .bind(channel_id)
        .execute(&database.pool)
        .await?;
        assert!(
            persistent_disable_action(&database.pool, channel_id, 401, &observation, &config(),)
                .await?
                .is_none()
        );

        database.cleanup().await?;
        Ok(())
    }
}
