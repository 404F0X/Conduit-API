//! Ownership of asynchronous maintenance and recovery work.
use crate::CancellationToken;
use std::{future::Future, sync::Mutex, time::Duration};
use tokio::task::JoinHandle;

#[derive(Debug, Default)]
pub struct TaskSupervisor {
    shutdown: CancellationToken,
    joins: Mutex<Vec<JoinHandle<()>>>,
    adopted: Mutex<Vec<tokio::task::AbortHandle>>,
}

impl TaskSupervisor {
    pub fn adopt(&self, handle: tokio::task::AbortHandle) {
        if let Ok(mut handles) = self.adopted.lock() {
            handles.retain(|handle| !handle.is_finished());
            if self.shutdown.is_cancelled() {
                handle.abort();
            } else {
                handles.push(handle);
            }
        } else {
            handle.abort();
        }
    }
    pub fn spawn(&self, future: impl Future<Output = ()> + Send + 'static) -> bool {
        let Ok(mut joins) = self.joins.lock() else {
            return false;
        };
        if self.shutdown.is_cancelled() {
            return false;
        }
        joins.retain(|join| !join.is_finished());
        let shutdown = self.shutdown.clone();
        joins.push(tokio::spawn(async move {
            tokio::select! { biased; () = shutdown.cancelled() => {}, () = future => {} }
        }));
        true
    }

    pub fn cancel(&self) {
        self.shutdown.cancel();
    }

    pub async fn shutdown(&self, timeout: Duration) {
        self.cancel();
        let adopted = self
            .adopted
            .lock()
            .map(|mut handles| std::mem::take(&mut *handles))
            .unwrap_or_default();
        for handle in &adopted {
            handle.abort();
        }
        let joins = self
            .joins
            .lock()
            .map(|mut joins| std::mem::take(&mut *joins))
            .unwrap_or_default();
        let deadline = tokio::time::Instant::now() + timeout;
        for mut join in joins {
            if tokio::time::timeout_at(deadline, &mut join).await.is_err() {
                join.abort();
                let _ = join.await;
            }
        }
        while adopted.iter().any(|handle| !handle.is_finished())
            && tokio::time::Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
}

impl Drop for TaskSupervisor {
    fn drop(&mut self) {
        self.shutdown.cancel();
        if let Ok(joins) = self.joins.get_mut() {
            for join in joins.drain(..) {
                join.abort();
            }
        }
        if let Ok(handles) = self.adopted.get_mut() {
            for handle in handles.drain(..) {
                handle.abort();
            }
        }
    }
}

/// The production owner also cancels on partial initialization failure.
pub struct TaskRuntime(pub std::sync::Arc<TaskSupervisor>);
impl Default for TaskRuntime {
    fn default() -> Self {
        Self(std::sync::Arc::new(TaskSupervisor::default()))
    }
}
impl Drop for TaskRuntime {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn repair_shutdown_also_waits_for_adopted_stream_tasks_and_partial_startup_cancels() {
        let tasks = std::sync::Arc::new(TaskSupervisor::default());
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let handle = tokio::spawn(async move {
            let _tx = tx;
            std::future::pending::<()>().await;
        });
        tasks.adopt(handle.abort_handle());
        tasks.shutdown(Duration::from_secs(1)).await;
        assert!(rx.await.is_err());
        assert!(handle.is_finished());
        let tasks = std::sync::Arc::new(TaskSupervisor::default());
        {
            let _owner = TaskRuntime(tasks.clone());
        }
        assert!(!tasks.spawn(async {}));
    }
    #[tokio::test]
    async fn shutdown_drops_inflight_work_and_rejects_late_work() {
        let tasks = TaskSupervisor::default();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        assert!(tasks.spawn(async move {
            let _tx = tx;
            std::future::pending::<()>().await;
        }));
        tasks.shutdown(Duration::from_secs(1)).await;
        assert!(rx.await.is_err());
        assert!(!tasks.spawn(async {}));
    }
}
