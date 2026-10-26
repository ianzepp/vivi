//! Focused tests for the per-project wake registry.

use std::sync::Arc;
use std::time::{Duration, Instant};

use vivi::server::project_wake::ProjectWake;

#[tokio::test]
async fn same_project_notify_wakes_waiter() {
    let wake = Arc::new(ProjectWake::new());
    let waiter = tokio::spawn({
        let wake = wake.clone();
        async move { wake.wait("proj-a", Duration::from_secs(2)).await }
    });
    // Yield so the waiter registers before we notify.
    tokio::task::yield_now().await;
    wake.notify("proj-a");
    assert!(
        waiter.await.unwrap(),
        "a notify on the same project must wake the waiter"
    );
}

#[tokio::test]
async fn wait_times_out_without_a_notify() {
    let wake = ProjectWake::new();
    let timeout = Duration::from_millis(50);
    let started = Instant::now();
    assert!(!wake.wait("proj-timeout", timeout).await);
    assert!(started.elapsed() >= timeout);
}

#[tokio::test]
async fn other_project_notify_does_not_wake_waiter() {
    let wake = Arc::new(ProjectWake::new());
    let waiter = tokio::spawn({
        let wake = wake.clone();
        async move { wake.wait("proj-a", Duration::from_millis(150)).await }
    });
    tokio::task::yield_now().await;
    wake.notify("proj-b");
    assert!(
        !waiter.await.unwrap(),
        "a notify on another project must not wake the waiter"
    );
}
