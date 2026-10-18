//! Tests for the merge lanes: the pull-model `feed`, `apply`, and the lane
//! thread.

use std::sync::mpsc::Receiver;
use std::time::Duration;

use crate::pump::host::Ctx;
use crate::pump::merge::Merger;
use crate::pump::merge_test::setup;
use crate::pump::state::{MergeJob, MergeResult, NeedRecord, State, Status, Unit};

/// The next batch a detached lane was sent.
fn take(lane: &Receiver<Vec<MergeJob>>) -> Vec<MergeJob> {
    lane.recv_timeout(Duration::from_secs(5)).unwrap()
}

// ----------------------------------------------------------- the merge thread

/// A state with the given units in the given statuses, each serving need
/// `n1` on branch `work/<uid>`, and the need owning exactly those units.
pub(super) fn state_with(units: &[(&str, Status)]) -> State {
    let mut st = State::default();
    for (uid, status) in units {
        st.order.push((*uid).to_string());
        st.units.insert(
            (*uid).to_string(),
            Unit {
                need: String::from("n1"),
                title: format!("Unit {uid}"),
                write_scope: vec![String::from("src/pump/merge.rs")],
                proof: vec![format!("./proof-{uid}.sh")],
                tools: String::from("plain"),
                status: *status,
                attempts: 1,
                branch: Some(format!("work/{uid}")),
                ..Unit::default()
            },
        );
    }
    st.needs.insert(
        String::from("n1"),
        NeedRecord {
            units: st.order.clone(),
            ..NeedRecord::default()
        },
    );
    st
}

#[test]
fn feed_moves_delivered_units_to_merging_and_sends_a_job() {
    let (_dir, ctx, _host) = setup("");
    let (merger, _out, jobs) = Merger::detached();
    let mut st = state_with(&[
        ("u1", Status::Delivered),
        ("u2", Status::Inflight),
        ("u3", Status::Merging),
    ]);
    merger.feed(&ctx, &mut st);
    assert_eq!(st.units["u1"].status, Status::Merging);
    assert_eq!(st.units["u2"].status, Status::Inflight);
    assert_eq!(st.units["u3"].status, Status::Merging);
    let sent = take(&jobs).remove(0);
    assert_eq!(sent.uid, "u1");
    assert_eq!(sent.branch, "work/u1");
    assert_eq!(sent.title, "Unit u1");
    assert_eq!(sent.need, "n1");
    assert_eq!(sent.proof, vec![String::from("./proof-u1.sh")]);
    assert_eq!(sent.tools, "plain");
    assert_eq!(sent.write_scope, vec![String::from("src/pump/merge.rs")]);
    assert!(jobs.try_recv().is_err());
}

#[test]
fn feed_under_hold_merge_sends_only_fix_units() {
    let (_dir, ctx, _host) = setup("");
    std::fs::write(ctx.home.join("HOLD-MERGE"), "").unwrap();
    let (merger, _out, jobs) = Merger::detached();
    let mut st = state_with(&[("u1", Status::Delivered), ("pumpfix9", Status::Delivered)]);
    merger.feed(&ctx, &mut st);
    assert_eq!(st.units["u1"].status, Status::Delivered);
    assert_eq!(st.units["pumpfix9"].status, Status::Merging);
    let sent = take(&jobs).remove(0);
    assert_eq!(sent.uid, "pumpfix9");
    assert!(jobs.try_recv().is_err());
}

