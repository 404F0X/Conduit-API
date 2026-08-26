use async_trait::async_trait;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TransactionError {
    #[error("transaction is already closed")]
    AlreadyClosed,
    #[error("transaction backend error: {0}")]
    Backend(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionState {
    Active,
    Committed,
    RolledBack,
}

// ---- Ambient transaction slot (RUST-P3-003 S11) ----------------------------
//
// Go carries the live transaction through the request context by value-key:
// `ent.NewTxContext(parent, tx)` stores it and `ent.TxFromContext(ctx)`
// type-asserts it back (`conduit/internal/ent/ent.go:73-84`). Go's
// `context.WithValue` payload is an `interface{}`; the direct Rust analog is a
// type-erased `Arc<dyn Any + Send + Sync>` that the backend downcasts back to
// its concrete shared-transaction type. The slot lives on
// `repo::RequestContext` so repos keep their `&RequestContext` signatures.

/// Type-erased carrier for an ambient DB transaction attached to a
/// [`crate::repo::RequestContext`].
///
/// Mirrors Go `ent.NewTxContext` / `ent.TxFromContext` (ent.go:73-84): the
/// context stores an opaque value, and only the DB backend that put it there
/// knows the concrete type to take it back out (`Arc::downcast`, no unsafe).
/// Equality is `Arc::ptr_eq` — two slots are equal iff they carry the *same*
/// live transaction, which is what "same tx in ctx" means in Go.
#[derive(Clone)]
pub struct TxSlot(Arc<dyn std::any::Any + Send + Sync>);

impl TxSlot {
    /// Wrap an already-shared payload (avoids double-`Arc`).
    pub fn from_arc(payload: Arc<dyn std::any::Any + Send + Sync>) -> Self {
        Self(payload)
    }

    /// Recover the concrete payload — Go's `ctx.Value(txCtxKey{}).(*Tx)` type
    /// assertion. Returns `None` when the slot was populated by a different
    /// backend (wrong concrete type).
    pub fn downcast_arc<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        Arc::clone(&self.0).downcast::<T>().ok()
    }

    /// Clone out the erased payload — used by the auth→db context bridge to
    /// move the handle between the two `RequestContext` types without either
    /// crate depending on the other.
    pub fn as_any_arc(&self) -> Arc<dyn std::any::Any + Send + Sync> {
        Arc::clone(&self.0)
    }
}

impl PartialEq for TxSlot {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for TxSlot {}

impl std::fmt::Debug for TxSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The payload is a live DB transaction — never dump it.
        f.write_str("TxSlot(..)")
    }
}

#[async_trait]
pub trait TransactionManager: Send + Sync {
    type Handle: TransactionHandle;

    async fn begin(&self) -> Result<Self::Handle, TransactionError>;
}

#[async_trait]
pub trait TransactionHandle: Send {
    async fn commit(&mut self) -> Result<(), TransactionError>;

    async fn rollback(&mut self) -> Result<(), TransactionError>;

    async fn savepoint(&mut self, name: &str) -> Result<(), TransactionError>;

    async fn release_savepoint(&mut self, name: &str) -> Result<(), TransactionError>;

    async fn rollback_to_savepoint(&mut self, name: &str) -> Result<(), TransactionError>;

    fn state(&self) -> TransactionState;
}

pub type TransactionFuture<'tx, T> =
    Pin<Box<dyn Future<Output = Result<T, TransactionError>> + Send + 'tx>>;

pub async fn run_in_tx<M, F, T>(manager: &M, operation: F) -> Result<T, TransactionError>
where
    M: TransactionManager,
    F: for<'tx> FnOnce(&'tx mut M::Handle) -> TransactionFuture<'tx, T> + Send,
    T: Send,
{
    let mut tx = manager.begin().await?;
    match operation(&mut tx).await {
        Ok(value) => {
            tx.commit().await?;
            Ok(value)
        }
        Err(error) => {
            tx.rollback().await?;
            Err(error)
        }
    }
}

