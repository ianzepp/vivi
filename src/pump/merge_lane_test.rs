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
fn feed_sends_a_local_pool_job_with_its_worktree_source() {
    let (_dir, ctx, _host) = setup("");
    let ctx = ctx.with_pool_sources(BTreeMap::from([(
        String::from("glm"),
        PathBuf::from("/wtree/repo"),
    )]));
    let (merger, _out, jobs) = Merger::detached();
    let mut st = state_with(&[("u1", Status::Delivered)]);
    st.units.get_mut("u1").unwrap().pool = Some(String::from("glm"));
    merger.feed(&ctx, &mut st);
    assert_eq!(st.units["u1"].status, Status::Merging);
    assert_eq!(
        take(&jobs).remove(0).source,
        Some(PathBuf::from("/wtree/repo"))
    );
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

/// Give each need priority level 1 in pump.db.
fn write_priority(ctx: &Ctx, needs: &[&str]) {
    for need in needs {
        crate::pump::priority::set(&ctx.home, need, 1).unwrap();
    }
}

#[test]
fn feed_sends_fix_units_first_then_priority_units_then_the_rest() {
    let (_dir, ctx, _host) = setup("");
    write_priority(&ctx, &["hot"]);
    let (merger, _out, jobs) = Merger::detached();
    let mut st = state_with(&[
        ("u1", Status::Delivered),
        ("pumpfix9", Status::Delivered),
        ("u3", Status::Delivered),
    ]);
    st.units.get_mut("u3").unwrap().need = String::from("hot");
    merger.feed(&ctx, &mut st);
    let order: Vec<String> = take(&jobs).into_iter().map(|job| job.uid).collect();
    assert_eq!(order, ["pumpfix9", "u3", "u1"]);
}

#[test]
fn feed_under_hold_merge_also_sends_priority_units() {
    let (_dir, ctx, _host) = setup("");
    std::fs::write(ctx.home.join("HOLD-MERGE"), "").unwrap();
    write_priority(&ctx, &["hot"]);
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
fn apply_proof_usage_parks_the_unit_needs_replan() {
    let (_dir, ctx, host) = setup("");
    let (merger, out, _jobs) = Merger::detached();
    let mut st = state_with(&[("u1", Status::Merging)]);
    out.send(MergeResult {
        uid: String::from("u1"),
        ok: Some(false),
        why: Some(String::from("proof-usage")),
        detail: Some(String::from(
            "The unit's proof command failed as a usage error, not as a test failure.\n\
             command: cargo test --lib a b\n\
             excerpt: error: unexpected argument 'b' found",
        )),
        ..MergeResult::default()
    })
    .unwrap();
    merger.apply(&ctx, &mut st);
    let u = &st.units["u1"];
    assert_eq!(u.status, Status::NeedsReplan);
    assert_eq!(u.attempts, 1);
    assert!(u.history.is_empty());
    assert_eq!(u.task, None);
    assert!(!host.logs().iter().any(|l| l.starts_with("fail u1")));
    assert!(
        host.logs()
            .iter()
            .any(|l| l == "needs-replan u1: proof usage error")
    );
    assert!(st.needs["n1"].amendment_pending);
    let exceptions = std::fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap();
    assert!(exceptions.contains("needs-replan u1"));
    assert!(exceptions.contains("cargo test --lib a b"));
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
            .any(|l| l == "lane default/0: batch of 1: 1 merged")
    );
    assert!(
        host.logs()
            .iter()
            .any(|l| l == "lane default/0: proof u1 rc=0 0s: ./proof-u1.sh")
    );
    // nothing else is waiting on the channel
    assert!(merger.results.try_recv().is_err());
}

// ------------------------------------------------ unit branches in the base repo

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use crate::pump::config::{GateConfig, MergeConfig, PumpConfig, PumpTarget};
use crate::pump::host::{FakeHost, RealHost, test_ctx};
use crate::pump::merge::{batch_work, fetch_unit_branch, merge_work};

/// A config with one local-worktree target `tela` (integration branch `main`).
fn tela_config() -> PumpConfig {
    PumpConfig {
        targets: BTreeMap::from([(
            String::from("tela"),
            PumpTarget {
                path: String::from("tela"),
                branch: String::from("main"),
                merge: Some(MergeConfig {
                    shared_checks: vec![String::from("true")],
                    ..MergeConfig::default()
                }),
                gate: Some(GateConfig {
                    command: String::from("true"),
                    ..GateConfig::default()
                }),
                ..PumpTarget::default()
            },
        )]),
        committer_name: String::from("Pump Tester"),
        committer_email: String::from("pump@example.com"),
        ..PumpConfig::default()
    }
}

/// The `tela` target's lane 0 context, its lane 1 clone context, the base
/// repository path, and the fake host.
fn tela_lanes() -> (
    tempfile::TempDir,
    Ctx,
    Ctx,
    std::path::PathBuf,
    Arc<FakeHost>,
) {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), tela_config());
    let lane0 = ctx.for_target("tela").unwrap();
    let base = lane0.repo.clone();
    let lane1 = lane0.with_repo(ctx.home.join("merge-lanes").join("tela").join("1"));
    host.reply("merge-base --is-ancestor origin/work/", 1, "");
    host.reply("rev-parse HEAD", 0, "tipbase123\n");
    (dir, lane0, lane1, base, host)
}

