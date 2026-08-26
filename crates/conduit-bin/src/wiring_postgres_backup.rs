//! PostgreSQL backup, restore, and automatic-backup adapter.
//!
//! PostgreSQL rows are converted with `to_jsonb`, so JSONB, booleans, numeric
//! fields, and timestamps keep their native shape.  Restore uses
//! `jsonb_populate_record` against the destination table's composite type;
//! PostgreSQL therefore performs the inverse type conversion instead of a
//! fragile application-side string binder.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use chrono::{DateTime, Timelike, Utc};
use conduit_services::{BackupDataSource, BackupSection, BackupServiceError, BackupServiceResult};
use conduit_storage::StorageError;
use serde_json::{Map, Value};
use sqlx::{PgPool, Postgres, Row, Transaction, types::Json};

const PRICING_CONFIGURATION_TABLES: &[&str] = &[
    "upstream_model_deployments",
    "model_routes",
    "channel_model_price_versions",
    "price_books",
    "price_book_versions",
    "price_book_items",
    "price_tiers",
    "project_commercial_profiles",
    "project_price_adjustments",
    "provider_price_snapshots",
    "provider_price_rows",
    "provider_price_change_events",
    "change_sets",
    "change_set_items",
    "change_set_events",
    "pricing_change_audits",
];

pub struct PgBackupDataSourceAdapter {
    pool: PgPool,
}

impl PgBackupDataSourceAdapter {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    async fn load_table(&self, table: &str) -> BackupServiceResult<Value> {
        let rows = sqlx::query(&format!(
            "SELECT to_jsonb(backup_row) AS payload FROM (SELECT * FROM {table}) backup_row"
        ))
        .fetch_all(&self.pool)
        .await
        .map_err(|error| backup_storage_error(format!("load {table} failed: {error}")))?;
        let values = rows
            .into_iter()
            .map(|row| row.get::<Json<Value>, _>("payload").0)
            .collect();
        Ok(Value::Array(values))
    }

    async fn load_api_keys(&self) -> BackupServiceResult<Value> {
        let rows = sqlx::query(
            "SELECT to_jsonb(api_key) || jsonb_build_object( \
                 'project_name', COALESCE(project.name, '')) AS payload \
             FROM api_keys api_key LEFT JOIN projects project ON project.id = api_key.project_id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|error| {
            backup_storage_error(format!("load api_keys with project name failed: {error}"))
        })?;
        Ok(Value::Array(
            rows.into_iter()
                .map(|row| row.get::<Json<Value>, _>("payload").0)
                .collect(),
        ))
    }

    async fn load_pricing_configuration(&self) -> BackupServiceResult<Value> {
        let accounting_settings = sqlx::query(
            "SELECT to_jsonb(source) AS payload FROM (\
             SELECT * FROM systems WHERE key=$1 AND deleted_at=0) source",
        )
        .bind(conduit_services::system_key::GENERAL_SETTINGS)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| backup_storage_error(format!("load accounting settings failed: {error}")))?
        .into_iter()
        .map(|row| row.get::<Json<Value>, _>("payload").0)
        .collect();
        let mut sections = Map::new();
        sections.insert(
            "accounting_settings".into(),
            Value::Array(accounting_settings),
        );
        for table in PRICING_CONFIGURATION_TABLES {
            sections.insert((*table).to_string(), self.load_table(table).await?);
        }
        Ok(Value::Object(sections))
    }
}

fn backup_storage_error(message: String) -> BackupServiceError {
    BackupServiceError::Storage(StorageError::Operation(message))
}

fn section_table(section: BackupSection) -> &'static str {
    match section {
        BackupSection::Projects => "projects",
        BackupSection::Channels => "channels",
        BackupSection::ModelPrices => "channel_model_prices",
        BackupSection::PricingConfiguration => "pricing_configuration",
        BackupSection::Models => "models",
        BackupSection::ApiKeys => "api_keys",
        BackupSection::UsageRequests => "requests",
        BackupSection::UsageLogs => "usage_logs",
    }
}

#[async_trait]
impl BackupDataSource for PgBackupDataSourceAdapter {
    async fn load_section(
        &self,
        _ctx: &conduit_db::RequestContext,
        section: BackupSection,
    ) -> BackupServiceResult<Value> {
        match section {
            BackupSection::ApiKeys => self.load_api_keys().await,
            BackupSection::PricingConfiguration => self.load_pricing_configuration().await,
            _ => self.load_table(section_table(section)).await,
        }
    }
}

pub struct PgBackupExtAdapter {
    service: Arc<conduit_services::BackupService>,
    pool: PgPool,
    system: Arc<conduit_services::SystemService>,
    data_storage_repo: Arc<dyn conduit_db::repo::data_storage_repo::DataStorageRepo>,
    #[cfg(test)]
    test_backup_encryption_key: Option<[u8; 32]>,
}

impl PgBackupExtAdapter {
    pub fn new(
        pool: PgPool,
        system: Arc<conduit_services::SystemService>,
        data_storage_repo: Arc<dyn conduit_db::repo::data_storage_repo::DataStorageRepo>,
    ) -> Self {
        let repo = Arc::new(UnusedBackupRepo);
        let storage = Arc::new(conduit_storage::InMemoryStorageAdapter::new());
        let data_source = Arc::new(PgBackupDataSourceAdapter::new(pool.clone()));
        let service = Arc::new(
            conduit_services::BackupService::new(repo, storage).with_data_source(data_source),
        );
        Self {
            service,
            pool,
            system,
            data_storage_repo,
            #[cfg(test)]
            test_backup_encryption_key: None,
        }
    }

    #[cfg(test)]
    fn with_test_backup_encryption_key(mut self, key: [u8; 32]) -> Self {
        self.test_backup_encryption_key = Some(key);
        self
    }

    fn encrypt_backup_archive(
        &self,
        bytes: &[u8],
        sections: conduit_services::backup_archive_crypto::BackupSections,
    ) -> Result<Vec<u8>, String> {
        #[cfg(test)]
        if let Some(key) = self.test_backup_encryption_key.as_ref() {
            return conduit_services::backup_archive_crypto::encrypt_with_key(bytes, key);
        }
        conduit_services::backup_archive_crypto::encrypt_if_sensitive(bytes, sections)
    }

    fn decrypt_backup_archive(&self, bytes: &[u8]) -> Result<Vec<u8>, String> {
        #[cfg(test)]
        if let Some(key) = self.test_backup_encryption_key.as_ref() {
            return conduit_services::backup_archive_crypto::decrypt_if_enveloped_with_key(
                bytes, key,
            );
        }
        conduit_services::backup_archive_crypto::decrypt_if_enveloped(bytes)
    }
}

struct UnusedBackupRepo;

#[async_trait]
impl conduit_services::BackupRepo for UnusedBackupRepo {
    async fn create_backup(
        &self,
        _ctx: &conduit_db::RequestContext,
        _job: conduit_services::BackupJob,
    ) -> BackupServiceResult<conduit_services::BackupJob> {
        Err(backup_storage_error(
            "backup metadata persistence is not used by the inline adapter".to_string(),
        ))
    }

    async fn get_backup(
        &self,
        _ctx: &conduit_db::RequestContext,
        _backup_id: &str,
    ) -> BackupServiceResult<Option<conduit_services::BackupJob>> {
        Ok(None)
    }

    async fn update_backup_status(
        &self,
        _ctx: &conduit_db::RequestContext,
        _backup_id: &str,
        _expected_status: conduit_services::BackupStatus,
        _job: conduit_services::BackupJob,
    ) -> BackupServiceResult<Option<conduit_services::BackupJob>> {
        Ok(None)
    }

    async fn create_restore_request(
        &self,
        _ctx: &conduit_db::RequestContext,
        request: conduit_services::BackupRestoreRequest,
    ) -> BackupServiceResult<conduit_services::BackupRestoreRequest> {
        Ok(request)
    }
}

#[async_trait]
impl conduit_admin_graphql::backup_ext::BackupExtServices for PgBackupExtAdapter {
    async fn run_backup(
        &self,
        opts: conduit_admin_graphql::backup_ext::BackupOptionsInput,
    ) -> Result<String, conduit_admin_graphql::backup_ext::BackupExtError> {
        use conduit_admin_graphql::backup_ext::BackupExtError;
        let bytes = self
            .service
            .dump(
                &system_context(),
                conduit_services::BackupOptions {
                    include_projects: true,
                    include_channels: opts.include_channels,
                    include_models: opts.include_models,
                    include_api_keys: opts.include_api_keys,
                    include_model_prices: opts.include_model_prices,
                    include_usage_stats: opts.include_usage_stats,
                    include_request_logs: opts.include_request_logs,
                },
            )
            .await
            .map_err(|error| BackupExtError::Backup(error.to_string()))?;
        let bytes = self
            .encrypt_backup_archive(
                &bytes,
                conduit_services::backup_archive_crypto::BackupSections {
                    include_channels: opts.include_channels,
                    include_api_keys: opts.include_api_keys,
                    include_request_logs: opts.include_request_logs,
                },
            )
            .map_err(BackupExtError::Backup)?;
        Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
    }

