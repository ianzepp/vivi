//! Tests for the live merge-lane visibility: lane-tagged log lines, the
//! step lines of a batch, the lane status lines and `status.txt`.

use std::sync::Arc;

use crate::pump::host::Ctx;
use crate::pump::merge::{Merger, batch_work, merge_work};
use crate::pump::merge_lane_test::state_with;
use crate::pump::merge_test::{setup, unit};
use crate::pump::progress::{LaneProgress, duration_text};
use crate::pump::run::write_status;
use crate::pump::state::{MergeResult, State, Status};

/// `ctx` as the thread of lane `default/2`.
fn lane_ctx(ctx: &Ctx) -> Ctx {
    ctx.with_lane(Arc::new(LaneProgress::new("default/2")))
}

#[test]
fn a_lane_context_prefixes_log_lines_and_exceptions_but_a_plain_one_does_not() {
    let (_dir, ctx, host) = setup("");
    ctx.log("main line");
    let lane = lane_ctx(&ctx);
    lane.log("lane line");
    lane.step("a step");
    lane.exception("some-kind", "u1", "text");
    assert_eq!(
        host.logs(),
        vec![
            "main line",
            "lane default/2: lane line",
            "lane default/2: a step",
            "lane default/2: EXCEPTION some-kind u1",
        ]
    );
}

#[test]
fn a_two_unit_batch_logs_its_steps_in_order() {
    let (_dir, ctx, host) = setup("cargo fmt");
    host.reply("status --porcelain", 0, " M src/x.rs\n");
    host.reply("rev-parse HEAD", 0, "tip0123456789abc\n");
    let lane = lane_ctx(&ctx);
    let jobs = vec![unit("u1", "work/u1"), unit("u2", "work/u2")];
    let results = batch_work(&lane, &jobs);
    assert!(results.iter().all(|r| r.ok == Some(true)), "{results:?}");
    let logs: Vec<String> = host.logs();
    let expected = [
        "lane default/2: merge u1 1/2 ok",
        "lane default/2: merge u2 2/2 ok",
        "lane default/2: format: cargo fmt",
        "lane default/2: format done 0s",
        "lane default/2: style commit",
        "lane default/2: check 1/1 start: cargo test --lib",
        "lane default/2: batch 2 shared rc=0 0s: cargo test --lib",
        "lane default/2: proof u1 start: ./proof-u1.sh",
        "lane default/2: proof u1 rc=0 0s: ./proof-u1.sh",
        "lane default/2: proof u2 start: ./proof-u2.sh",
        "lane default/2: proof u2 rc=0 0s: ./proof-u2.sh",
        "lane default/2: publish: fetch",
        "lane default/2: publish: push attempt 1",
        "lane default/2: publish: pushed tip0123456",
    ];
    assert_eq!(logs, expected, "{logs:#?}");
}

#[test]
fn a_conflicting_unit_and_an_integrated_unit_say_so() {
    let (_dir, ctx, host) = setup("");
    host.reply("merge-base --is-ancestor origin/work/u2 origin/next", 0, "");
    host.reply("rev-parse origin/next", 0, "feedface00\n");
    host.reply("--no-edit -m 'merge u1", 1, "CONFLICT");
    host.reply("diff --name-only --diff-filter=U", 0, "src/lib.rs\n");
    let lane = lane_ctx(&ctx);
    let jobs = vec![unit("u1", "work/u1"), unit("u2", "work/u2")];
    let results = batch_work(&lane, &jobs);
    assert_eq!(results.len(), 2, "{results:?}");
    let logs = host.logs();
    assert!(
        logs.contains(&String::from("lane default/2: merge u1 1/2 conflict")),
        "{logs:#?}"
    );
    assert!(
        logs.contains(&String::from(
            "lane default/2: merge u2 2/2 already integrated"
        )),
        "{logs:#?}"
    );
}

#[test]
fn the_single_unit_path_logs_merge_check_and_publish_steps() {
    let (_dir, ctx, host) = setup("");
    host.reply("rev-parse HEAD", 0, "tip9\n");
    let lane = lane_ctx(&ctx);
    let result = merge_work(&lane, &unit("u1", "work/u1"));
    assert_eq!(result.ok, Some(true));
    let logs = host.logs();
    let expected = [
        "lane default/2: merge u1 1/1 ok",
        "lane default/2: check 1/1 start: cargo test --lib",
        "lane default/2: proof u1 rc=0 0s: cargo test --lib",
        "lane default/2: proof u1 start: ./proof-u1.sh",
        "lane default/2: proof u1 rc=0 0s: ./proof-u1.sh",
        "lane default/2: publish: fetch",
        "lane default/2: publish: push attempt 1",
        "lane default/2: publish: pushed tip9",
    ];
    assert_eq!(logs, expected, "{logs:#?}");
}

