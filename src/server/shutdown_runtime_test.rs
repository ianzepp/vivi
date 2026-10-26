//! The runtime helper drives the serving future and returns its output
//! through an explicitly consumed runtime.

use std::time::Duration;

use super::super::run;

#[test]
fn run_drives_a_timer_and_a_spawned_task_and_returns_the_output() {
    let output = run(async {
        tokio::time::sleep(Duration::from_millis(1)).await;
        tokio::spawn(async { 21 * 2 })
            .await
            .expect("spawned task joins")
    })
    .expect("runtime builds");
    assert_eq!(output, 42);
}