/// Write the configured priority file.
fn write_priority(ctx: &Ctx, text: &str) {
    let path = ctx.cfg.priority_path(&ctx.project);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[test]
fn feed_sends_priority_units_first_then_fix_units_then_the_rest() {
    let (_dir, ctx, _host) = setup("");
    write_priority(&ctx, "# hot\nhot\n");
    let (merger, _out, jobs) = Merger::detached();
    let mut st = state_with(&[
        ("u1", Status::Delivered),
        ("pumpfix9", Status::Delivered),
        ("u3", Status::Delivered),
    ]);
    st.units.get_mut("u3").unwrap().need = String::from("hot");
    merger.feed(&ctx, &mut st);
    let order: Vec<String> = take(&jobs).into_iter().map(|job| job.uid).collect();
    assert_eq!(order, ["u3", "pumpfix9", "u1"]);
}

#[test]
fn feed_under_hold_merge_also_sends_priority_units() {
    let (_dir, ctx, _host) = setup("");
    std::fs::write(ctx.home.join("HOLD-MERGE"), "").unwrap();
    write_priority(&ctx, "hot\n");
    let (merger, _out, jobs) = Merger::detached();
    let mut st = state_with(&[("u1", Status::Delivered), ("u2", Status::Delivered)]);
    st.units.get_mut("u2").unwrap().need = String::from("hot");
    merger.feed(&ctx, &mut st);
    assert_eq!(st.units["u1"].status, Status::Delivered);
    assert_eq!(st.units["u2"].status, Status::Merging);
    assert_eq!(take(&jobs)[0].uid, "u2");
    assert!(jobs.try_recv().is_err());
}

#[test]
fn feed_holds_a_delivered_unit_until_its_need_dependency_lands() {
    let (_dir, ctx, host) = setup("");
    host.reply(
        "graph blocked",
        0,
        r#"[{"source_id":"n1","kind":"need","label":"N","blocked_by":["dep"]}]"#,
    );
    let (merger, _out, jobs) = Merger::detached();
    let mut st = state_with(&[("u1", Status::Delivered)]);

    merger.feed(&ctx, &mut st);
    assert_eq!(st.units["u1"].status, Status::Delivered);
    assert_eq!(st.units["u1"].held_on.as_deref(), Some("dep"));
    assert!(jobs.try_recv().is_err());

    // The dependency lands: the next pass feeds the unit.
    host.reply("graph blocked", 0, "[]");
    merger.feed(&ctx, &mut st);
    assert_eq!(st.units["u1"].status, Status::Merging);
    assert!(st.units["u1"].held_on.is_none());
    assert_eq!(take(&jobs)[0].uid, "u1");
}

#[test]
fn apply_ok_true_marks_the_unit_merged_and_counts_the_merge() {
    let (_dir, ctx, host) = setup("");
    let (merger, out, _jobs) = Merger::detached();
    let mut st = state_with(&[("u1", Status::Merging), ("u2", Status::Inflight)]);
    out.send(MergeResult {
        uid: String::from("u1"),
        ok: Some(true),
        tip: Some(String::from("abcdef1234567890")),
        secs: Some(7),
        ..MergeResult::default()
    })
    .unwrap();
    merger.apply(&ctx, &mut st);
    let u = &st.units["u1"];
    assert_eq!(u.status, Status::Merged);
    assert_eq!(u.commit.as_deref(), Some("abcdef1234567890"));
    assert_eq!(u.since, 1_000_000);
    assert!(u.outside.is_empty());
    assert_eq!(st.merges, 1);
    assert_eq!(st.units["u2"].status, Status::Inflight);
    assert!(st.closed_needs.is_empty());
    assert!(
        host.logs()
            .iter()
            .any(|l| l == "merged u1 tip=abcdef1234 in 7s")
    );
}

#[test]
fn apply_closes_a_need_when_its_last_unit_merges_and_appends_needs_ready() {
    let (_dir, ctx, _host) = setup("");
    let (merger, out, _jobs) = Merger::detached();
    let mut st = state_with(&[("u1", Status::Merging), ("u2", Status::Merged)]);
    out.send(MergeResult {
        uid: String::from("u1"),
        ok: Some(true),
        tip: Some(String::from("tip1234567890abcd")),
        ..MergeResult::default()
    })
    .unwrap();
    merger.apply(&ctx, &mut st);
    assert_eq!(st.closed_needs, vec![String::from("n1")]);
    let text = std::fs::read_to_string(ctx.home.join("needs-ready.txt")).unwrap();
    assert_eq!(text, "n1 tip1234567 u1,u2\n");
}

#[test]
fn apply_ok_false_fails_the_unit_through_collect_fail() {
    let (_dir, ctx, host) = setup("");
    let (merger, out, _jobs) = Merger::detached();
    let mut st = state_with(&[("u1", Status::Merging)]);
    out.send(MergeResult {
        uid: String::from("u1"),
        ok: Some(false),
        why: Some(String::from("proof-failed")),
        detail: Some(String::from("boom")),
        ..MergeResult::default()
    })
    .unwrap();
    merger.apply(&ctx, &mut st);
    let u = &st.units["u1"];
    assert_eq!(u.status, Status::Pending);
    assert_eq!(u.task, None);
    assert_eq!(u.history.len(), 1);
    assert_eq!(u.history[0].attempt, 1);
    assert_eq!(u.history[0].why, "proof-failed");
    assert_eq!(u.history[0].detail, "boom");
    assert_eq!(st.merges, 0);
    assert!(
        host.logs()
            .iter()
            .any(|l| l == "fail u1 attempt=1 proof-failed")
    );
}

#[test]
fn apply_ok_null_returns_the_unit_to_delivered() {
    let (_dir, ctx, host) = setup("");
    let (merger, out, _jobs) = Merger::detached();
    let mut st = state_with(&[("u1", Status::Merging)]);
    out.send(MergeResult {
        uid: String::from("u1"),
        ok: None,
        why: Some(String::from("push-failed")),
        detail: Some("x".repeat(300)),
        ..MergeResult::default()
    })
    .unwrap();
    merger.apply(&ctx, &mut st);
    assert_eq!(st.units["u1"].status, Status::Delivered);
    assert_eq!(st.merges, 0);
    let expected = format!("push-failed u1: {}", "x".repeat(200));
    assert_eq!(
        host.logs().last().map(String::as_str),
        Some(expected.as_str())
    );
}

#[test]
fn an_out_of_scope_merge_logs_scope_warn() {
    let (_dir, ctx, host) = setup("");
    let (merger, out, _jobs) = Merger::detached();
    let mut st = state_with(&[("u1", Status::Merging)]);
    out.send(MergeResult {
        uid: String::from("u1"),
        ok: Some(true),
        tip: Some(String::from("tipabcdef")),
        outside: vec![String::from("README.md"), String::from("Cargo.lock")],
        secs: Some(3),
        ..MergeResult::default()
    })
    .unwrap();
    merger.apply(&ctx, &mut st);
    assert_eq!(st.units["u1"].status, Status::Merged);
    assert_eq!(
        st.units["u1"].outside,
        vec![String::from("README.md"), String::from("Cargo.lock")]
    );
    assert!(
        host.logs()
            .iter()
            .any(|l| l == "scope-warn u1 touched outside scope: README.md Cargo.lock")
    );
    assert!(
        host.logs()
            .iter()
            .any(|l| l == "merged u1 tip=tipabcdef in 3s")
    );
}

#[test]
fn the_thread_merges_a_single_job_with_merge_work_and_reports_one_result() {
    let (_dir, ctx, host) = setup("");
    host.reply("rev-parse HEAD", 0, "tipreal123\n");
    let merger = Merger::start(&ctx);
    let mut st = state_with(&[("u1", Status::Delivered)]);
    merger.feed(&ctx, &mut st);
    assert_eq!(st.units["u1"].status, Status::Merging);
    let result = merger.results.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(result.uid, "u1");
    assert_eq!(result.ok, Some(true));
    assert_eq!(result.tip.as_deref(), Some("tipreal123"));
    assert!(result.outside.is_empty());
    let cmds = host.commands();
    assert!(
        cmds.iter()
            .any(|c| c.contains("merge --no-ff --no-edit -m 'merge u1: Unit u1 (need n1)'"))
    );
    assert!(
        cmds.iter()
            .any(|c| c == "git push origin HEAD:refs/heads/next")
    );
    assert!(cmds.iter().all(|c| !c.contains("merge-jobs")));
    assert!(
        host.logs()
            .iter()
            .any(|l| l == "lane default/0 batch of 1: 1 merged")
    );
    assert!(
        host.logs()
            .iter()
            .any(|l| l == "proof u1 rc=0 0s: ./proof-u1.sh")
    );
    // nothing else is waiting on the channel
    assert!(merger.results.try_recv().is_err());
}
