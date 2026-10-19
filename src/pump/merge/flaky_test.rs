//! Tests for shared-check flake accounting and project-need escalation.

use std::fs;

use tempfile::tempdir;

use crate::pump::config::PumpConfig;
use crate::pump::host::test_ctx;

use super::{DAILY_NEED_THRESHOLD, DAY_SECS, need_body_path, need_subject, record_worker_flake};

const CHECK: &str = "python scripta/test-stage4b-workspace.py";
const FAILURE: &str = "test stage4b_timeout ... FAILED\nfixture timed out under load";

#[test]
fn repeated_flakes_in_a_rolling_day_open_one_project_need() {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    host.reply("need list --status open", 0, "[]");
    host.reply("need send", 0, "created operator ab12cd34\nsent ab12cd34\n");

    for _ in 0..DAILY_NEED_THRESHOLD - 1 {
        record_worker_flake(&ctx, "stage4b_timeout", CHECK, FAILURE);
    }
    assert!(
        host.commands()
            .iter()
            .all(|command| !command.contains("need send"))
    );

    // The first two events fall outside the rolling day and no longer count.
    host.advance(DAY_SECS + 1);
    record_worker_flake(&ctx, "stage4b_timeout", CHECK, FAILURE);
    assert!(
        host.commands()
            .iter()
            .all(|command| !command.contains("need send"))
    );

    for _ in 1..DAILY_NEED_THRESHOLD {
        record_worker_flake(&ctx, "stage4b_timeout", CHECK, FAILURE);
    }
    assert_eq!(
        host.commands()
            .iter()
            .filter(|command| command.contains("need send"))
            .count(),
        1
    );
    assert!(
        fs::read_to_string(need_body_path(&ctx, "stage4b_timeout"))
            .unwrap()
            .contains("done_when: Make `stage4b_timeout` pass reliably")
    );

    // The open project need suppresses a duplicate when later flakes arrive.
    host.reply(
        "need list --status open",
        0,
        &format!(
            r#"[{{"handle":"ab12cd34","subject":"{}","date":"2026-10-19","marks":[]}}]"#,
            need_subject("stage4b_timeout")
        ),
    );
    record_worker_flake(&ctx, "stage4b_timeout", CHECK, FAILURE);
    assert_eq!(
        host.commands()
            .iter()
            .filter(|command| command.contains("need send"))
            .count(),
        1
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("home/exceptions.txt"))
            .unwrap()
            .matches("flaky-check stage4b_timeout")
            .count(),
        DAILY_NEED_THRESHOLD + 3
    );
}