    async fn restore(
        &self,
        data: Vec<u8>,
        opts: conduit_admin_graphql::backup_ext::RestoreOptionsInput,
    ) -> Result<(), conduit_admin_graphql::backup_ext::BackupExtError> {
        use conduit_admin_graphql::backup_ext::BackupExtError;
        let data = self
            .decrypt_backup_archive(&data)
            .map_err(BackupExtError::Restore)?;
        restore_archive(&self.pool, &data, opts).await
    }

    async fn trigger_auto_backup(
        &self,
    ) -> Result<(), conduit_admin_graphql::backup_ext::BackupExtError> {
        let service = self.service.clone();
        let system = self.system.clone();
        let data_storage_repo = self.data_storage_repo.clone();
        tokio::spawn(async move {
            if let Err(error) = run_auto_backup(service, system.clone(), data_storage_repo).await {
                tracing::error!(%error, "manual PostgreSQL automatic-backup trigger failed");
                record_auto_backup_error(&system, error).await;
            }
        });
        Ok(())
    }
}

fn system_context() -> conduit_db::RequestContext {
    conduit_db::RequestContext::new(conduit_db::PolicyContext::new(
        conduit_db::Principal::system(),
    ))
}

async fn record_auto_backup_error(system: &conduit_services::SystemService, error: String) {
    use conduit_services::system_service::system_key;
    let ctx = system_context();
    let mut settings = system
        .get_json::<conduit_services::AutoBackupSettings>(&ctx, system_key::AUTO_BACKUP_SETTINGS)
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    settings.last_backup_error = Some(error);
    let _ = system
        .set_json(&ctx, system_key::AUTO_BACKUP_SETTINGS, &settings)
        .await;
}

async fn run_auto_backup(
    service: Arc<conduit_services::BackupService>,
    system: Arc<conduit_services::SystemService>,
    data_storage_repo: Arc<dyn conduit_db::repo::data_storage_repo::DataStorageRepo>,
) -> Result<(), String> {
    use conduit_services::system_service::system_key;
    use conduit_storage::{DataStorageConfig, DataStorageKind, StorageMetadata, StorageObject};

    let ctx = system_context();
    let mut settings = system
        .get_json::<conduit_services::AutoBackupSettings>(&ctx, system_key::AUTO_BACKUP_SETTINGS)
        .await
        .map_err(|error| error.to_string())?
        .unwrap_or_default();
    if settings.data_storage_id == 0 {
        return Err("auto backup data storage is not configured".to_string());
    }
    let storage = data_storage_repo
        .find_data_storage_unchecked(&ctx, &settings.data_storage_id.to_string())
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| {
            format!(
                "auto backup data storage {} not found",
                settings.data_storage_id
            )
        })?;
    if storage.status != "active" {
        return Err(format!(
            "auto backup data storage {} is not active",
            storage.id
        ));
    }
    let kind = match storage.storage_type.as_str() {
        "database" => {
            return Err("auto backup requires a file or object data storage".to_string());
        }
        "fs" => DataStorageKind::Local,
        "s3" => DataStorageKind::S3,
        "gcs" => DataStorageKind::Gcs,
        "webdav" => DataStorageKind::WebDav,
        other => DataStorageKind::Unknown(other.to_string()),
    };
    let config = DataStorageConfig::from_value(&storage.settings)
        .map_err(|error| format!("invalid auto backup storage settings: {error}"))?;
    let backend = conduit_storage::build_storage_backend(&kind, Some(&config))
        .map_err(|error| error.to_string())?;
    let bytes = service
        .dump(
            &ctx,
            conduit_services::BackupOptions {
                include_projects: true,
                include_channels: settings.include_channels,
                include_models: settings.include_models,
                include_api_keys: settings.include_api_keys,
                include_model_prices: settings.include_model_prices,
                include_usage_stats: settings.include_usage_stats,
                include_request_logs: settings.include_request_logs,
            },
        )
        .await
        .map_err(|error| error.to_string())?;
    let bytes = conduit_services::backup_archive_crypto::encrypt_if_sensitive(
        &bytes,
        conduit_services::backup_archive_crypto::BackupSections {
            include_channels: settings.include_channels,
            include_api_keys: settings.include_api_keys,
            include_request_logs: settings.include_request_logs,
        },
    )?;
    let now = chrono::Utc::now();
    let key = format!("backups/auto/conduit-{}.json", now.format("%Y%m%dT%H%M%SZ"));
    let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    backend
        .put(
            StorageObject::new(key.clone(), bytes).with_metadata(
                StorageMetadata::new(key, size).with_content_type("application/json"),
            ),
        )
        .await
        .map_err(|error| error.to_string())?;
    settings.last_backup_at = Some(now);
    settings.last_backup_error = None;
    system
        .set_json(&ctx, system_key::AUTO_BACKUP_SETTINGS, &settings)
        .await
        .map_err(|error| error.to_string())?;
    Ok(())
}

async fn run_scheduled_auto_backup(
    service: Arc<conduit_services::BackupService>,
    system: Arc<conduit_services::SystemService>,
    data_storage_repo: Arc<dyn conduit_db::repo::data_storage_repo::DataStorageRepo>,
) -> Result<(), String> {
    use conduit_services::system_service::system_key;

    let now = Utc::now();
    let settings = system
        .get_json::<conduit_services::AutoBackupSettings>(
            &system_context(),
            system_key::AUTO_BACKUP_SETTINGS,
        )
        .await
        .map_err(|error| error.to_string())?
        .unwrap_or_default();
    if !scheduled_auto_backup_due(&settings, now) {
        return Ok(());
    }
    run_auto_backup(service, system, data_storage_repo).await
}

fn scheduled_auto_backup_due(
    settings: &conduit_services::AutoBackupSettings,
    now: DateTime<Utc>,
) -> bool {
    if !settings.enabled || now.hour() != 2 {
        return false;
    }
    let frequency = match settings.frequency {
        conduit_services::BackupFrequency::Daily => conduit_scheduler::BackupFrequency::Daily,
        conduit_services::BackupFrequency::Weekly => conduit_scheduler::BackupFrequency::Weekly,
        conduit_services::BackupFrequency::Monthly => conduit_scheduler::BackupFrequency::Monthly,
    };
    conduit_scheduler::should_run_backup(frequency, now)
        && !settings
            .last_backup_at
            .is_some_and(|last| last.date_naive() == now.date_naive())
}

impl conduit_scheduler::AutoBackupExecutor for PgBackupExtAdapter {
    fn run_backup(&self) -> Result<(), String> {
        let service = self.service.clone();
        let system = self.system.clone();
        let data_storage_repo = self.data_storage_repo.clone();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                match run_scheduled_auto_backup(service, system.clone(), data_storage_repo).await {
                    Ok(()) => Ok(()),
                    Err(error) => {
                        record_auto_backup_error(&system, error.clone()).await;
                        Err(error)
                    }
                }
            })
        })
    }
}