#[test]
fn a_moved_integration_tip_is_logged_before_it_is_merged() {
    let (_dir, ctx, host) = setup("");
    host.reply("merge-base --is-ancestor origin/next HEAD", 1, "");
    host.reply("rev-parse origin/next", 0, "0123456789abcdef\n");
    host.reply("rev-parse HEAD", 0, "tip9\n");
    let lane = lane_ctx(&ctx);
    let result = merge_work(&lane, &unit("u1", "work/u1"));
    assert_eq!(result.ok, Some(true), "{result:?}");
    let logs = host.logs();
    let moved = "lane default/2: publish: merging moved next tip 0123456789";
    assert!(logs.contains(&String::from(moved)), "{logs:#?}");
}

#[test]
fn a_rejected_push_logs_each_attempt_and_the_failure() {
    let (_dir, ctx, host) = setup("");
    host.reply("push origin HEAD:refs/heads/next", 1, "rejected");
    let lane = lane_ctx(&ctx);
    let result = merge_work(&lane, &unit("u1", "work/u1"));
    assert_eq!(result.ok, None);
    let logs = host.logs();
    for line in [
        "lane default/2: publish: push attempt 1",
        "lane default/2: publish: push attempt 3",
        "lane default/2: publish: failed push-failed",
    ] {
        assert!(logs.contains(&String::from(line)), "{line}: {logs:#?}");
    }
}

#[test]
fn durations_read_as_seconds_minutes_and_hours() {
    assert_eq!(duration_text(45), "45s");
    assert_eq!(duration_text(130), "2m10s");
    assert_eq!(duration_text(723), "12m03s");
    assert_eq!(duration_text(3905), "1h05m05s");
}

#[test]
fn lane_lines_show_idle_busy_and_backoff() {
    let (_dir, ctx, host) = setup("");
    host.set_now(1_000);
    let (merger, out, jobs) = Merger::detached_lanes(2);
    let mut st = state_with(&[("u1", Status::Delivered)]);
    assert_eq!(
        merger.lane_lines(1_000),
        ["lane default/0 idle", "lane default/1 idle"]
    );
    merger.feed(&ctx, &mut st);
    assert_eq!(jobs[0].try_recv().unwrap().len(), 1);
    assert_eq!(
        merger.lane_lines(1_723),
        [
            "lane default/0 busy 1 units 12m03s: queued (12m03s)",
            "lane default/1 idle"
        ]
    );
    out.send(failed_push("u1")).unwrap();
    merger.apply(&ctx, &mut st);
    for _ in 0..2 {
        st = state_with(&[("u1", Status::Delivered)]);
        merger.feed(&ctx, &mut st);
        assert_eq!(jobs[0].try_recv().unwrap().len(), 1);
        out.send(failed_push("u1")).unwrap();
        merger.apply(&ctx, &mut st);
    }
    let lines = merger.lane_lines(1_180);
    assert_eq!(lines[0], "lane default/0 backoff 7m00s left", "{lines:?}");
}

/// A lane-level failure for `uid`.
fn failed_push(uid: &str) -> MergeResult {
    MergeResult {
        uid: uid.to_string(),
        why: Some(String::from("push-failed")),
        detail: Some(String::from("rejected")),
        ..MergeResult::default()
    }
}

#[test]
fn a_busy_lane_reports_its_latest_step() {
    let progress = LaneProgress::new("default/1");
    progress.begin(8, 100);
    progress.set_step("check 1/4 start: stage 1", 713);
    assert_eq!(
        progress.busy_text(823),
        "8 units 12m03s: check 1/4 start: stage 1 (1m50s)"
    );
}

#[test]
fn status_txt_ends_with_one_line_per_lane() {
    let (dir, ctx, _host) = setup("");
    let (merger, _out, _jobs) = Merger::detached_lanes(2);
    write_status(&ctx, &State::default(), &merger.lane_lines(ctx.now()));
    let text = std::fs::read_to_string(ctx.home.join("status.txt")).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 9, "{text}");
    assert_eq!(lines[7], "lane default/0 idle");
    assert_eq!(lines[8], "lane default/1 idle");
    drop(dir);
}
