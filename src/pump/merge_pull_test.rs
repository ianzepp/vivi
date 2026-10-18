//! Tests for the pull model: lanes take ranked batches only when idle, each
//! lane works in its own checkout, and the publish routine absorbs a moved
//! remote tip.

use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use tempfile::{TempDir, tempdir};

use crate::pump::config::{MergeConfig, PumpConfig};
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::merge::{Merger, merge_work};
use crate::pump::merge_lane_test::state_with;
use crate::pump::state::{MergeJob, MergeResult, State, Status, Unit};

/// The next batch a detached lane was sent.
fn take(lane: &Receiver<Vec<MergeJob>>) -> Vec<MergeJob> {
    lane.recv_timeout(Duration::from_secs(5)).unwrap()
}

/// A context with `batch` jobs per lane and `lanes` lanes, one shared check.
fn lane_setup(batch: u32, lanes: u32) -> (TempDir, Ctx, Arc<FakeHost>) {
    let dir = tempdir().unwrap();
    let cfg = PumpConfig {
        branch: String::from("next"),
        committer_name: String::from("Pump Tester"),
        committer_email: String::from("pump@example.com"),
        merge: MergeConfig {
            batch,
            lanes,
            shared_checks: vec![String::from("cargo test --lib")],
            ..MergeConfig::default()
        },
        ..PumpConfig::default()
    };
    let (ctx, host) = test_ctx(dir.path(), cfg);
    host.reply("merge-base --is-ancestor origin/work/", 1, "");
    (dir, ctx, host)
}

/// `count` delivered units `u1..u<count>`.
fn delivered(count: usize) -> State {
    let units: Vec<(String, Status)> = (1..=count)
        .map(|i| (format!("u{i}"), Status::Delivered))
        .collect();
    let refs: Vec<(&str, Status)> = units.iter().map(|(u, s)| (u.as_str(), *s)).collect();
    state_with(&refs)
}

/// The uids of a batch.
fn uids(batch: &[MergeJob]) -> Vec<&str> {
    batch.iter().map(|job| job.uid.as_str()).collect()
}

/// A merged result for `uid`.
fn merged(uid: &str) -> MergeResult {
    MergeResult {
        uid: uid.to_string(),
        ok: Some(true),
        tip: Some(String::from("tip1234567890")),
        ..MergeResult::default()
    }
}

#[test]
fn an_idle_lane_takes_only_batch_units_and_the_rest_stay_delivered() {
    let (_dir, ctx, host) = lane_setup(3, 1);
    let (merger, _out, jobs) = Merger::detached();
    let mut st = delivered(8);
    merger.feed(&ctx, &mut st);
    assert_eq!(uids(&take(&jobs)), ["u1", "u2", "u3"]);
    for uid in ["u1", "u2", "u3"] {
        assert_eq!(st.units[uid].status, Status::Merging, "{uid}");
    }
    for uid in ["u4", "u5", "u6", "u7", "u8"] {
        assert_eq!(st.units[uid].status, Status::Delivered, "{uid}");
    }
    assert!(jobs.try_recv().is_err());
    assert!(
        host.logs()
            .iter()
            .any(|l| l == "lane default/0 takes 3: u1,u2,u3")
    );
}

#[test]
fn a_busy_lane_gets_nothing_until_its_results_are_applied() {
    let (_dir, ctx, _host) = lane_setup(2, 1);
    let (merger, out, jobs) = Merger::detached();
    let mut st = delivered(6);
    merger.feed(&ctx, &mut st);
    assert_eq!(uids(&take(&jobs)), ["u1", "u2"]);
    merger.feed(&ctx, &mut st);
    assert!(jobs.try_recv().is_err(), "the lane is busy");
    assert_eq!(st.units["u3"].status, Status::Delivered);

    // One of two results is not enough: the lane is still busy.
    out.send(merged("u1")).unwrap();
    merger.apply(&ctx, &mut st);
    merger.feed(&ctx, &mut st);
    assert!(jobs.try_recv().is_err(), "u2 is still inside the lane");

    // The last result frees the lane, which then takes the next two.
    out.send(merged("u2")).unwrap();
    merger.apply(&ctx, &mut st);
    merger.feed(&ctx, &mut st);
    assert_eq!(uids(&take(&jobs)), ["u3", "u4"]);
}