async fn restore_archive(
    pool: &PgPool,
    data: &[u8],
    opts: conduit_admin_graphql::backup_ext::RestoreOptionsInput,
) -> Result<(), conduit_admin_graphql::backup_ext::BackupExtError> {
    use conduit_admin_graphql::backup_ext::{BackupConflictStrategy, BackupExtError};
    use conduit_services::{
        parse_backup_manifest, supported_backup_versions, validate_backup_version,
    };

    let manifest =
        parse_backup_manifest(data).map_err(|error| BackupExtError::Restore(error.to_string()))?;
    validate_backup_version(&manifest.version, supported_backup_versions())
        .map_err(|error| BackupExtError::Restore(error.to_string()))?;

    let mut tx = pool
        .begin()
        .await
        .map_err(|error| BackupExtError::Restore(error.to_string()))?;
    crate::wiring::lock_accounting_currency_price_writes(&mut tx)
        .await
        .map_err(|error| BackupExtError::Restore(error.to_string()))?;
    if opts.include_model_prices {
        validate_pricing_restore_currency(
            &mut tx,
            manifest.entities.channel_model_prices.as_ref(),
            manifest.entities.pricing_configuration.as_ref(),
        )
        .await
        .map_err(BackupExtError::Restore)?;
    }
    // Parent-like entities precede their consumers.  The current schema does
    // not require every foreign key, but this order remains valid if those
    // constraints are strengthened later.
    let sections = [
        (
            true,
            "projects",
            manifest.entities.projects.as_ref(),
            BackupConflictStrategy::Skip,
        ),
        (
            opts.include_models,
            "models",
            manifest.entities.models.as_ref(),
            opts.model_conflict_strategy,
        ),
        (
            opts.include_channels,
            "channels",
            manifest.entities.channels.as_ref(),
            opts.channel_conflict_strategy,
        ),
        (
            opts.include_model_prices,
            "channel_model_prices",
            manifest.entities.channel_model_prices.as_ref(),
            opts.model_price_conflict_strategy,
        ),
        (
            opts.include_api_keys,
            "api_keys",
            manifest.entities.api_keys.as_ref(),
            opts.api_key_conflict_strategy,
        ),
        (
            opts.include_request_logs,
            "requests",
            manifest.entities.usage_requests.as_ref(),
            BackupConflictStrategy::Skip,
        ),
        (
            opts.include_usage_stats,
            "usage_logs",
            manifest.entities.usage_logs.as_ref(),
            BackupConflictStrategy::Skip,
        ),
    ];

    for (enabled, table, section, strategy) in sections {
        if !enabled {
            continue;
        }
        let Some(rows) = section.and_then(Value::as_array) else {
            continue;
        };
        let columns = table_columns(&mut tx, table)
            .await
            .map_err(|error| BackupExtError::Restore(error.to_string()))?;
        for row in rows {
            let Some(object) = row.as_object() else {
                return Err(BackupExtError::Restore(format!(
                    "{table} section contains a non-object row"
                )));
            };
            restore_row(&mut tx, table, &columns, object, strategy)
                .await
                .map_err(BackupExtError::Restore)?;
        }
        reset_id_sequence(&mut tx, table)
            .await
            .map_err(|error| BackupExtError::Restore(error.to_string()))?;
    }

    if opts.include_model_prices {
        restore_pricing_configuration(
            &mut tx,
            manifest.entities.pricing_configuration.as_ref(),
            opts.model_price_conflict_strategy,
        )
        .await
        .map_err(BackupExtError::Restore)?;
    }
    tx.commit()
        .await
        .map_err(|error| BackupExtError::Restore(error.to_string()))
}

async fn validate_pricing_restore_currency(
    tx: &mut Transaction<'_, Postgres>,
    channel_model_prices: Option<&Value>,
    configuration: Option<&Value>,
) -> Result<(), String> {
    let current_currency = current_accounting_currency(tx).await?;
    let archived_currency =
        archived_accounting_currency(configuration)?.unwrap_or_else(|| current_currency.clone());

    validate_archived_price_currencies(channel_model_prices, configuration, &archived_currency)?;

    if !current_currency.eq_ignore_ascii_case(&archived_currency)
        && target_has_pricing_state(tx).await?
    {
        return Err(format!(
            "cannot restore accounting currency {archived_currency} into pricing state using {current_currency}; rebuild or restore into an empty pricing database"
        ));
    }
    Ok(())
}

async fn current_accounting_currency(tx: &mut Transaction<'_, Postgres>) -> Result<String, String> {
    let raw = sqlx::query_scalar::<_, String>(
        "SELECT value FROM systems WHERE key=$1 AND deleted_at=0 LIMIT 1",
    )
    .bind(conduit_services::system_key::GENERAL_SETTINGS)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|error| format!("failed to load current accounting currency: {error}"))?;
    let currency = raw
        .as_deref()
        .map(parse_accounting_currency_value)
        .transpose()?
        .unwrap_or_else(|| {
            conduit_core::objects::money::DEFAULT_ACCOUNTING_CURRENCY_CODE.to_string()
        });
    normalize_accounting_currency(&currency, "current accounting settings")
}

fn archived_accounting_currency(configuration: Option<&Value>) -> Result<Option<String>, String> {
    let Some(rows) = configuration
        .and_then(Value::as_object)
        .and_then(|sections| sections.get("accounting_settings"))
        .and_then(Value::as_array)
    else {
        return Ok(None);
    };
    if rows.len() > 1 {
        return Err("accounting_settings backup section contains multiple rows".into());
    }
    let Some(object) = rows.first().and_then(Value::as_object) else {
        return if rows.is_empty() {
            Ok(None)
        } else {
            Err("accounting_settings backup section contains a non-object row".into())
        };
    };
    if object.get("key").and_then(Value::as_str)
        != Some(conduit_services::system_key::GENERAL_SETTINGS)
    {
        return Err("accounting_settings section contains an unexpected system key".into());
    }
    let raw = object
        .get("value")
        .and_then(Value::as_str)
        .ok_or_else(|| "accounting_settings backup row has no string value".to_string())?;
    parse_accounting_currency_value(raw).map(Some)
}

fn parse_accounting_currency_value(raw: &str) -> Result<String, String> {
    let value: Value = serde_json::from_str(raw)
        .map_err(|error| format!("invalid accounting settings JSON: {error}"))?;
    let currency = value
        .get("accounting_currency_code")
        .or_else(|| value.get("accountingCurrencyCode"))
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| "accounting currency code is not a string".to_string())
        })
        .transpose()?
        .unwrap_or(conduit_core::objects::money::DEFAULT_ACCOUNTING_CURRENCY_CODE);
    normalize_accounting_currency(currency, "accounting settings")
}

fn normalize_accounting_currency(currency: &str, source: &str) -> Result<String, String> {
    let currency = currency.trim().to_ascii_uppercase();
    if currency.len() == 3
        && currency
            .chars()
            .all(|character| character.is_ascii_alphabetic())
    {
        Ok(currency)
    } else {
        Err(format!("{source} contain an invalid accounting currency"))
    }
}

fn validate_archived_price_currencies(
    channel_model_prices: Option<&Value>,
    configuration: Option<&Value>,
    accounting_currency: &str,
) -> Result<(), String> {
    validate_price_section_currency(
        "channel_model_prices",
        channel_model_prices,
        "currency_code",
        accounting_currency,
    )?;
    let sections = configuration.and_then(Value::as_object);
    validate_price_section_currency(
        "channel_model_price_versions",
        sections.and_then(|value| value.get("channel_model_price_versions")),
        "currency_code",
        accounting_currency,
    )?;
    validate_price_section_currency(
        "price_books",
        sections.and_then(|value| value.get("price_books")),
        "currency",
        accounting_currency,
    )
}

fn validate_price_section_currency(
    section: &str,
    rows: Option<&Value>,
    field: &str,
    accounting_currency: &str,
) -> Result<(), String> {
    let Some(rows) = rows else { return Ok(()) };
    let rows = rows
        .as_array()
        .ok_or_else(|| format!("{section} backup section is not an array"))?;
    for (index, row) in rows.iter().enumerate() {
        let currency = row
            .as_object()
            .and_then(|row| row.get(field))
            .and_then(Value::as_str)
            .ok_or_else(|| format!("{section} row {} has no {field}", index + 1))?;
        if !currency.eq_ignore_ascii_case(accounting_currency) {
            return Err(format!(
                "{section} row {} uses {currency}, expected accounting currency {accounting_currency}",
                index + 1
            ));
        }
    }
    Ok(())
}

async fn target_has_pricing_state(tx: &mut Transaction<'_, Postgres>) -> Result<bool, String> {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM price_books) \
             OR EXISTS(SELECT 1 FROM price_book_versions) \
             OR EXISTS(SELECT 1 FROM channel_model_prices) \
             OR EXISTS(SELECT 1 FROM channel_model_price_versions)",
    )
    .fetch_one(&mut **tx)
    .await
    .map_err(|error| format!("failed to inspect existing pricing state: {error}"))
}

