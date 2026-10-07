//! Bounded local durable metering input, handed to PostgreSQL exactly once.
use std::collections::BTreeMap;
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};

use conduit_config::model::UsageRecoveryConfig;
use conduit_db::repo::usage_repo::{CreateUsageLogInput, row_from_input};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeteredEvent {
    pub version: u8,
    pub event_key: String,
    pub reservation_key: Option<String>,
    // This typed input contains only identity, time, structured usage and cost audit.
    pub usage: CreateUsageLogInput,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
enum JournalRecord {
    Ready { event: MeteredEvent },
    Ack { key: String },
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JournalFrame {
    payload: JournalRecord,
    checksum: String,
}

struct JournalState {
    reserved: HashSet<String>,
}
pub struct UsageJournal {
    config: UsageRecoveryConfig,
    directory: PathBuf,
    _lock: File,
    state: Mutex<JournalState>,
    healthy: AtomicBool,
}

pub fn event_key(request_key: &str) -> String {
    format!(
        "{:x}",
        Sha256::digest(format!("usage:v1:api:{request_key}"))
    )
}

impl UsageJournal {
    pub fn open(config: UsageRecoveryConfig) -> Result<Self, String> {
        fs::create_dir_all(&config.directory).map_err(|e| e.to_string())?;
        let directory = fs::canonicalize(&config.directory).map_err(|e| e.to_string())?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(directory.join("journal.lock"))
            .map_err(|e| e.to_string())?;
        lock.try_lock().map_err(|_| {
            "usage journal directory is already owned by another process".to_string()
        })?;
        let journal = Self {
            config,
            directory,
            _lock: lock,
            state: Mutex::new(JournalState {
                reserved: HashSet::new(),
            }),
            healthy: AtomicBool::new(true),
        };
        // The fsynced append log is authoritative; ready files are only replay indexes.
        // A rename's directory durability therefore never decides metering acceptance.
        journal.restore_indexes()?;
        Ok(journal)
    }

    fn append(&self, payload: JournalRecord) -> Result<(), String> {
        let checksum = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&payload).map_err(|e| e.to_string())?)
        );
        let mut bytes =
            serde_json::to_vec(&JournalFrame { payload, checksum }).map_err(|e| e.to_string())?;
        bytes.push(b'\n');
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.directory.join("journal.wal"))
            .map_err(|e| e.to_string())?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())
    }

    fn restore_indexes(&self) -> Result<(), String> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.directory.join("journal.wal"))
            .map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        // Persist the WAL's directory entry before any event can be accepted.
        // Ready-file renames are only indexes and are rebuilt from this log.
        #[cfg(unix)]
        {
            File::open(&self.directory)
                .and_then(|directory| directory.sync_all())
                .map_err(|e| e.to_string())?;
            if let Some(parent) = self.directory.parent() {
                File::open(parent)
                    .and_then(|directory| directory.sync_all())
                    .map_err(|e| e.to_string())?;
            }
        }
        let mut pending = BTreeMap::new();
        let mut reader = BufReader::new(file);
        loop {
            let mut line = Vec::new();
            let length = reader
                .by_ref()
                .take(self.config.max_event_bytes + 1024)
                .read_until(b'\n', &mut line)
                .map_err(|e| e.to_string())?;
            if length == 0 {
                break;
            }
            let parsed = serde_json::from_slice::<JournalFrame>(&line);
            let valid = line.last() == Some(&b'\n')
                && parsed.as_ref().is_ok_and(|frame| {
                    serde_json::to_vec(&frame.payload)
                        .is_ok_and(|bytes| frame.checksum == format!("{:x}", Sha256::digest(bytes)))
                });
            if !valid {
                self.healthy.store(false, Ordering::Release);
                return Ok(());
            }
            match parsed.map_err(|e| e.to_string())?.payload {
                JournalRecord::Ready { event } => {
                    if event.version != 1 || !valid_key(&event.event_key) {
                        self.healthy.store(false, Ordering::Release);
                        return Ok(());
                    }
                    pending.insert(event.event_key.clone(), event);
                }
                JournalRecord::Ack { key } => {
                    pending.remove(&key);
                }
            }
        }
        for entry in fs::read_dir(&self.directory).map_err(|e| e.to_string())? {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.extension().is_some_and(|v| v == "quarantine") {
                self.healthy.store(false, Ordering::Release);
            }
            if path.extension().is_some_and(|v| v == "tmp") {
                fs::remove_file(&path).map_err(|e| e.to_string())?;
            }
            if path.extension().is_some_and(|v| v == "ready")
                && !pending.contains_key(path.file_stem().and_then(|v| v.to_str()).unwrap_or(""))
            {
                fs::remove_file(path).map_err(|e| e.to_string())?;
            }
        }
        for (key, event) in pending {
            let bytes = serde_json::to_vec(&event).map_err(|e| e.to_string())?;
            if bytes.len() as u64 > self.config.max_event_bytes {
                self.healthy.store(false, Ordering::Release);
                return Ok(());
            }
            let path = self.directory.join(format!("{key}.ready"));
            let mut file = File::create(path).map_err(|e| e.to_string())?;
            file.write_all(&bytes)
                .and_then(|_| file.sync_all())
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    fn disk_bytes(&self) -> Result<u64, String> {
        let mut total = 0_u64;
        for entry in fs::read_dir(&self.directory).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            if entry
                .path()
                .extension()
                .is_some_and(|v| v == "ready" || v == "quarantine" || v == "tmp" || v == "wal")
            {
                total = total.saturating_add(entry.metadata().map_err(|e| e.to_string())?.len());
            }
        }
        Ok(total)
    }

    pub fn reserve(&self, request_key: &str) -> Result<(), String> {
        if !self.healthy.load(Ordering::Acquire) {
            return Err("usage journal is unhealthy; admission stopped".into());
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| "usage journal mutex poisoned")?;
        let key = event_key(request_key);
        if state.reserved.contains(&key) {
            return Ok(());
        }
        if state.reserved.is_empty()
            && !fs::read_dir(&self.directory)
                .map_err(|e| e.to_string())?
                .filter_map(Result::ok)
                .any(|entry| {
                    entry
                        .path()
                        .extension()
                        .is_some_and(|v| v == "ready" || v == "tmp" || v == "quarantine")
                })
        {
            let wal = OpenOptions::new()
                .write(true)
                .open(self.directory.join("journal.wal"))
                .map_err(|e| e.to_string())?;
            wal.set_len(0)
                .and_then(|_| wal.sync_all())
                .map_err(|e| e.to_string())?;
        }
        let required = self.disk_bytes()?.saturating_add(
            (state.reserved.len() as u64 + 1).saturating_mul(
                self.config
                    .max_event_bytes
                    .saturating_mul(2)
                    .saturating_add(1024),
            ),
        );
        if required > self.config.max_bytes {
            return Err("usage journal capacity exhausted; admission stopped".into());
        }
        state.reserved.insert(key);
        Ok(())
    }

    pub fn release(&self, request_key: &str) {
        if let Ok(mut state) = self.state.lock() {
            state.reserved.remove(&event_key(request_key));
        }
    }

    pub fn put_ready(&self, event: &MeteredEvent) -> Result<(), String> {
        let result = self.write_event(event);
        if result.is_err() {
            self.healthy.store(false, Ordering::Release);
        }
        result
    }

    fn write_event(&self, event: &MeteredEvent) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "usage journal mutex poisoned")?;
        if !valid_key(&event.event_key) || event.version != 1 {
            return Err("invalid metering event identity".into());
        }
        let bytes = serde_json::to_vec(event).map_err(|e| e.to_string())?;
        if bytes.len() as u64 > self.config.max_event_bytes {
            return Err("metering event exceeds reserved budget".into());
        }
        let path = self.directory.join(format!("{}.ready", event.event_key));
        if path.exists() {
            if fs::read(&path).map_err(|e| e.to_string())? != bytes {
                return Err("metering event key/payload conflict".into());
            }
            state.reserved.remove(&event.event_key);
            return Ok(());
        }
        if !state.reserved.contains(&event.event_key) {
            return Err("metering event has no admission reservation".into());
        }
        self.append(JournalRecord::Ready {
            event: event.clone(),
        })?;
        let temporary = path.with_extension("tmp");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|e| e.to_string())?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
        drop(file);
        fs::rename(&temporary, &path).map_err(|e| e.to_string())?;
        state.reserved.remove(&event.event_key);
        Ok(())
    }

    fn read_event(&self, path: &Path) -> Result<MeteredEvent, String> {
        let length = path.metadata().map_err(|e| e.to_string())?.len();
        if length > self.config.max_event_bytes {
            return Err("oversized journal file".into());
        }
        let event: MeteredEvent =
            serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        if event.version != 1
            || !valid_key(&event.event_key)
            || path.file_stem().and_then(|v| v.to_str()) != Some(&event.event_key)
        {
            return Err("journal identity mismatch".into());
        }
        Ok(event)
    }

    pub fn ack(&self, key: &str) -> Result<(), String> {
        if !valid_key(key) {
            return Err("invalid journal acknowledgement".into());
        }
        let _state = self
            .state
            .lock()
            .map_err(|_| "usage journal mutex poisoned")?;
        if !self.directory.join(format!("{key}.ready")).exists() {
            return Ok(());
        }
        self.append(JournalRecord::Ack {
            key: key.to_string(),
        })?;
        match fs::remove_file(self.directory.join(format!("{key}.ready"))) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }

    pub async fn replay(&self, pool: &PgPool) -> Result<usize, String> {
        let mut paths: Vec<_> = fs::read_dir(&self.directory)
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|v| v == "ready"))
            .collect();
        paths.sort();
        let mut done = 0;
        for path in paths.into_iter().take(self.config.replay_batch_size) {
            if !path.exists() {
                continue;
            }
            let event = match self.read_event(&path) {
                Ok(event) => event,
                Err(error) => {
                    if !path.exists() {
                        continue;
                    }
                    fs::rename(&path, path.with_extension("quarantine"))
                        .map_err(|e| e.to_string())?;
                    self.healthy.store(false, Ordering::Release);
                    return Err(error);
                }
            };
            if let Err(error) = handoff(pool, &event).await {
                if error.contains("payload conflict") || error.contains("manual reconciliation") {
                    fs::rename(&path, path.with_extension("quarantine"))
                        .map_err(|e| e.to_string())?;
                    self.healthy.store(false, Ordering::Release);
                }
                return Err(error);
            }
            self.ack(&event.event_key)?;
            done += 1;
        }
        Ok(done)
    }
}