#[test]
fn a_fix_unit_delivered_after_twenty_others_is_taken_first_by_the_next_idle_lane() {
    let (_dir, ctx, _host) = lane_setup(3, 1);
    let (merger, out, jobs) = Merger::detached();
    let mut st = delivered(20);
    merger.feed(&ctx, &mut st);
    let first = take(&jobs);
    assert_eq!(uids(&first), ["u1", "u2", "u3"]);

    // A fix unit lands while the lane is busy, behind 17 delivered units.
    st.order.push(String::from("pumpfix9"));
    let fix = Unit {
        status: Status::Delivered,
        branch: Some(String::from("work/pumpfix9")),
        ..st.units["u1"].clone()
    };
    st.units.insert(String::from("pumpfix9"), fix);
    for job in &first {
        out.send(merged(&job.uid)).unwrap();
    }
    merger.apply(&ctx, &mut st);
    merger.feed(&ctx, &mut st);
    assert_eq!(uids(&take(&jobs)), ["pumpfix9", "u4", "u5"]);
}

#[test]
fn two_idle_lanes_each_get_a_distinct_batch() {
    let (_dir, ctx, host) = lane_setup(2, 2);
    let (merger, out, lanes) = Merger::detached_lanes(2);
    let mut st = delivered(5);
    merger.feed(&ctx, &mut st);
    assert_eq!(uids(&take(&lanes[0])), ["u1", "u2"]);
    assert_eq!(uids(&take(&lanes[1])), ["u3", "u4"]);
    assert_eq!(st.units["u5"].status, Status::Delivered);
    let logs = host.logs();
    assert!(logs.iter().any(|l| l == "lane default/0 takes 2: u1,u2"));
    assert!(logs.iter().any(|l| l == "lane default/1 takes 2: u3,u4"));

    // A slow lane never blocks the other: lane 1 finishes first and is
    // re-fed while lane 0 is still busy.
    out.send(merged("u3")).unwrap();
    out.send(merged("u4")).unwrap();
    merger.apply(&ctx, &mut st);
    merger.feed(&ctx, &mut st);
    assert_eq!(uids(&take(&lanes[1])), ["u5"]);
    assert!(lanes[0].try_recv().is_err());
}

#[test]
fn a_unit_naming_a_target_no_lane_serves_is_held_and_reported() {
    let (_dir, ctx, host) = lane_setup(2, 1);
    let (merger, _out, jobs) = Merger::detached();
    let mut st = delivered(1);
    st.units.get_mut("u1").unwrap().target = String::from("gone");
    merger.feed(&ctx, &mut st);
    assert_eq!(st.units["u1"].status, Status::Held);
    assert!(jobs.try_recv().is_err());
    assert!(
        host.logs()
            .iter()
            .any(|l| l == "EXCEPTION unknown-target u1")
    );
}

#[test]
fn started_lanes_each_merge_in_their_own_checkout_cloned_from_the_target_remote() {
    let (_dir, ctx, host) = lane_setup(1, 2);
    host.reply("remote get-url", 0, "https://example.com/r.git\n");
    host.reply("rev-parse HEAD", 0, "tipreal123\n");
    let merger = Merger::start(&ctx);
    let mut st = delivered(2);
    merger.feed(&ctx, &mut st);
    for _ in 0..2 {
        let result = merger.results.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(result.ok, Some(true), "{result:?}");
    }
    let lane_one = ctx.home.join("merge-lanes").join("default").join("1");
    let clones: Vec<String> = host
        .commands()
        .into_iter()
        .filter(|c| c.starts_with("git clone"))
        .collect();
    assert_eq!(clones.len(), 1, "only lane 1 is cloned: {clones:?}");
    assert!(clones[0].contains("--reference"), "{}", clones[0]);
    assert!(
        clones[0].contains("https://example.com/r.git"),
        "{}",
        clones[0]
    );
    assert!(
        clones[0].contains(lane_one.to_string_lossy().as_ref()),
        "{}",
        clones[0]
    );
    let logs = host.logs();
    assert!(logs.iter().any(|l| l == "lane default/0 takes 1: u1"));
    assert!(logs.iter().any(|l| l == "lane default/1 takes 1: u2"));
}

