//! Cancellation tree (01 §6).
//!
//! ```text
//! daemon root ─┬─ session ─┬─ turn ─┬─ step (provider stream)
//!              │           │        ├─ tool calls ─ process tree / MCP / browser
//!              │           │        ├─ hooks
//!              │           │        └─ subagent turns
//!              │           ├─ terminals
//!              │           └─ background shell jobs
//!              ├─ MCP pool
//!              ├─ browser manager
//!              └─ background jobs (feeds, retention, indexer)
//! ```
//!
//! Rules: cancelling a parent cancels all children; a child never cancels its parent.
//! All scopes share one [`TaskTracker`] so daemon shutdown can wait for every task.
//! This is the **only** module allowed to start tasks (`cargo xtask check-spawn`).

use std::future::Future;
use std::time::Duration;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

#[derive(Clone)]
pub struct TaskScope {
    name: &'static str,
    token: CancellationToken,
    tracker: TaskTracker,
}

impl TaskScope {
    /// The daemon root scope.
    pub fn root() -> TaskScope {
        TaskScope {
            name: "daemon",
            token: CancellationToken::new(),
            tracker: TaskTracker::new(),
        }
    }

    /// A child scope: cancelled when `self` is cancelled; cancelling it leaves `self` alone.
    pub fn child(&self, name: &'static str) -> TaskScope {
        TaskScope {
            name,
            token: self.token.child_token(),
            tracker: self.tracker.clone(),
        }
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn token(&self) -> &CancellationToken {
        &self.token
    }

    pub fn cancel(&self) {
        self.token.cancel();
    }

    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    /// Spawn a task tracked by this tree. The future should watch [`Self::token`].
    pub fn spawn<F>(&self, fut: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.tracker.spawn(fut)
    }

    /// Spawn a task that is dropped (aborted at its next await) when the scope is cancelled.
    pub fn spawn_cancellable<F>(&self, fut: F) -> JoinHandle<Option<F::Output>>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let token = self.token.clone();
        self.tracker.spawn(async move {
            tokio::select! {
                out = fut => Some(out),
                () = token.cancelled() => None,
            }
        })
    }

    /// Root only: stop accepting new tasks and wait up to `timeout` for running ones.
    /// Returns `true` if everything finished in time.
    pub async fn close_and_wait(&self, timeout: Duration) -> bool {
        self.tracker.close();
        tokio::time::timeout(timeout, self.tracker.wait())
            .await
            .is_ok()
    }

    pub fn task_count(&self) -> usize {
        self.tracker.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn parent_cancels_child_not_vice_versa() {
        let root = TaskScope::root();
        let session = root.child("session");
        let turn = session.child("turn");
        let other = root.child("mcp");

        turn.cancel();
        assert!(turn.is_cancelled());
        assert!(!session.is_cancelled() && !root.is_cancelled());

        let tool = session.child("turn").child("tool");
        session.cancel();
        assert!(tool.is_cancelled());
        assert!(!other.is_cancelled(), "siblings untouched");

        root.cancel();
        assert!(other.is_cancelled());
    }

    #[tokio::test]
    async fn close_and_wait_waits_for_tasks() {
        let root = TaskScope::root();
        let s = root.child("job");
        let h = s.spawn_cancellable(futures::future::pending::<()>());
        root.cancel();
        assert!(root.close_and_wait(Duration::from_secs(2)).await);
        assert_eq!(h.await.unwrap(), None);
    }
}