pub async fn run_nested_in_tx<H, F, T>(
    tx: &mut H,
    savepoint_name: &str,
    operation: F,
) -> Result<T, TransactionError>
where
    H: TransactionHandle,
    F: for<'tx> FnOnce(&'tx mut H) -> TransactionFuture<'tx, T> + Send,
    T: Send,
{
    tx.savepoint(savepoint_name).await?;
    match operation(tx).await {
        Ok(value) => {
            tx.release_savepoint(savepoint_name).await?;
            Ok(value)
        }
        Err(error) => {
            // Roll back the nested unit and release the savepoint so the outer tx can continue.
            tx.rollback_to_savepoint(savepoint_name).await?;
            tx.release_savepoint(savepoint_name).await?;
            Err(error)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransactionLogEntry {
    Begin,
    Commit,
    Rollback,
    Savepoint(String),
    ReleaseSavepoint(String),
    RollbackToSavepoint(String),
}

#[derive(Debug, Clone, Default)]
pub struct FakeTransactionManager {
    log: Arc<std::sync::Mutex<Vec<TransactionLogEntry>>>,
}

impl FakeTransactionManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn log(&self) -> Vec<TransactionLogEntry> {
        self.log
            .lock()
            .map_or_else(|_| Vec::new(), |log| log.clone())
    }
}

#[async_trait]
impl TransactionManager for FakeTransactionManager {
    type Handle = FakeTransactionHandle;

    async fn begin(&self) -> Result<Self::Handle, TransactionError> {
        push_log(&self.log, TransactionLogEntry::Begin)?;
        Ok(FakeTransactionHandle {
            log: Arc::clone(&self.log),
            state: TransactionState::Active,
        })
    }
}

#[derive(Debug)]
pub struct FakeTransactionHandle {
    log: Arc<std::sync::Mutex<Vec<TransactionLogEntry>>>,
    state: TransactionState,
}

#[async_trait]
impl TransactionHandle for FakeTransactionHandle {
    async fn commit(&mut self) -> Result<(), TransactionError> {
        self.ensure_active()?;
        push_log(&self.log, TransactionLogEntry::Commit)?;
        self.state = TransactionState::Committed;
        Ok(())
    }

    async fn rollback(&mut self) -> Result<(), TransactionError> {
        self.ensure_active()?;
        push_log(&self.log, TransactionLogEntry::Rollback)?;
        self.state = TransactionState::RolledBack;
        Ok(())
    }

    async fn savepoint(&mut self, name: &str) -> Result<(), TransactionError> {
        self.ensure_active()?;
        push_log(&self.log, TransactionLogEntry::Savepoint(name.to_string()))
    }

    async fn release_savepoint(&mut self, name: &str) -> Result<(), TransactionError> {
        self.ensure_active()?;
        push_log(
            &self.log,
            TransactionLogEntry::ReleaseSavepoint(name.to_string()),
        )
    }

    async fn rollback_to_savepoint(&mut self, name: &str) -> Result<(), TransactionError> {
        self.ensure_active()?;
        push_log(
            &self.log,
            TransactionLogEntry::RollbackToSavepoint(name.to_string()),
        )
    }

    fn state(&self) -> TransactionState {
        self.state
    }
}

impl FakeTransactionHandle {
    fn ensure_active(&self) -> Result<(), TransactionError> {
        match self.state {
            TransactionState::Active => Ok(()),
            TransactionState::Committed | TransactionState::RolledBack => {
                Err(TransactionError::AlreadyClosed)
            }
        }
    }
}

fn push_log(
    log: &std::sync::Mutex<Vec<TransactionLogEntry>>,
    entry: TransactionLogEntry,
) -> Result<(), TransactionError> {
    let mut log = log
        .lock()
        .map_err(|_| TransactionError::Backend("fake transaction log poisoned".to_string()))?;
    log.push(entry);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fake_transaction_commit() -> Result<(), TransactionError> {
        let manager = FakeTransactionManager::new();
        let mut tx = manager.begin().await?;

        tx.commit().await?;

        assert_eq!(tx.state(), TransactionState::Committed);
        assert_eq!(
            manager.log(),
            vec![TransactionLogEntry::Begin, TransactionLogEntry::Commit]
        );
        Ok(())
    }

    #[tokio::test]
    async fn fake_transaction_rollback() -> Result<(), TransactionError> {
        let manager = FakeTransactionManager::new();
        let mut tx = manager.begin().await?;

        tx.rollback().await?;

        assert_eq!(tx.state(), TransactionState::RolledBack);
        assert_eq!(
            manager.log(),
            vec![TransactionLogEntry::Begin, TransactionLogEntry::Rollback]
        );
        Ok(())
    }

    #[tokio::test]
    async fn fake_nested_transaction_rollback_to_savepoint() -> Result<(), TransactionError> {
        let manager = FakeTransactionManager::new();
        let mut tx = manager.begin().await?;

        tx.savepoint("nested_1").await?;
        tx.rollback_to_savepoint("nested_1").await?;
        tx.release_savepoint("nested_1").await?;
        tx.commit().await?;

        assert_eq!(
            manager.log(),
            vec![
                TransactionLogEntry::Begin,
                TransactionLogEntry::Savepoint("nested_1".to_string()),
                TransactionLogEntry::RollbackToSavepoint("nested_1".to_string()),
                TransactionLogEntry::ReleaseSavepoint("nested_1".to_string()),
                TransactionLogEntry::Commit,
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn fake_nested_transaction_success() -> Result<(), TransactionError> {
        let manager = FakeTransactionManager::new();
        let mut tx = manager.begin().await?;

        tx.savepoint("nested_1").await?;
        tx.release_savepoint("nested_1").await?;
        tx.commit().await?;

        assert_eq!(
            manager.log(),
            vec![
                TransactionLogEntry::Begin,
                TransactionLogEntry::Savepoint("nested_1".to_string()),
                TransactionLogEntry::ReleaseSavepoint("nested_1".to_string()),
                TransactionLogEntry::Commit,
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn run_in_tx_commits_successful_operation() -> Result<(), TransactionError> {
        let manager = FakeTransactionManager::new();

        let value = run_in_tx(&manager, |tx| {
            Box::pin(async move {
                tx.savepoint("work").await?;
                tx.release_savepoint("work").await?;
                Ok(42)
            })
        })
        .await?;

        assert_eq!(value, 42);
        assert_eq!(
            manager.log(),
            vec![
                TransactionLogEntry::Begin,
                TransactionLogEntry::Savepoint("work".to_string()),
                TransactionLogEntry::ReleaseSavepoint("work".to_string()),
                TransactionLogEntry::Commit,
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn run_in_tx_rolls_back_failed_operation() {
        let manager = FakeTransactionManager::new();

        let result = run_in_tx(&manager, |_tx| {
            Box::pin(async {
                Err::<(), TransactionError>(TransactionError::Backend("boom".to_string()))
            })
        })
        .await;

        assert_eq!(result, Err(TransactionError::Backend("boom".to_string())));
        assert_eq!(
            manager.log(),
            vec![TransactionLogEntry::Begin, TransactionLogEntry::Rollback]
        );
    }

    #[tokio::test]
    async fn run_nested_in_tx_releases_savepoint_on_success() -> Result<(), TransactionError> {
        let manager = FakeTransactionManager::new();

        run_in_tx(&manager, |tx| {
            Box::pin(async move {
                run_nested_in_tx(tx, "nested_1", |tx| {
                    Box::pin(async move {
                        tx.savepoint("inner_work").await?;
                        tx.release_savepoint("inner_work").await?;
                        Ok(())
                    })
                })
                .await
            })
        })
        .await?;

        assert_eq!(
            manager.log(),
            vec![
                TransactionLogEntry::Begin,
                TransactionLogEntry::Savepoint("nested_1".to_string()),
                TransactionLogEntry::Savepoint("inner_work".to_string()),
                TransactionLogEntry::ReleaseSavepoint("inner_work".to_string()),
                TransactionLogEntry::ReleaseSavepoint("nested_1".to_string()),
                TransactionLogEntry::Commit,
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn run_nested_in_tx_rolls_back_to_savepoint_and_outer_tx_can_commit()
    -> Result<(), TransactionError> {
        let manager = FakeTransactionManager::new();

        run_in_tx(&manager, |tx| {
            Box::pin(async move {
                let result = run_nested_in_tx(tx, "nested_1", |_tx| {
                    Box::pin(async {
                        Err::<(), TransactionError>(TransactionError::Backend(
                            "nested failed".to_string(),
                        ))
                    })
                })
                .await;

                assert_eq!(
                    result,
                    Err(TransactionError::Backend("nested failed".to_string()))
                );
                Ok(())
            })
        })
        .await?;

        assert_eq!(
            manager.log(),
            vec![
                TransactionLogEntry::Begin,
                TransactionLogEntry::Savepoint("nested_1".to_string()),
                TransactionLogEntry::RollbackToSavepoint("nested_1".to_string()),
                TransactionLogEntry::ReleaseSavepoint("nested_1".to_string()),
                TransactionLogEntry::Commit,
            ]
        );
        Ok(())
    }

    // ---- TxSlot (RUST-P3-003 S11) ----

    #[test]
    fn tx_slot_downcast_recovers_concrete_payload() {
        // Mirrors Go's `ctx.Value(txCtxKey{}).(*Tx)` type assertion: the slot
        // erases the type on the way in and recovers it on the way out.
        let payload: Arc<std::sync::Mutex<i32>> = Arc::new(std::sync::Mutex::new(7));
        let slot = TxSlot::from_arc(payload);

        let recovered = slot.downcast_arc::<std::sync::Mutex<i32>>();
        let value = recovered
            .as_deref()
            .and_then(|mutex| mutex.lock().ok().map(|guard| *guard));
        assert_eq!(value, Some(7));
    }

    #[test]
    fn tx_slot_downcast_to_wrong_type_is_none() {
        // Go's type assertion `.(T)` with the wrong T yields (nil, false); the
        // Rust analog is a `None` — a slot from another backend is invisible.
        let slot = TxSlot::from_arc(Arc::new(String::from("not a tx")));
        assert!(slot.downcast_arc::<std::sync::Mutex<i32>>().is_none());
    }

    #[test]
    fn tx_slot_equality_is_pointer_identity() {
        let a = TxSlot::from_arc(Arc::new(1_i32));
        let b = a.clone();
        let c = TxSlot::from_arc(Arc::new(1_i32));

        // Clones carry the same live tx (equal); a fresh slot does not.
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(format!("{a:?}"), "TxSlot(..)");
    }
}