/// A job for `uid` on branch `work/<uid>` of the `tela` target.
fn tela_job(uid: &str) -> MergeJob {
    MergeJob {
        proof: Vec::new(),
        target: String::from("tela"),
        ..crate::pump::merge_test::unit(uid, &format!("work/{uid}"))
    }
}

#[test]
fn a_lane_clone_fetches_a_base_only_unit_branch_through_pump_base() {
    let (_dir, _lane0, lane1, base, host) = tela_lanes();
    host.reply("remote get-url pump-base", 2, "error: No such remote\n");
    host.reply("ls-remote --exit-code pump-base refs/heads/work/u1", 0, "");
    let result = merge_work(&lane1, &tela_job("u1"));
    assert_eq!(result.ok, Some(true), "{result:?}\n{:?}", host.commands());
    let commands = host.commands();
    let add = format!("git remote add pump-base {}", base.display());
    assert!(commands.contains(&add), "{commands:?}");
    let fetch = "git fetch pump-base +refs/heads/work/u1:refs/remotes/origin/work/u1";
    assert!(commands.iter().any(|c| c == fetch), "{commands:?}");
    assert!(
        !commands
            .iter()
            .any(|c| c.contains("fetch origin refs/heads/work/u1"))
    );
}

#[test]
fn a_lane_clone_falls_back_to_the_remote_when_the_base_has_no_such_branch() {
    let (_dir, _lane0, lane1, _base, host) = tela_lanes();
    host.reply("ls-remote --exit-code pump-base refs/heads/work/u1", 2, "");
    let result = merge_work(&lane1, &tela_job("u1"));
    assert_eq!(result.ok, Some(true), "{result:?}");
    let commands = host.commands();
    let fetch = "git fetch origin refs/heads/work/u1:refs/remotes/origin/work/u1";
    assert!(commands.iter().any(|c| c == fetch), "{commands:?}");
    assert!(!commands.iter().any(|c| c.contains("fetch pump-base")));
}

#[test]
fn the_batch_path_fetches_base_only_branches_in_a_lane_clone_through_pump_base() {
    let (_dir, _lane0, lane1, _base, host) = tela_lanes();
    host.reply("ls-remote --exit-code pump-base refs/heads/", 0, "");
    let results = batch_work(&lane1, &[tela_job("u1"), tela_job("u2")]);
    assert!(results.iter().all(|r| r.ok == Some(true)), "{results:?}");
    let commands = host.commands();
    for uid in ["u1", "u2"] {
        let fetch =
            format!("git fetch pump-base +refs/heads/work/{uid}:refs/remotes/origin/work/{uid}");
        assert!(commands.contains(&fetch), "{commands:?}");
    }
    assert!(
        !commands
            .iter()
            .any(|c| c.starts_with("git fetch origin refs/heads/work/"))
    );
}

#[test]
fn the_batch_path_uses_the_local_branch_in_the_target_checkout_on_lane_zero() {
    let (_dir, lane0, _lane1, _base, host) = tela_lanes();
    host.reply("show-ref --verify --quiet refs/heads/work/", 0, "");
    let results = batch_work(&lane0, &[tela_job("u1"), tela_job("u2")]);
    assert!(results.iter().all(|r| r.ok == Some(true)), "{results:?}");
    let commands = host.commands();
    for uid in ["u1", "u2"] {
        let update = format!("git update-ref refs/remotes/origin/work/{uid} refs/heads/work/{uid}");
        assert!(commands.contains(&update), "{commands:?}");
    }
    assert!(!commands.iter().any(|c| c.contains("pump-base")));
    assert!(
        !commands
            .iter()
            .any(|c| c.starts_with("git fetch origin refs/heads/work/"))
    );
}

