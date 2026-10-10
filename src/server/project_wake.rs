//! Per-project wake registry for append notification: one
//! `tokio::sync::Notify` per canonical project id, created on demand.
//! The project store calls [`ProjectWake::notify`] after a successful
//! append; a long-poll endpoint calls [`ProjectWake::wait`].

use std::collections::HashMap;
use std::pin::pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Notify;

/// Registry of per-project wake signals. Clone and share the handle;
/// every clone sees the same notifiers.
#[derive(Default, Debug)]
pub struct ProjectWake {
    notifiers: Mutex<HashMap<String, Arc<Notify>>>,
}

impl ProjectWake {
    pub fn new() -> Self {
        Self::default()
    }

    /// Wake every waiter registered on `project_id` (canonical id).
    /// No-op when nothing has ever waited on the project, so the append
    /// path cannot grow the map for projects nobody polls.
    pub fn notify(&self, project_id: &str) {
        if let Some(notify) = self
            .notifiers
            .lock()
            .expect("project wake mutex poisoned")
            .get(project_id)
            .cloned()
        {
            notify.notify_waiters();
        }
    }

    /// Wait until `project_id` (canonical id) is woken or `timeout`
    /// elapses. Returns whether a wake arrived first.
    pub async fn wait(&self, project_id: &str, timeout: Duration) -> bool {
        let notify = self
            .notifiers
            .lock()
            .expect("project wake mutex poisoned")
            .entry(project_id.to_string())
            .or_insert_with(|| Arc::new(Notify::new()))
            .clone();
        // Register the waiter before arming the deadline so a notify
        // racing the entry into wait() is not lost.
        let mut notified = pin!(notify.notified());
        notified.as_mut().enable();
        tokio::time::timeout(timeout, notified).await.is_ok()
    }
}