async fn restore_pricing_configuration(
    tx: &mut Transaction<'_, Postgres>,
    configuration: Option<&Value>,
    strategy: conduit_admin_graphql::backup_ext::BackupConflictStrategy,
) -> Result<(), String> {
    let Some(sections) = configuration.and_then(Value::as_object) else {
        return Ok(());
    };
    if let Some(rows) = sections
        .get("accounting_settings")
        .and_then(Value::as_array)
    {
        for row in rows {
            let object = row.as_object().ok_or_else(|| {
                "accounting_settings section contains a non-object row".to_string()
            })?;
            restore_accounting_settings_row(tx, object).await?;
        }
        reset_id_sequence(tx, "systems")
            .await
            .map_err(|error| error.to_string())?;
    }

    const ORDERED_TABLES: &[&str] = &[
        "upstream_model_deployments",
        "model_routes",
        "price_books",
        "price_book_versions",
        "price_book_items",
        "price_tiers",
        "project_commercial_profiles",
        "project_price_adjustments",
        "channel_model_price_versions",
    ];
    for table in ORDERED_TABLES {
        let Some(rows) = sections.get(*table).and_then(Value::as_array) else {
            continue;
        };
        let columns = table_columns(tx, table)
            .await
            .map_err(|error| error.to_string())?;
        for row in rows {
            let object = row
                .as_object()
                .ok_or_else(|| format!("{table} section contains a non-object row"))?;
            if *table == "project_commercial_profiles" {
                restore_row_with_identity(tx, table, &columns, object, strategy, "project_id")
                    .await?;
            } else {
                restore_row(tx, table, &columns, object, strategy).await?;
            }
        }
        reset_id_sequence(tx, table)
            .await
            .map_err(|error| error.to_string())?;
    }
    restore_provider_pricing_history(tx, sections).await?;
    Ok(())
}

struct HistoryReference<'a> {
    column: &'static str,
    ids: &'a HashMap<i64, i64>,
    required: bool,
}

async fn restore_provider_pricing_history(
    tx: &mut Transaction<'_, Postgres>,
    sections: &Map<String, Value>,
) -> Result<(), String> {
    let snapshot_ids =
        restore_history_section(tx, sections, "provider_price_snapshots", &[]).await?;
    let row_ids = restore_history_section(
        tx,
        sections,
        "provider_price_rows",
        &[HistoryReference {
            column: "snapshot_id",
            ids: &snapshot_ids,
            required: true,
        }],
    )
    .await?;
    restore_history_section(
        tx,
        sections,
        "provider_price_change_events",
        &[
            HistoryReference {
                column: "from_snapshot_id",
                ids: &snapshot_ids,
                required: false,
            },
            HistoryReference {
                column: "to_snapshot_id",
                ids: &snapshot_ids,
                required: true,
            },
        ],
    )
    .await?;
    let change_set_ids = restore_history_section_with(tx, sections, "change_sets", &[], |object| {
        remap_provider_change_set_source_revision(object, &snapshot_ids)
    })
    .await?;
    restore_history_section_with(
        tx,
        sections,
        "change_set_items",
        &[HistoryReference {
            column: "change_set_id",
            ids: &change_set_ids,
            required: true,
        }],
        |object| {
            remap_history_json_reference(
                "change_set_items",
                object,
                "source_snapshot",
                "snapshotID",
                &snapshot_ids,
            )?;
            remap_history_json_reference(
                "change_set_items",
                object,
                "source_snapshot",
                "providerPriceRowID",
                &row_ids,
            )
        },
    )
    .await?;
    restore_history_section_with(
        tx,
        sections,
        "change_set_events",
        &[HistoryReference {
            column: "change_set_id",
            ids: &change_set_ids,
            required: true,
        }],
        |object| {
            remap_history_json_reference(
                "change_set_events",
                object,
                "detail",
                "snapshotID",
                &snapshot_ids,
            )?;
            remap_history_json_reference(
                "change_set_events",
                object,
                "detail",
                "providerPriceRowID",
                &row_ids,
            )
        },
    )
    .await?;
    restore_history_section(
        tx,
        sections,
        "pricing_change_audits",
        &[
            HistoryReference {
                column: "source_snapshot_id",
                ids: &snapshot_ids,
                required: false,
            },
            HistoryReference {
                column: "source_observation_id",
                ids: &row_ids,
                required: false,
            },
            HistoryReference {
                column: "source_change_set_id",
                ids: &change_set_ids,
                required: false,
            },
        ],
    )
    .await?;
    Ok(())
}

async fn restore_history_section(
    tx: &mut Transaction<'_, Postgres>,
    sections: &Map<String, Value>,
    table: &str,
    references: &[HistoryReference<'_>],
) -> Result<HashMap<i64, i64>, String> {
    restore_history_section_with(tx, sections, table, references, |_| Ok(())).await
}

async fn restore_history_section_with<F>(
    tx: &mut Transaction<'_, Postgres>,
    sections: &Map<String, Value>,
    table: &str,
    references: &[HistoryReference<'_>],
    mut transform: F,
) -> Result<HashMap<i64, i64>, String>
where
    F: FnMut(&mut Map<String, Value>) -> Result<(), String>,
{
    let Some(rows) = sections.get(table).and_then(Value::as_array) else {
        return Ok(HashMap::new());
    };
    let columns = table_columns(tx, table)
        .await
        .map_err(|error| error.to_string())?;
    let mut restored_ids = HashMap::new();
    for row in rows {
        let object = row
            .as_object()
            .ok_or_else(|| format!("{table} section contains a non-object row"))?;
        let source_id = json_integer_key(object, "id")?
            .ok_or_else(|| format!("{table} backup row has no id"))?;
        let mut filtered = object
            .iter()
            .filter(|(name, _)| columns.contains(*name))
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect::<Map<_, _>>();
        for reference in references {
            remap_history_reference(table, &mut filtered, reference)?;
        }
        transform(&mut filtered)?;
        if table == "pricing_change_audits" {
            remap_pricing_audit_change_set_entity(&mut filtered, references)?;
        }
        let restored_id = restore_history_row(tx, table, filtered).await?;
        if restored_ids.insert(source_id, restored_id).is_some() {
            return Err(format!("{table} backup contains duplicate id {source_id}"));
        }
    }
    reset_id_sequence(tx, table)
        .await
        .map_err(|error| error.to_string())?;
    Ok(restored_ids)
}

fn remap_provider_change_set_source_revision(
    object: &mut Map<String, Value>,
    snapshot_ids: &HashMap<i64, i64>,
) -> Result<(), String> {
    if object.get("kind").and_then(Value::as_str) != Some("provider_price") {
        return Ok(());
    }
    let source_id = object
        .get("source_revision")
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<i64>().ok())
        .ok_or_else(|| "provider-price change set has an invalid source revision".to_string())?;
    let restored_id = snapshot_ids.get(&source_id).ok_or_else(|| {
        format!("provider-price change set references missing snapshot {source_id}")
    })?;
    object.insert("source_revision".into(), restored_id.to_string().into());
    Ok(())
}

fn remap_history_json_reference(
    table: &str,
    object: &mut Map<String, Value>,
    column: &str,
    key: &str,
    ids: &HashMap<i64, i64>,
) -> Result<(), String> {
    let Some(payload) = object.get_mut(column) else {
        return Ok(());
    };
    if payload.is_null() {
        return Ok(());
    }
    let payload = payload
        .as_object_mut()
        .ok_or_else(|| format!("{table} backup row has a non-object {column}"))?;
    let Some(source_id) = json_integer_key(payload, key)? else {
        return Ok(());
    };
    let restored_id = ids
        .get(&source_id)
        .ok_or_else(|| format!("{table} backup row references missing {key} {source_id}"))?;
    payload.insert(key.into(), Value::from(*restored_id));
    Ok(())
}

fn remap_pricing_audit_change_set_entity(
    object: &mut Map<String, Value>,
    references: &[HistoryReference<'_>],
) -> Result<(), String> {
    if object.get("entity_type").and_then(Value::as_str) != Some("change_set") {
        return Ok(());
    }
    let source_id = object
        .get("entity_id")
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<i64>().ok())
        .ok_or_else(|| "pricing audit change-set entity has an invalid id".to_string())?;
    let change_set_ids = references
        .iter()
        .find(|reference| reference.column == "source_change_set_id")
        .ok_or_else(|| "pricing audit change-set id mapping is unavailable".to_string())?
        .ids;
    let restored_id = change_set_ids
        .get(&source_id)
        .ok_or_else(|| format!("pricing audit references missing change set {source_id}"))?;
    object.insert("entity_id".into(), Value::from(restored_id.to_string()));
    Ok(())
}

fn remap_history_reference(
    table: &str,
    object: &mut Map<String, Value>,
    reference: &HistoryReference<'_>,
) -> Result<(), String> {
    let source_id = json_integer_key(object, reference.column)?;
    let Some(source_id) = source_id else {
        if reference.required {
            return Err(format!("{table} backup row has no {}", reference.column));
        }
        return Ok(());
    };
    let restored_id = reference.ids.get(&source_id).ok_or_else(|| {
        format!(
            "{table} backup row references missing {} {source_id}",
            reference.column
        )
    })?;
    object.insert(reference.column.into(), Value::from(*restored_id));
    Ok(())
}

async fn restore_history_row(
    tx: &mut Transaction<'_, Postgres>,
    table: &str,
    mut object: Map<String, Value>,
) -> Result<i64, String> {
    let source_id =
        json_integer_key(&object, "id")?.ok_or_else(|| format!("{table} backup row has no id"))?;
    let mut comparable = object.clone();
    comparable.remove("id");
    let table_name = quote_identifier(table);
    let identical_id = sqlx::query_scalar::<_, i64>(&format!(
        "SELECT id FROM {table_name} AS target WHERE (to_jsonb(target) - 'id') = $1 LIMIT 1"
    ))
    .bind(Json(Value::Object(comparable)))
    .fetch_optional(&mut **tx)
    .await
    .map_err(|error| error.to_string())?;
    if let Some(id) = identical_id {
        return Ok(id);
    }

    let id_exists = sqlx::query_scalar::<_, bool>(&format!(
        "SELECT EXISTS(SELECT 1 FROM {table_name} WHERE id=$1)"
    ))
    .bind(source_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(|error| error.to_string())?;
    if !id_exists {
        let columns = object.keys().cloned().collect::<HashSet<_>>();
        restore_row_with_identity(
            tx,
            table,
            &columns,
            &object,
            conduit_admin_graphql::backup_ext::BackupConflictStrategy::Error,
            "id",
        )
        .await?;
        return Ok(source_id);
    }

    object.remove("id");
    let mut columns = object.keys().cloned().collect::<Vec<_>>();
    columns.sort();
    let names = columns
        .iter()
        .map(|name| quote_identifier(name))
        .collect::<Vec<_>>()
        .join(", ");
    let source_names = columns
        .iter()
        .map(|name| format!("source.{}", quote_identifier(name)))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "INSERT INTO {table_name} ({names}) \
         SELECT {source_names} FROM jsonb_populate_record(NULL::{table_name}, $1::jsonb) AS source \
         RETURNING id"
    );
    sqlx::query_scalar::<_, i64>(&sql)
        .bind(Json(Value::Object(object)))
        .fetch_one(&mut **tx)
        .await
        .map_err(|error| error.to_string())
}

async fn restore_accounting_settings_row(
    tx: &mut Transaction<'_, Postgres>,
    object: &Map<String, Value>,
) -> Result<(), String> {
    if object.get("key").and_then(Value::as_str)
        != Some(conduit_services::system_key::GENERAL_SETTINGS)
    {
        return Err("accounting_settings section contains an unexpected system key".into());
    }
    let columns = table_columns(tx, "systems")
        .await
        .map_err(|error| error.to_string())?;
    let mut filtered = object
        .iter()
        .filter(|(name, _)| columns.contains(*name))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect::<Map<_, _>>();
    filtered.remove("id");
    let updated = sqlx::query(
        "UPDATE systems AS target SET value=source.value,updated_at=source.updated_at,deleted_at=0 \
         FROM jsonb_populate_record(NULL::systems,$1::jsonb) AS source \
         WHERE target.key=source.key",
    )
    .bind(Json(Value::Object(filtered)))
    .execute(&mut **tx)
    .await
    .map_err(|error| error.to_string())?;
    if updated.rows_affected() == 0 {
        restore_row(
            tx,
            "systems",
            &columns,
            object,
            conduit_admin_graphql::backup_ext::BackupConflictStrategy::Error,
        )
        .await?;
    }
    Ok(())
}

async fn table_columns(
    tx: &mut Transaction<'_, Postgres>,
    table: &str,
) -> Result<HashSet<String>, sqlx::Error> {
    let rows = sqlx::query_scalar::<_, String>(
        "SELECT column_name FROM information_schema.columns \
         WHERE table_schema = current_schema() AND table_name = $1",
    )
    .bind(table)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows.into_iter().collect())
}

fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

#[cfg(test)]
fn json_id(object: &Map<String, Value>) -> Result<Option<i64>, String> {
    json_integer_key(object, "id")
}

fn json_integer_key(object: &Map<String, Value>, key: &str) -> Result<Option<i64>, String> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => number
            .as_i64()
            .map(Some)
            .ok_or_else(|| "backup row id exceeds bigint range".to_string()),
        Some(Value::String(raw)) => raw
            .parse::<i64>()
            .map(Some)
            .map_err(|_| format!("invalid backup row {key} {raw:?}")),
        Some(other) => Err(format!("invalid backup row {key} {other}")),
    }
}

async fn restore_row(
    tx: &mut Transaction<'_, Postgres>,
    table: &str,
    table_columns: &HashSet<String>,
    object: &Map<String, Value>,
    strategy: conduit_admin_graphql::backup_ext::BackupConflictStrategy,
) -> Result<(), String> {
    restore_row_with_identity(tx, table, table_columns, object, strategy, "id").await
}

async fn restore_row_with_identity(
    tx: &mut Transaction<'_, Postgres>,
    table: &str,
    table_columns: &HashSet<String>,
    object: &Map<String, Value>,
    strategy: conduit_admin_graphql::backup_ext::BackupConflictStrategy,
    identity_column: &str,
) -> Result<(), String> {
    use conduit_admin_graphql::backup_ext::BackupConflictStrategy;

    let mut filtered = Map::new();
    for (name, value) in object {
        if table_columns.contains(name) {
            filtered.insert(name.clone(), value.clone());
        }
    }
    if filtered.is_empty() {
        return Ok(());
    }
    let identity = json_integer_key(&filtered, identity_column)?;
    let exists = if let Some(identity) = identity {
        sqlx::query_scalar::<_, bool>(&format!(
            "SELECT EXISTS(SELECT 1 FROM {} WHERE {} = $1)",
            quote_identifier(table),
            quote_identifier(identity_column),
        ))
        .bind(identity)
        .fetch_one(&mut **tx)
        .await
        .map_err(|error| error.to_string())?
    } else {
        false
    };

    if exists {
        match strategy {
            BackupConflictStrategy::Skip => return Ok(()),
            BackupConflictStrategy::Error => {
                return Err(format!(
                    "{table} row {} already exists",
                    identity.map_or_else(|| "null".to_string(), |value| value.to_string())
                ));
            }
            BackupConflictStrategy::Overwrite => {
                let mut update_columns = filtered
                    .keys()
                    .filter(|name| name.as_str() != identity_column)
                    .cloned()
                    .collect::<Vec<_>>();
                update_columns.sort();
                if update_columns.is_empty() {
                    return Ok(());
                }
                let assignments = update_columns
                    .iter()
                    .map(|name| {
                        let quoted = quote_identifier(name);
                        format!("{quoted} = source.{quoted}")
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                let sql = format!(
                    "UPDATE {table_name} AS target SET {assignments} \
                     FROM jsonb_populate_record(NULL::{table_name}, $1::jsonb) AS source \
                     WHERE target.{identity} = source.{identity}",
                    table_name = quote_identifier(table),
                    identity = quote_identifier(identity_column),
                );
                sqlx::query(&sql)
                    .bind(Json(Value::Object(filtered)))
                    .execute(&mut **tx)
                    .await
                    .map_err(|error| error.to_string())?;
                return Ok(());
            }
        }
    }

    let mut columns = filtered.keys().cloned().collect::<Vec<_>>();
    columns.sort();
    let names = columns
        .iter()
        .map(|name| quote_identifier(name))
        .collect::<Vec<_>>()
        .join(", ");
    let source_names = columns
        .iter()
        .map(|name| format!("source.{}", quote_identifier(name)))
        .collect::<Vec<_>>()
        .join(", ");
    let conflict = if matches!(strategy, BackupConflictStrategy::Skip) && identity.is_some() {
        format!(
            " ON CONFLICT ({}) DO NOTHING",
            quote_identifier(identity_column)
        )
    } else {
        String::new()
    };
    let table_name = quote_identifier(table);
    let sql = format!(
        "INSERT INTO {table_name} ({names}) \
         SELECT {source_names} FROM jsonb_populate_record(NULL::{table_name}, $1::jsonb) AS source{conflict}"
    );
    sqlx::query(&sql)
        .bind(Json(Value::Object(filtered)))
        .execute(&mut **tx)
        .await
        .map_err(|error| error.to_string())?;
    Ok(())
}

async fn reset_id_sequence(
    tx: &mut Transaction<'_, Postgres>,
    table: &str,
) -> Result<(), sqlx::Error> {
    let has_id_column = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM information_schema.columns \
         WHERE table_schema=current_schema() AND table_name=$1 AND column_name='id')",
    )
    .bind(table)
    .fetch_one(&mut **tx)
    .await?;
    if !has_id_column {
        return Ok(());
    }
    let sequence =
        sqlx::query_scalar::<_, Option<String>>("SELECT pg_get_serial_sequence($1, 'id')")
            .bind(table)
            .fetch_one(&mut **tx)
            .await?;
    let Some(sequence) = sequence else {
        return Ok(());
    };
    let table_name = quote_identifier(table);
    let sql = format!(
        "SELECT setval($1::regclass, \
         GREATEST(COALESCE(MAX(id), 1), 1), COUNT(*) > 0) FROM {table_name}"
    );
    sqlx::query(&sql).bind(sequence).execute(&mut **tx).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_admin_graphql::backup_ext::{
        BackupConflictStrategy, BackupExtServices, BackupOptionsInput, RestoreOptionsInput,
    };
    use conduit_cache::NoopCache;

    const TEST_BACKUP_ENCRYPTION_KEY: [u8; 32] = [0x42; 32];

    #[test]
    fn scheduled_backup_requires_the_02utc_slot_and_deduplicates_the_day()
    -> Result<(), Box<dyn std::error::Error>> {
        let before_slot = DateTime::parse_from_rfc3339("2024-01-07T01:30:00Z")?.with_timezone(&Utc);
        let slot = DateTime::parse_from_rfc3339("2024-01-07T02:30:00Z")?.with_timezone(&Utc);
        let mut settings = conduit_services::AutoBackupSettings {
            enabled: true,
            frequency: conduit_services::BackupFrequency::Weekly,
            ..conduit_services::AutoBackupSettings::default()
        };

        assert!(!scheduled_auto_backup_due(&settings, before_slot));
        assert!(scheduled_auto_backup_due(&settings, slot));
        settings.last_backup_at = Some(slot);
        assert!(!scheduled_auto_backup_due(&settings, slot));
        Ok(())
    }

    #[test]
    fn identifier_quoting_is_total() {
        assert_eq!(quote_identifier("models"), "\"models\"");
        assert_eq!(quote_identifier("a\"b"), "\"a\"\"b\"");
    }

    #[test]
    fn json_id_accepts_backup_number_and_string_forms() -> Result<(), String> {
        let mut object = Map::new();
        object.insert("id".to_string(), Value::from(7));
        assert_eq!(json_id(&object)?, Some(7));
        object.insert("id".to_string(), Value::from("8"));
        assert_eq!(json_id(&object)?, Some(8));
        Ok(())
    }

    #[tokio::test]
    async fn pricing_history_restore_remaps_colliding_parent_and_child_ids_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        sqlx::query(
            "INSERT INTO provider_price_snapshots \
             (id,channel_id,adapter_id,adapter_version,attempted_endpoints,status,warnings,started_at,observed_at) \
             VALUES(900,1,'target','1','[]'::jsonb,'success','[]'::jsonb,now(),now())",
        )
        .execute(&database.pool)
        .await?;
        sqlx::query(
            "INSERT INTO provider_price_rows \
             (id,snapshot_id,channel_id,upstream_model_id,group_name,billing_kind,quality) \
             VALUES(901,900,1,'target-model','','tokens','exact')",
        )
        .execute(&database.pool)
        .await?;
        sqlx::query(
            "INSERT INTO change_sets \
             (id,kind,scope_type,scope_id,title,status,source_revision,submitted_at,created_at,updated_at) \
             VALUES(903,'provider_price','channel','1','target','pending_review','900',now(),now(),now())",
        )
        .execute(&database.pool)
        .await?;
        sqlx::query(
            "INSERT INTO change_set_items \
             (id,change_set_id,item_key,action,after_snapshot,source_snapshot,created_at,updated_at) \
             VALUES(905,903,'target-model','create','{\"items\":[]}'::jsonb,\
                    '{\"snapshotID\":900,\"providerPriceRowID\":901}'::jsonb,now(),now())",
        )
        .execute(&database.pool)
        .await?;
        sqlx::query(
            "INSERT INTO change_set_events(id,change_set_id,event_type,actor_type,detail,created_at) \
             VALUES(906,903,'submitted','system','{}'::jsonb,now())",
        )
        .execute(&database.pool)
        .await?;
        sqlx::query(
            "INSERT INTO pricing_change_audits \
             (id,actor_type,operation,entity_type,entity_id,source_snapshot_id,\
              source_observation_id,source_change_set_id,accounting_currency,\
              accounting_settings_version,result,request_correlation_id,created_at) \
             VALUES(904,'system','target','change_set','903',900,901,903,\
                    'CNY',1,'success','target-audit',now())",
        )
        .execute(&database.pool)
        .await?;

        let observed_at = chrono::Utc::now().to_rfc3339();
        let configuration = serde_json::json!({
            "provider_price_snapshots": [{
                "id": 900,
                "channel_id": 2,
                "adapter_id": "source",
                "adapter_version": "1",
                "attempted_endpoints": [],
                "status": "success",
                "warnings": [],
                "started_at": observed_at,
                "observed_at": observed_at
            }],
            "provider_price_rows": [{
                "id": 901,
                "snapshot_id": 900,
                "channel_id": 2,
                "upstream_model_id": "source-model",
                "group_name": "",
                "billing_kind": "tokens",
                "quality": "exact",
                "source_unit": "CHANNEL_BALANCE_UNIT"
            }],
            "provider_price_change_events": [{
                "id": 902,
                "channel_id": 2,
                "from_snapshot_id": null,
                "to_snapshot_id": 900,
                "upstream_model_id": "source-model",
                "group_name": "",
                "billing_kind": "tokens",
                "event_type": "added",
                "created_at": observed_at
            }],
            "change_sets": [{
                "id": 903,
                "kind": "provider_price",
                "scope_type": "channel",
                "scope_id": "2",
                "title": "source",
                "status": "pending_review",
                "base_revision": "",
                "source_revision": "900",
                "validation_error": null,
                "submitted_at": observed_at,
                "created_at": observed_at,
                "updated_at": observed_at
            }],
            "change_set_items": [{
                "id": 905,
                "change_set_id": 903,
                "item_key": "source-model",
                "action": "create",
                "before_snapshot": null,
                "after_snapshot": {"items": []},
                "source_snapshot": {"snapshotID": 900, "providerPriceRowID": 901},
                "created_at": observed_at,
                "updated_at": observed_at
            }],
            "change_set_events": [{
                "id": 906,
                "change_set_id": 903,
                "event_type": "submitted",
                "actor_type": "system",
                "detail": {"snapshotID": 900, "providerPriceRowID": 901},
                "created_at": observed_at
            }],
            "pricing_change_audits": [{
                "id": 904,
                "actor_type": "system",
                "operation": "restore-source",
                "entity_type": "change_set",
                "entity_id": "903",
                "source_snapshot_id": 900,
                "source_observation_id": 901,
                "source_change_set_id": 903,
                "accounting_currency": "CNY",
                "accounting_settings_version": 1,
                "result": "success",
                "request_correlation_id": "source-audit",
                "created_at": observed_at
            }]
        });
        let mut tx = database.pool.begin().await?;
        restore_pricing_configuration(&mut tx, Some(&configuration), BackupConflictStrategy::Skip)
            .await?;
        tx.commit().await?;

        let imported_snapshot_id = sqlx::query_scalar::<_, i64>(
            "SELECT id FROM provider_price_snapshots WHERE channel_id=2",
        )
        .fetch_one(&database.pool)
        .await?;
        let (imported_row_id, row_snapshot_id) = sqlx::query_as::<_, (i64, i64)>(
            "SELECT id,snapshot_id FROM provider_price_rows WHERE channel_id=2",
        )
        .fetch_one(&database.pool)
        .await?;
        let (imported_change_set_id, imported_source_revision) =
            sqlx::query_as::<_, (i64, String)>(
                "SELECT id,source_revision FROM change_sets \
                 WHERE kind='provider_price' AND scope_id='2'",
            )
            .fetch_one(&database.pool)
            .await?;
        let (item_change_set_id, item_snapshot_id, item_row_id) =
            sqlx::query_as::<_, (i64, i64, i64)>(
                "SELECT change_set_id,CAST(source_snapshot->>'snapshotID' AS BIGINT),\
                        CAST(source_snapshot->>'providerPriceRowID' AS BIGINT) \
                 FROM change_set_items WHERE item_key='source-model'",
            )
            .fetch_one(&database.pool)
            .await?;
        let (event_change_set_id, event_snapshot_id, event_row_id) =
            sqlx::query_as::<_, (i64, i64, i64)>(
                "SELECT change_set_id,CAST(detail->>'snapshotID' AS BIGINT),\
                        CAST(detail->>'providerPriceRowID' AS BIGINT) \
                 FROM change_set_events WHERE event_type='submitted' AND change_set_id=$1",
            )
            .bind(imported_change_set_id)
            .fetch_one(&database.pool)
            .await?;
        let audit_sources = sqlx::query_as::<_, (String, Option<i64>, Option<i64>, Option<i64>)>(
            "SELECT entity_id,source_snapshot_id,source_observation_id,source_change_set_id \
             FROM pricing_change_audits WHERE request_correlation_id='source-audit'",
        )
        .fetch_one(&database.pool)
        .await?;

        assert_ne!(imported_snapshot_id, 900);
        assert_ne!(imported_row_id, 901);
        assert_ne!(imported_change_set_id, 903);
        assert_eq!(row_snapshot_id, imported_snapshot_id);
        assert_eq!(imported_source_revision, imported_snapshot_id.to_string());
        assert_eq!(item_change_set_id, imported_change_set_id);
        assert_eq!(item_snapshot_id, imported_snapshot_id);
        assert_eq!(item_row_id, imported_row_id);
        assert_eq!(event_change_set_id, imported_change_set_id);
        assert_eq!(event_snapshot_id, imported_snapshot_id);
        assert_eq!(event_row_id, imported_row_id);
        assert_eq!(
            audit_sources,
            (
                imported_change_set_id.to_string(),
                Some(imported_snapshot_id),
                Some(imported_row_id),
                Some(imported_change_set_id)
            )
        );

        database.cleanup().await?;
        Ok(())
    }

    #[test]
    fn pricing_restore_preflight_rejects_mixed_archive_currencies() -> Result<(), String> {
        let configuration = serde_json::json!({
            "accounting_settings": [{
                "key": conduit_services::system_key::GENERAL_SETTINGS,
                "value": "{\"accounting_currency_code\":\"CNY\"}"
            }],
            "channel_model_price_versions": [{"currency_code": "CNY"}],
            "price_books": [{"currency": "USD"}]
        });
        let prices = serde_json::json!([{"currency_code": "CNY"}]);

        assert_eq!(
            archived_accounting_currency(Some(&configuration))?.as_deref(),
            Some("CNY")
        );
        let error = validate_archived_price_currencies(Some(&prices), Some(&configuration), "CNY")
            .unwrap_err();
        assert!(error.contains("price_books row 1 uses USD"));
        Ok(())
    }

    #[test]
    fn legacy_accounting_settings_default_to_cny_during_restore() -> Result<(), String> {
        assert_eq!(parse_accounting_currency_value("{}")?, "CNY");
        Ok(())
    }

    #[test]
    fn pricing_configuration_backup_covers_every_price_state_table() {
        assert_eq!(
            PRICING_CONFIGURATION_TABLES,
            &[
                "upstream_model_deployments",
                "model_routes",
                "channel_model_price_versions",
                "price_books",
                "price_book_versions",
                "price_book_items",
                "price_tiers",
                "project_commercial_profiles",
                "project_price_adjustments",
                "provider_price_snapshots",
                "provider_price_rows",
                "provider_price_change_events",
                "change_sets",
                "change_set_items",
                "change_set_events",
                "pricing_change_audits",
            ]
        );
    }

    #[tokio::test]
    async fn postgres_backup_dump_and_restore_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        sqlx::query(
            "INSERT INTO channels \
             (id, \"type\", name, status, credentials, default_test_model, settings) \
             VALUES (70, 'openai', 'backup-source', 'enabled', \
                     '{\"api_keys\":[\"secret\"]}'::jsonb, 'gpt-test', \
                     '{\"nested\":true}'::jsonb)",
        )
        .execute(&database.pool)
        .await?;
        let system = Arc::new(conduit_services::SystemService::from_system_repo(
            Arc::new(conduit_db::PgSystemRepo::new(database.pool.clone())),
            Arc::new(NoopCache::new()),
        ));
        let adapter = PgBackupExtAdapter::new(
            database.pool.clone(),
            system,
            Arc::new(conduit_db::PgDataStorageRepo::new(database.pool.clone())),
        )
        .with_test_backup_encryption_key(TEST_BACKUP_ENCRYPTION_KEY);
        let encoded = adapter
            .run_backup(BackupOptionsInput {
                include_channels: true,
                include_model_prices: false,
                include_models: false,
                include_api_keys: false,
                include_usage_stats: false,
                include_request_logs: false,
            })
            .await?;
        let bytes = base64::engine::general_purpose::STANDARD.decode(encoded)?;
        assert!(!String::from_utf8_lossy(&bytes).contains("secret"));
        let plaintext = conduit_services::backup_archive_crypto::decrypt_if_enveloped_with_key(
            &bytes,
            &TEST_BACKUP_ENCRYPTION_KEY,
        )?;
        let manifest = conduit_services::parse_backup_manifest(&plaintext)?;
        let channels = manifest
            .entities
            .channels
            .as_ref()
            .and_then(Value::as_array)
            .ok_or("channels missing from backup")?;
        let source = channels
            .iter()
            .find(|row| row["id"] == 70)
            .ok_or("source channel missing from backup")?;
        assert_eq!(source["settings"]["nested"], true);
        assert_eq!(source["credentials"]["api_keys"][0], "secret");

        sqlx::query("DELETE FROM channels WHERE id = 70")
            .execute(&database.pool)
            .await?;
        adapter
            .restore(
                bytes,
                RestoreOptionsInput {
                    include_channels: true,
                    include_model_prices: false,
                    include_models: false,
                    include_api_keys: false,
                    include_usage_stats: false,
                    include_request_logs: false,
                    channel_conflict_strategy: BackupConflictStrategy::Error,
                    model_conflict_strategy: BackupConflictStrategy::Skip,
                    model_price_conflict_strategy: BackupConflictStrategy::Skip,
                    api_key_conflict_strategy: BackupConflictStrategy::Skip,
                },
            )
            .await?;
        let restored_secret: Json<Value> =
            sqlx::query_scalar("SELECT credentials FROM channels WHERE id = 70")
                .fetch_one(&database.pool)
                .await?;
        assert_eq!(restored_secret.0["api_keys"][0], "secret");

        let archive = serde_json::json!({
            "version": conduit_services::BACKUP_VERSION,
            "timestamp": "2026-08-15T00:00:00Z",
            "channels": [{
                "id": 71,
                "type": "openai",
                "name": "backup-restored",
                "status": "enabled",
                "credentials": {"api_keys": []},
                "default_test_model": "gpt-restored",
                "settings": {"restored": true}
            }]
        });
        restore_archive(
            &database.pool,
            &serde_json::to_vec(&archive)?,
            RestoreOptionsInput {
                include_channels: true,
                include_model_prices: false,
                include_models: false,
                include_api_keys: false,
                include_usage_stats: false,
                include_request_logs: false,
                channel_conflict_strategy: BackupConflictStrategy::Error,
                model_conflict_strategy: BackupConflictStrategy::Skip,
                model_price_conflict_strategy: BackupConflictStrategy::Skip,
                api_key_conflict_strategy: BackupConflictStrategy::Skip,
            },
        )
        .await?;
        let restored = sqlx::query("SELECT name, settings FROM channels WHERE id = 71")
            .fetch_one(&database.pool)
            .await?;
        assert_eq!(restored.get::<String, _>("name"), "backup-restored");
        assert_eq!(
            restored.get::<Json<Value>, _>("settings").0["restored"],
            true
        );

        let invalid_archive = serde_json::json!({
            "version": conduit_services::BACKUP_VERSION,
            "timestamp": "2026-08-15T00:00:00Z",
            "channels": [
                {
                    "id": 72,
                    "type": "openai",
                    "name": "must-roll-back",
                    "status": "enabled",
                    "credentials": {"api_keys": []},
                    "default_test_model": "gpt-test",
                    "settings": {}
                },
                {"id": 73, "type": "openai"}
            ]
        });
        assert!(
            restore_archive(
                &database.pool,
                &serde_json::to_vec(&invalid_archive)?,
                RestoreOptionsInput {
                    include_channels: true,
                    include_model_prices: false,
                    include_models: false,
                    include_api_keys: false,
                    include_usage_stats: false,
                    include_request_logs: false,
                    channel_conflict_strategy: BackupConflictStrategy::Error,
                    model_conflict_strategy: BackupConflictStrategy::Skip,
                    model_price_conflict_strategy: BackupConflictStrategy::Skip,
                    api_key_conflict_strategy: BackupConflictStrategy::Skip,
                },
            )
            .await
            .is_err()
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM channels WHERE id IN (72, 73)")
                .fetch_one(&database.pool)
                .await?,
            0
        );
        database.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn pricing_configuration_round_trips_between_isolated_databases_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let source = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        sqlx::query(
            "INSERT INTO systems(key,value,created_at,updated_at) VALUES \
             ($1,$2,now(),now()) ON CONFLICT(key) DO UPDATE SET value=EXCLUDED.value, \
             updated_at=EXCLUDED.updated_at,deleted_at=0",
        )
        .bind(conduit_services::system_key::GENERAL_SETTINGS)
        .bind(
            r#"{"accounting_currency_code":"CNY","credit_display_name":"积分","credits_per_accounting_unit":"10000","exchange_rates":[],"accounting_rate_version":7,"timezone":"Asia/Shanghai"}"#,
        )
        .execute(&source.pool)
        .await?;
        sqlx::query(
            "INSERT INTO upstream_model_deployments \
             (id,channel_id,upstream_model_id,internal_name,variant,status,source) \
             VALUES(650,70,'backup-upstream-model','backup deployment','','enabled','discovered')",
        )
        .execute(&source.pool)
        .await?;
        sqlx::query(
            "INSERT INTO model_routes(id,public_model_id,deployment_id,status) \
             VALUES(651,60,650,'enabled')",
        )
        .execute(&source.pool)
        .await?;
        sqlx::query(
            "INSERT INTO channel_model_prices \
             (id,channel_id,model_id,currency_code,price,reference_id) VALUES \
             (700,70,'backup-priced-model','CNY','{\"items\":[]}'::jsonb,'backup-price-head')",
        )
        .execute(&source.pool)
        .await?;
        sqlx::query(
            "INSERT INTO channel_model_price_versions \
             (id,channel_id,model_id,channel_model_price_id,currency_code,price,status, \
              effective_start_at,reference_id) VALUES \
             (701,70,'backup-priced-model',700,'CNY','{\"items\":[]}'::jsonb, \
              'active',now(),'backup-price-version')",
        )
        .execute(&source.pool)
        .await?;
        sqlx::query(
            "INSERT INTO price_books(id,name,currency,status,is_default) \
             VALUES(800,'backup-retail','CNY','enabled',FALSE)",
        )
        .execute(&source.pool)
        .await?;
        sqlx::query(
            "INSERT INTO price_book_versions(id,price_book_id,version,status,reference_id) \
             VALUES(801,800,1,'published','backup-retail-v1')",
        )
        .execute(&source.pool)
        .await?;
        sqlx::query(
            "INSERT INTO project_commercial_profiles \
             (project_id,account_type,billing_currency,status,created_at,updated_at) \
             VALUES(900,'personal','STATION_CREDIT','active',now(),now())",
        )
        .execute(&source.pool)
        .await?;
        sqlx::query(
            "INSERT INTO pricing_change_audits \
             (id,actor_type,actor_id,operation,entity_type,entity_id,before_snapshot,after_snapshot, \
              accounting_currency,accounting_settings_version,result,request_correlation_id,created_at) \
             VALUES(802,'user',42,'create_price_book','price_book','800',NULL, \
                    '{\"id\":800}'::jsonb,'CNY',7,'success','backup-audit',now())",
        )
        .execute(&source.pool)
        .await?;

        let source_system = Arc::new(conduit_services::SystemService::from_system_repo(
            Arc::new(conduit_db::PgSystemRepo::new(source.pool.clone())),
            Arc::new(NoopCache::new()),
        ));
        let source_adapter = PgBackupExtAdapter::new(
            source.pool.clone(),
            source_system,
            Arc::new(conduit_db::PgDataStorageRepo::new(source.pool.clone())),
        )
        .with_test_backup_encryption_key(TEST_BACKUP_ENCRYPTION_KEY);
        let encoded = source_adapter
            .run_backup(BackupOptionsInput {
                include_channels: false,
                include_model_prices: true,
                include_models: false,
                include_api_keys: false,
                include_usage_stats: false,
                include_request_logs: false,
            })
            .await?;
        let encrypted = base64::engine::general_purpose::STANDARD.decode(encoded)?;
        let plaintext = conduit_services::backup_archive_crypto::decrypt_if_enveloped_with_key(
            &encrypted,
            &TEST_BACKUP_ENCRYPTION_KEY,
        )?;
        let manifest = conduit_services::parse_backup_manifest(&plaintext)?;
        assert_eq!(manifest.version, conduit_services::BACKUP_VERSION);
        let pricing = manifest
            .entities
            .pricing_configuration
            .as_ref()
            .and_then(Value::as_object)
            .ok_or("pricing_configuration missing")?;
        assert!(pricing.contains_key("accounting_settings"));
        for table in PRICING_CONFIGURATION_TABLES {
            assert!(pricing.contains_key(*table), "missing backup table {table}");
        }
        let deployment_rows = pricing["upstream_model_deployments"]
            .as_array()
            .ok_or("upstream_model_deployments backup is not an array")?;
        assert!(
            deployment_rows.iter().all(Value::is_object),
            "deployment backup rows must be objects: {deployment_rows:?}"
        );

        let target = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let target_system = Arc::new(conduit_services::SystemService::from_system_repo(
            Arc::new(conduit_db::PgSystemRepo::new(target.pool.clone())),
            Arc::new(NoopCache::new()),
        ));
        let target_adapter = PgBackupExtAdapter::new(
            target.pool.clone(),
            target_system,
            Arc::new(conduit_db::PgDataStorageRepo::new(target.pool.clone())),
        )
        .with_test_backup_encryption_key(TEST_BACKUP_ENCRYPTION_KEY);
        target_adapter
            .restore(
                encrypted,
                RestoreOptionsInput {
                    include_channels: false,
                    include_model_prices: true,
                    include_models: false,
                    include_api_keys: false,
                    include_usage_stats: false,
                    include_request_logs: false,
                    channel_conflict_strategy: BackupConflictStrategy::Skip,
                    model_conflict_strategy: BackupConflictStrategy::Skip,
                    model_price_conflict_strategy: BackupConflictStrategy::Error,
                    api_key_conflict_strategy: BackupConflictStrategy::Skip,
                },
            )
            .await?;
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT currency_code FROM channel_model_prices WHERE id=700",
            )
            .fetch_one(&target.pool)
            .await?,
            "CNY"
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT upstream_model_id FROM upstream_model_deployments WHERE id=650",
            )
            .fetch_one(&target.pool)
            .await?,
            "backup-upstream-model"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT deployment_id FROM model_routes WHERE id=651",)
                .fetch_one(&target.pool)
                .await?,
            650
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT currency FROM price_books WHERE id=800")
                .fetch_one(&target.pool)
                .await?,
            "CNY"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT accounting_settings_version FROM pricing_change_audits WHERE id=802",
            )
            .fetch_one(&target.pool)
            .await?,
            7
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT billing_currency FROM project_commercial_profiles WHERE project_id=900",
            )
            .fetch_one(&target.pool)
            .await?,
            "STATION_CREDIT"
        );
        let restored_settings: String =
            sqlx::query_scalar("SELECT value FROM systems WHERE key=$1 AND deleted_at=0")
                .bind(conduit_services::system_key::GENERAL_SETTINGS)
                .fetch_one(&target.pool)
                .await?;
        assert_eq!(
            serde_json::from_str::<Value>(&restored_settings)?["accounting_rate_version"],
            7
        );
        assert!(
            sqlx::query("UPDATE pricing_change_audits SET result='tampered' WHERE id=802")
                .execute(&target.pool)
                .await
                .is_err(),
            "restored pricing audit must remain append-only"
        );
        let incompatible_configuration = serde_json::json!({
            "accounting_settings": [{
                "key": conduit_services::system_key::GENERAL_SETTINGS,
                "value": "{\"accounting_currency_code\":\"USD\"}"
            }],
            "channel_model_price_versions": [],
            "price_books": []
        });
        let no_channel_prices = serde_json::json!([]);
        let mut preflight_tx = target.pool.begin().await?;
        let error = validate_pricing_restore_currency(
            &mut preflight_tx,
            Some(&no_channel_prices),
            Some(&incompatible_configuration),
        )
        .await
        .unwrap_err();
        assert!(error.contains("cannot restore accounting currency USD"));
        preflight_tx.rollback().await?;

        source.cleanup().await?;
        target.cleanup().await?;
        Ok(())
    }
}