#[test]
fn a_lane_checkout_that_cannot_be_made_leaves_its_jobs_undecided() {
    let (_dir, ctx, host) = lane_setup(1, 2);
    host.reply("remote get-url", 2, "fatal: no such remote\n");
    let merger = Merger::start(&ctx);
    let mut st = delivered(2);
    merger.feed(&ctx, &mut st);
    let mut by_uid = std::collections::BTreeMap::new();
    for _ in 0..2 {
        let result = merger.results.recv_timeout(Duration::from_secs(5)).unwrap();
        by_uid.insert(result.uid.clone(), result);
    }
    assert_eq!(by_uid["u1"].ok, Some(true));
    assert_eq!(by_uid["u2"].ok, None);
    assert_eq!(by_uid["u2"].why.as_deref(), Some("lane-checkout-failed"));
    assert!(host.commands().iter().all(|c| !c.starts_with("git clone")));
}

// ------------------------------------------------------------ the publish routine

/// A plain job for branch `work/u1`.
fn job() -> MergeJob {
    MergeJob {
        uid: String::from("u1"),
        target: String::from("default"),
        branch: String::from("work/u1"),
        title: String::from("A unit"),
        need: String::from("n1"),
        proof: Vec::new(),
        tools: String::from("plain"),
        write_scope: vec![String::from("src/pump/merge.rs")],
    }
}

/// Commands that push the integration branch.
fn pushes(host: &FakeHost) -> usize {
    host.commands()
        .iter()
        .filter(|c| c.starts_with("git push "))
        .count()
}

#[test]
fn a_moved_remote_tip_is_merged_in_before_the_push_without_rerunning_checks() {
    let (_dir, ctx, host) = lane_setup(1, 1);
    host.reply("merge-base --is-ancestor origin/next HEAD", 1, "");
    host.reply("rev-parse HEAD", 0, "tipmoved\n");
    let result = merge_work(&ctx, &job());
    assert_eq!(result.ok, Some(true), "{result:?}");
    assert_eq!(result.tip.as_deref(), Some("tipmoved"));
    let cmds = host.commands();
    let merge = cmds
        .iter()
        .position(|c| {
            c.contains("merge --no-ff --no-edit -m 'merge origin/next into lane' origin/next")
        })
        .expect("the moved tip is merged");
    assert!(cmds[merge].contains("user.name=Pump Tester"));
    let push = cmds
        .iter()
        .position(|c| c == "git push origin HEAD:refs/heads/next")
        .unwrap();
    assert!(merge < push);
    assert_eq!(
        cmds.iter().filter(|c| *c == "cargo test --lib").count(),
        1,
        "the checks ran once, before the publish"
    );
}

#[test]
fn a_conflict_with_the_moved_remote_tip_is_undecided_and_nothing_is_pushed() {
    let (_dir, ctx, host) = lane_setup(1, 1);
    host.reply("merge-base --is-ancestor origin/next HEAD", 1, "");
    host.reply("merge origin/next into lane", 1, "CONFLICT\n");
    host.reply("diff --name-only --diff-filter=U", 0, "src/lib.rs\n");
    let result = merge_work(&ctx, &job());
    assert_eq!(result.ok, None, "{result:?}");
    assert_eq!(result.why.as_deref(), Some("publish-conflict"));
    assert!(result.detail.as_deref().unwrap().contains("src/lib.rs"));
    assert_eq!(pushes(&host), 0);
    assert_eq!(
        host.commands().last().map(String::as_str),
        Some("git reset -q --hard origin/next")
    );
}

#[test]
fn a_rejected_push_retries_fetch_merge_push_and_can_still_land() {
    let (_dir, ctx, host) = lane_setup(1, 1);
    host.reply_once("git push origin", 1, " ! [rejected] next (fetch first)\n");
    host.reply_once("git push origin", 1, " ! [rejected] next (fetch first)\n");
    host.reply("rev-parse HEAD", 0, "tipretry\n");
    let result = merge_work(&ctx, &job());
    assert_eq!(result.ok, Some(true), "{result:?}");
    assert_eq!(pushes(&host), 3);
    let fetches = host
        .commands()
        .iter()
        .filter(|c| *c == "git fetch -q origin next")
        .count();
    assert_eq!(
        fetches, 3,
        "each round fetches the integration branch first"
    );
}

#[test]
fn a_push_rejected_three_times_is_undecided() {
    let (_dir, ctx, host) = lane_setup(1, 1);
    host.reply("git push origin", 1, " ! [rejected] next (fetch first)\n");
    let result = merge_work(&ctx, &job());
    assert_eq!(result.ok, None, "{result:?}");
    assert_eq!(result.why.as_deref(), Some("push-failed"));
    assert_eq!(pushes(&host), 3);
    assert_eq!(
        host.commands().last().map(String::as_str),
        Some("git reset -q --hard origin/next")
    );
}