fn valid_key(key: &str) -> bool {
    key.len() == 64 && key.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub async fn handoff(
    pool: &PgPool,
    event: &MeteredEvent,
) -> Result<Option<conduit_db::row::UsageLogRow>, String> {
    let bytes = serde_json::to_vec(event).map_err(|e| e.to_string())?;
    let hash = format!("{:x}", Sha256::digest(bytes));
    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
    sqlx::query("INSERT INTO usage_recovery_receipts(event_key,payload_hash) VALUES($1,$2) ON CONFLICT DO NOTHING")
        .bind(&event.event_key).bind(&hash).execute(&mut *tx).await.map_err(|e| e.to_string())?;
    let receipt = sqlx::query("SELECT payload_hash,usage_log_id FROM usage_recovery_receipts WHERE event_key=$1 FOR UPDATE")
        .bind(&event.event_key).fetch_one(&mut *tx).await.map_err(|e| e.to_string())?;
    if receipt.get::<String, _>("payload_hash") != hash {
        return Err("usage receipt payload conflict".into());
    }
    if let Some(id) = receipt.get::<Option<i64>, _>("usage_log_id") {
        let usage_exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM usage_logs WHERE id=$1)")
                .bind(id)
                .fetch_one(&mut *tx)
                .await
                .map_err(|e| e.to_string())?;
        if !usage_exists {
            let settled: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM usage_charge_outbox WHERE usage_log_id=$1 AND status='completed')")
                .bind(id).fetch_one(&mut *tx).await.map_err(|e| e.to_string())?;
            if !settled {
                return Err("usage receipt has no usage or completed settlement fence; manual reconciliation required".into());
            }
        }
        sqlx::query("UPDATE requests SET metering_pending=FALSE WHERE id=$1")
            .bind(
                event
                    .usage
                    .request_id
                    .parse::<i64>()
                    .map_err(|e| e.to_string())?,
            )
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        tx.commit().await.map_err(|e| e.to_string())?;
        // The receipt is the durable commit fence even after completed usage
        // has passed its retention period. Never recreate a GC'd charge.
        return conduit_db::PgUsageRepo::new(pool.clone())
            .find_by_id(id)
            .await
            .map_err(|e| e.to_string());
    }
    let row = conduit_db::PgUsageRepo::insert_on_connection(&mut tx, row_from_input(&event.usage))
        .await
        .map_err(|e| e.to_string())?;
    let id: i64 = row.id.parse::<i64>().map_err(|e| e.to_string())?;
    sqlx::query("INSERT INTO usage_charge_outbox(usage_log_id,reservation_key,status,available_at,created_at,updated_at) VALUES($1,$2,'pending',now(),now(),now()) ON CONFLICT DO NOTHING")
        .bind(id).bind(&event.reservation_key).execute(&mut *tx).await.map_err(|e| e.to_string())?;
    sqlx::query("UPDATE usage_recovery_receipts SET usage_log_id=$2 WHERE event_key=$1")
        .bind(&event.event_key)
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    sqlx::query("UPDATE requests SET metering_pending=FALSE WHERE id=$1")
        .bind(
            event
                .usage
                .request_id
                .parse::<i64>()
                .map_err(|e| e.to_string())?,
        )
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(Some(row))
}