/// Run `git <args>` in `dir`, panicking on failure; the trimmed stdout.
fn git_in(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.name=T", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn with_real_git_a_lane_clone_gets_a_branch_that_only_the_base_repository_has() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let origin = root.join("origin.git");
    let base = root.join("base");
    let lane = root.join("lane");
    std::fs::create_dir_all(&origin).unwrap();
    git_in(&origin, &["init", "-q", "--bare", "-b", "main"]);
    git_in(
        root,
        &[
            "clone",
            "-q",
            origin.to_str().unwrap(),
            base.to_str().unwrap(),
        ],
    );
    git_in(&base, &["commit", "-q", "--allow-empty", "-m", "init"]);
    git_in(&base, &["push", "-q", "origin", "main"]);
    git_in(&base, &["checkout", "-q", "-b", "unit/x"]);
    git_in(&base, &["commit", "-q", "--allow-empty", "-m", "unit work"]);
    git_in(&base, &["checkout", "-q", "main"]);
    let want = git_in(&base, &["rev-parse", "refs/heads/unit/x"]);
    git_in(
        root,
        &[
            "clone",
            "-q",
            "--reference",
            base.to_str().unwrap(),
            origin.to_str().unwrap(),
            lane.to_str().unwrap(),
        ],
    );

    let mut cfg = tela_config();
    cfg.targets.get_mut("tela").unwrap().integration_repo = Some(base.to_string_lossy().into());
    let host = Arc::new(RealHost::new(root));
    let project = root.join("project");
    let ctx = Ctx::new(cfg, host, project, String::from("vivi"));
    let lane0 = ctx.for_target("tela").unwrap();
    assert_eq!(lane0.repo, base);
    let lane1 = lane0.with_repo(lane.clone());

    // The lane clone has no such branch; the base does, locally only.
    assert!(!git_in(&lane, &["branch", "-a"]).contains("unit/x"));
    let out = fetch_unit_branch(&lane1, "unit/x");
    assert_eq!(out.code, 0, "{}", out.text);
    assert_eq!(
        git_in(&lane, &["rev-parse", "refs/remotes/origin/unit/x"]),
        want
    );
    assert!(git_in(&lane, &["remote"]).contains("pump-base"));

    // Lane 0 is the base: the branch is local there.
    let out = fetch_unit_branch(&lane0, "unit/x");
    assert_eq!(out.code, 0, "{}", out.text);
    assert_eq!(
        git_in(&base, &["rev-parse", "refs/remotes/origin/unit/x"]),
        want
    );
}

#[test]
fn a_new_lane_clone_of_a_local_worktree_target_gets_the_pump_base_remote() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = tela_config();
    cfg.targets.get_mut("tela").unwrap().merge = Some(MergeConfig {
        lanes: 2,
        batch: 1,
        shared_checks: vec![String::from("true")],
        ..MergeConfig::default()
    });
    let (ctx, host) = test_ctx(dir.path(), cfg);
    host.reply("remote get-url origin", 0, "https://example.com/tela.git\n");
    host.reply("remote get-url pump-base", 2, "error: No such remote\n");
    host.reply("rev-parse HEAD", 0, "tipreal123\n");
    host.reply("merge-base --is-ancestor origin/work/", 1, "");
    let merger = Merger::start(&ctx);
    let mut st = state_with(&[("u1", Status::Delivered), ("u2", Status::Delivered)]);
    for unit in st.units.values_mut() {
        unit.target = String::from("tela");
    }
    merger.feed(&ctx, &mut st);
    for _ in 0..2 {
        let result = merger.results.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(result.ok, Some(true), "{result:?}");
    }
    let base = ctx.for_target("tela").unwrap().repo;
    let add = format!("git remote add pump-base {}", base.display());
    let commands = host.commands();
    let clone = commands
        .iter()
        .position(|c| c.starts_with("git clone"))
        .expect("lane 1 is cloned");
    assert!(commands[clone + 1..].contains(&add), "{commands:?}");
}
