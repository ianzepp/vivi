//! Tests for [`super::merge`] part 1: `failure_text`, `reset_repo` and
//! `merge_work`.

use std::sync::Arc;
use std::time::Duration;

use tempfile::{TempDir, tempdir};

use crate::pump::config::{MergeConfig, PumpConfig};
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::merge::{Merger, batch_work, failure_text, merge_work};
use crate::pump::state::{MergeJob, MergeResult, NeedRecord, State, Status, Unit};

/// A context whose integration branch is `next`, with one shared check,
/// an optional formatter, and a fake host.
fn setup(format: &str) -> (TempDir, Ctx, Arc<FakeHost>) {
    let dir = tempdir().unwrap();
    let cfg = PumpConfig {
        branch: String::from("next"),
        merge: MergeConfig {
            format: format.to_string(),
            shared_checks: vec![String::from("cargo test --lib")],
            ..MergeConfig::default()
        },
        ..PumpConfig::default()
    };
    let (ctx, host) = test_ctx(dir.path(), cfg);
    (dir, ctx, host)
}

/// A plain unit job for branch `work/u1`.
fn job() -> MergeJob {
    MergeJob {
        uid: String::from("u1"),
        branch: String::from("work/u1"),
        title: String::from("A unit"),
        need: String::from("n1"),
        proof: vec![String::from("./proof-u1.sh")],
        tools: String::from("plain"),
        write_scope: vec![
            String::from("src/pump/merge.rs"),
            String::from("src/pump/merge_test.rs"),
        ],
    }
}

/// Index of the first recorded command containing `fragment`.
fn index_of(cmds: &[String], fragment: &str) -> usize {
    cmds.iter().position(|c| c.contains(fragment)).unwrap()
}

#[test]
fn failure_text_keeps_panic_and_error_lines_with_context() {
    let out = "running 3 tests\ntest a ... ok\ntest b ... FAILED\n---- b stdout ----\nvalue is 3\nexpected 2\nfiller line\nerror[E0308]: mismatched types\n --> src/x.rs:2:3\nassertion `left == right` failed\nleft: 3\nright: 2\n";
    let text = failure_text(out);
    assert!(text.contains("test b ... FAILED"));
    assert!(text.contains("---- b stdout ----"));
    assert!(text.contains("value is 3"));
    assert!(text.contains("expected 2"));
    assert!(text.contains("error[E0308]: mismatched types"));
    assert!(text.contains(" --> src/x.rs:2:3"));
    assert!(text.contains("assertion `left == right` failed"));
    assert!(text.contains("left: 3"));
    assert!(text.contains("right: 2"));
    let (head, _) = text.split_once("\n...\n").unwrap();
    assert!(!head.contains("running 3 tests"));
    assert!(!head.contains("test a ... ok"));
    assert!(text.ends_with("right: 2\n"));
}

#[test]
fn failure_text_falls_back_to_the_tail_when_nothing_matches() {
    let out = "nothing to see here\n".repeat(200);
    let text = failure_text(&out);
    assert_eq!(text.chars().count(), 1800);
    let expected: String = out.chars().skip(out.chars().count() - 1800).collect();
    assert_eq!(text, expected);
}

#[test]
fn a_clean_merge_with_passing_checks_is_pushed_and_returns_the_tip() {
    let (_dir, ctx, host) = setup("");
    host.reply("rev-parse HEAD", 0, "tip123\n");
    let result = merge_work(&ctx, &job());
    assert_eq!(result.ok, Some(true));
    assert_eq!(result.tip.as_deref(), Some("tip123"));
    assert!(result.outside.is_empty());
    assert_eq!(result.secs, Some(0));
    assert_eq!(result.why, None);
    assert_eq!(
        host.commands(),
        vec![
            "git merge --abort",
            "git fetch origin next",
            "git checkout -q next",
            "git reset -q --hard origin/next",
            "git fetch origin refs/heads/work/u1:refs/remotes/origin/work/u1",
            "git merge --no-ff --no-edit -m 'merge u1: A unit (need n1)' origin/work/u1",
            "git diff --name-only HEAD~1 HEAD",
            "cargo test --lib",
            "./proof-u1.sh",
            "git status --porcelain",
            "git push origin HEAD:refs/heads/next",
            "git rev-parse HEAD",
        ]
    );
    assert_eq!(
        host.logs(),
        vec![
            "proof u1 rc=0 0s: cargo test --lib",
            "proof u1 rc=0 0s: ./proof-u1.sh",
        ]
    );
}

#[test]
fn a_fetch_failure_is_reported_as_fetch_failed() {
    let (_dir, ctx, host) = setup("");
    host.reply("refs/heads/work/u1", 1, "fatal: could not fetch\n");
    let result = merge_work(&ctx, &job());
    assert_eq!(result.ok, Some(false));
    assert_eq!(result.why.as_deref(), Some("fetch-failed"));
    assert_eq!(result.detail.as_deref(), Some("fatal: could not fetch\n"));
    assert_eq!(result.tip, None);
    assert_eq!(result.secs, None);
    assert_eq!(
        host.commands().last().map(String::as_str),
        Some("git fetch origin refs/heads/work/u1:refs/remotes/origin/work/u1")
    );
}

#[test]
fn a_merge_conflict_lists_the_conflicting_files_and_resets_the_repo() {
    let (_dir, ctx, host) = setup("");
    host.reply(
        "merge --no-ff",
        1,
        "Auto-merging src/a.rs\nCONFLICT (content): merge conflict in src/a.rs\n",
    );
    host.reply(
        "diff --name-only --diff-filter=U",
        0,
        "src/a.rs\nsrc/b.rs\n",
    );
    let result = merge_work(&ctx, &job());
    assert_eq!(result.ok, Some(false));
    assert_eq!(result.why.as_deref(), Some("merge-conflict"));
    assert_eq!(
        result.detail.as_deref(),
        Some(
            "The branch no longer merges into the integration branch. \
             Conflicting files: src/a.rs\nsrc/b.rs\n"
        )
    );
    assert_eq!(
        host.commands().last().map(String::as_str),
        Some("git reset -q --hard origin/next")
    );
    assert_eq!(index_of(&host.commands(), "merge --no-ff"), 5);
}

#[test]
fn files_changed_outside_the_write_scope_are_listed_in_outside() {
    let (_dir, ctx, host) = setup("");
    host.reply(
        "diff --name-only HEAD~1 HEAD",
        0,
        "src/pump/merge.rs\nREADME.md\nCargo.lock\n",
    );
    host.reply("rev-parse HEAD", 0, "tip123\n");
    let result = merge_work(&ctx, &job());
    assert_eq!(result.ok, Some(true));
    assert_eq!(
        result.outside,
        vec![String::from("README.md"), String::from("Cargo.lock")]
    );
}

#[test]
fn a_failing_shared_check_is_proof_failed_and_names_the_command() {
    let (_dir, ctx, host) = setup("");
    host.reply("cargo test --lib", 1, "error[E0308]: mismatched types\n");
    let result = merge_work(&ctx, &job());
    assert_eq!(result.ok, Some(false));
    assert_eq!(result.why.as_deref(), Some("proof-failed"));
    let detail = result.detail.unwrap_or_default();
    assert!(detail.contains("this command failed (exit 1):\ncargo test --lib"));
    assert!(detail.contains("Last output:\nerror[E0308]: mismatched types"));
    assert_eq!(
        host.logs().first().map(String::as_str),
        Some("proof u1 rc=1 0s: cargo test --lib")
    );
    assert_eq!(
        host.commands().last().map(String::as_str),
        Some("git reset -q --hard origin/next")
    );
}

#[test]
fn a_failing_unit_proof_is_proof_failed() {
    let (_dir, ctx, host) = setup("");
    host.reply(
        "./proof-u1.sh",
        1,
        "thread 'main' panicked at 'boom', src/main.rs:4:5\n",
    );
    let result = merge_work(&ctx, &job());
    assert_eq!(result.ok, Some(false));
    assert_eq!(result.why.as_deref(), Some("proof-failed"));
    let detail = result.detail.unwrap_or_default();
    assert!(detail.contains("this command failed (exit 1):\n./proof-u1.sh"));
    assert!(detail.contains("Last output:\nthread 'main' panicked at 'boom'"));
}

#[test]
fn a_full_tools_unit_skips_its_own_proof() {
    let (_dir, ctx, host) = setup("");
    host.reply("./proof-u1.sh", 1, "boom");
    host.reply("rev-parse HEAD", 0, "tip123\n");
    let mut j = job();
    j.tools = String::from("full");
    let result = merge_work(&ctx, &j);
    assert_eq!(result.ok, Some(true));
    assert!(host.commands().iter().all(|c| !c.contains("./proof-u1.sh")));
}

#[test]
fn a_formatter_change_is_committed_as_a_style_commit_before_the_checks() {
    let (_dir, ctx, host) = setup("cargo fmt --all");
    host.reply_once("status --porcelain", 0, " M src/lib.rs\n");
    host.reply("rev-parse HEAD", 0, "tip123\n");
    let result = merge_work(&ctx, &job());
    assert_eq!(result.ok, Some(true));
    let cmds = host.commands();
    let fmt = index_of(&cmds, "cargo fmt --all");
    let commit = index_of(&cmds, "commit -q -am 'style: cargo fmt after u1'");
    let check = index_of(&cmds, "cargo test --lib");
    assert!(fmt < commit);
    assert!(commit < check);
    assert_eq!(
        cmds[commit],
        "git commit -q -am 'style: cargo fmt after u1'"
    );
}

#[test]
fn a_rejected_push_is_ok_null_with_push_failed() {
    let (_dir, ctx, host) = setup("");
    host.reply(
        "push origin HEAD",
        1,
        "remote: rejected\n ! [remote rejected] (fetch first)\n",
    );
    let result = merge_work(&ctx, &job());
    assert_eq!(result.ok, None);
    assert_eq!(result.why.as_deref(), Some("push-failed"));
    assert_eq!(
        result.detail.as_deref(),
        Some("remote: rejected\n ! [remote rejected] (fetch first)\n")
    );
    assert_eq!(result.tip, None);
    assert_eq!(
        host.commands().last().map(String::as_str),
        Some("git reset -q --hard origin/next")
    );
}

/// A batch of two clean jobs on distinct branches.
fn batch() -> Vec<MergeJob> {
    vec![unit("u1", "work/u1"), unit("u2", "work/u2")]
}

/// A plain unit job for branch `work/<uid>`.
fn unit(uid: &str, branch: &str) -> MergeJob {
    MergeJob {
        uid: uid.to_string(),
        branch: branch.to_string(),
        title: format!("Unit {uid}"),
        need: String::from("n1"),
        proof: vec![format!("./proof-{uid}.sh")],
        tools: String::from("plain"),
        write_scope: vec![
            String::from("src/pump/merge.rs"),
            String::from("src/pump/merge_test.rs"),
        ],
    }
}

/// The result for `uid`.
fn for_uid<'a>(results: &'a [MergeResult], uid: &str) -> &'a MergeResult {
    results.iter().find(|r| r.uid == uid).unwrap()
}

/// How many recorded commands contain `fragment`.
fn count(cmds: &[String], fragment: &str) -> usize {
    cmds.iter().filter(|c| c.contains(fragment)).count()
}

#[test]
fn a_batch_of_clean_jobs_is_pushed_once_and_every_job_gets_the_same_tip() {
    let (_dir, ctx, host) = setup("");
    host.reply(
        "diff --name-only HEAD~1 HEAD",
        0,
        "src/pump/merge.rs\nREADME.md\n",
    );
    host.reply("rev-parse HEAD", 0, "tip9\n");
    let results = batch_work(&ctx, &batch());
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|r| r.ok == Some(true)));
    assert_eq!(
        results
            .iter()
            .filter(|r| r.tip.as_deref() == Some("tip9"))
            .count(),
        2
    );
    assert_eq!(
        results
            .iter()
            .filter(|r| r.outside == vec![String::from("README.md")])
            .count(),
        2
    );
    assert_eq!(results.iter().filter(|r| r.secs == Some(1)).count(), 2);
    let cmds = host.commands();
    assert_eq!(count(&cmds, "git push origin HEAD:refs/heads/next"), 1);
    assert_eq!(count(&cmds, "merge u1"), 1);
    assert_eq!(count(&cmds, "merge u2"), 1);
}

#[test]
fn main_is_merged_into_the_integration_branch_first_when_it_is_not_an_ancestor() {
    let (_dir, ctx, host) = setup("");
    host.reply("merge-base --is-ancestor origin/main HEAD", 1, "");
    host.reply("rev-parse HEAD", 0, "tipM\n");
    let results = batch_work(&ctx, &batch());
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|r| r.ok == Some(true)));
    let cmds = host.commands();
    let fetch = index_of(&cmds, "git fetch -q origin main");
    let base = index_of(&cmds, "git merge-base --is-ancestor origin/main HEAD");
    let main = index_of(
        &cmds,
        "merge --no-ff --no-edit -m 'merge main into next' origin/main",
    );
    let u1 = index_of(&cmds, "merge --no-ff --no-edit -m 'merge u1:");
    assert!(fetch < base && base < main && main < u1);
    assert_eq!(
        host.logs().first().map(String::as_str),
        Some("merged main into next")
    );
}

#[test]
fn a_main_that_does_not_merge_cleanly_is_aborted_and_logged_and_the_batch_continues() {
    let (_dir, ctx, host) = setup("");
    host.reply("merge-base --is-ancestor origin/main HEAD", 1, "");
    host.reply(
        "merge main into next",
        1,
        "Auto-merging Cargo.toml\nCONFLICT: merge conflict in Cargo.toml\n",
    );
    host.reply("rev-parse HEAD", 0, "tip3\n");
    let results = batch_work(&ctx, &batch());
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|r| r.ok == Some(true)));
    let cmds = host.commands();
    let bad = index_of(&cmds, "merge main into next");
    assert_eq!(cmds[bad + 1], "git merge --abort");
    assert!(index_of(&cmds, "merge --no-ff --no-edit -m 'merge u1:") > bad);
    assert_eq!(
        host.logs().first().map(String::as_str),
        Some(
            "main does not merge into next cleanly: \
             Auto-merging Cargo.toml | CONFLICT: merge conflict in Cargo.toml | "
        )
    );
}

#[test]
fn a_conflicting_job_is_a_merge_conflict_result_and_the_others_still_merge() {
    let (_dir, ctx, host) = setup("");
    host.reply(
        "merge u1",
        1,
        "CONFLICT (content): merge conflict in src/a.rs\n",
    );
    host.reply("diff --name-only --diff-filter=U", 0, "src/a.rs\n");
    host.reply("diff --name-only HEAD~1 HEAD", 0, "src/pump/merge.rs\n");
    host.reply("rev-parse HEAD", 0, "tip4\n");
    let results = batch_work(&ctx, &batch());
    assert_eq!(results.len(), 2);
    let u1 = for_uid(&results, "u1");
    assert_eq!(u1.ok, Some(false));
    assert_eq!(u1.why.as_deref(), Some("merge-conflict"));
    assert_eq!(
        u1.detail.as_deref(),
        Some(
            "The branch no longer merges into the integration branch. \
             Conflicting files: src/a.rs\n"
        )
    );
    let u2 = for_uid(&results, "u2");
    assert_eq!(u2.ok, Some(true));
    assert_eq!(u2.tip.as_deref(), Some("tip4"));
    let cmds = host.commands();
    let bad = index_of(&cmds, "merge u1");
    assert_eq!(cmds[bad + 1], "git diff --name-only --diff-filter=U");
    assert_eq!(cmds[bad + 2], "git merge --abort");
    assert!(index_of(&cmds, "merge --no-ff --no-edit -m 'merge u2:") > bad);
}

#[test]
fn a_job_whose_own_proof_fails_is_the_culprit_and_the_rest_are_retried_in_round_two() {
    let (_dir, ctx, host) = setup("");
    host.reply("./proof-u1.sh", 1, "panicked at 'boom'\n");
    host.reply("diff --name-only HEAD~1 HEAD", 0, "src/pump/merge.rs\n");
    host.reply("rev-parse HEAD", 0, "tip5\n");
    let results = batch_work(&ctx, &batch());
    assert_eq!(results.len(), 2);
    let u1 = for_uid(&results, "u1");
    assert_eq!(u1.ok, Some(false));
    assert_eq!(u1.why.as_deref(), Some("proof-failed"));
    let detail = u1.detail.as_deref().unwrap_or_default();
    assert!(detail.contains("./proof-u1.sh"));
    assert!(detail.contains("What failed:\npanicked at 'boom'"));
    let u2 = for_uid(&results, "u2");
    assert_eq!(u2.ok, Some(true));
    assert_eq!(u2.tip.as_deref(), Some("tip5"));
    let cmds = host.commands();
    assert_eq!(count(&cmds, "merge u1"), 1);
    assert_eq!(count(&cmds, "merge u2"), 2);
    assert_eq!(count(&cmds, "git push origin HEAD:refs/heads/next"), 1);
    assert!(
        host.logs()
            .iter()
            .any(|l| l == "proof u1 rc=1 0s: ./proof-u1.sh")
    );
}

#[test]
fn a_shared_check_failure_with_several_jobs_falls_back_to_one_at_a_time() {
    let (_dir, ctx, host) = setup("");
    host.reply("cargo test --lib", 1, "error[E0308]: mismatched types\n");
    let results = batch_work(&ctx, &batch());
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|r| r.ok == Some(false)));
    assert!(
        results
            .iter()
            .all(|r| r.why.as_deref() == Some("proof-failed"))
    );
    let detail = for_uid(&results, "u1")
        .detail
        .as_deref()
        .unwrap_or_default();
    assert!(detail.contains("Last output:\nerror[E0308]: mismatched types"));
    let cmds = host.commands();
    assert_eq!(count(&cmds, "merge --no-ff"), 4);
    assert!(
        host.logs()
            .iter()
            .any(|l| l == "batch 2 shared rc=1 0s: cargo test --lib")
    );
}

#[test]
fn a_shared_check_failure_with_one_job_is_proof_failed() {
    let (_dir, ctx, host) = setup("");
    host.reply("cargo test --lib", 1, "error: boom\n");
    let results = batch_work(&ctx, &[unit("u1", "work/u1")]);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].ok, Some(false));
    assert_eq!(results[0].why.as_deref(), Some("proof-failed"));
    let detail = results[0].detail.as_deref().unwrap_or_default();
    assert!(detail.contains("What failed:\nerror: boom"));
    assert_eq!(
        host.logs(),
        vec![String::from("batch 1 shared rc=1 0s: cargo test --lib")]
    );
    let cmds = host.commands();
    assert!(cmds.iter().all(|c| !c.contains("./proof-u1.sh")));
    assert!(cmds.iter().all(|c| !c.contains("push")));
    assert_eq!(
        cmds.last().map(String::as_str),
        Some("git reset -q --hard origin/next")
    );
}

#[test]
fn a_refused_push_leaves_every_job_undecided_with_push_failed() {
    let (_dir, ctx, host) = setup("");
    host.reply(
        "push origin HEAD",
        1,
        " ! [remote rejected] next (fetch first)\n",
    );
    host.reply("diff --name-only HEAD~1 HEAD", 0, "src/pump/merge.rs\n");
    let results = batch_work(&ctx, &batch());
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|r| r.ok.is_none()));
    assert!(
        results
            .iter()
            .all(|r| r.why.as_deref() == Some("push-failed"))
    );
    assert!(
        results
            .iter()
            .all(|r| r.detail.as_deref() == Some(" ! [remote rejected] next (fetch first)\n"))
    );
    assert!(results.iter().all(|r| r.tip.is_none()));
    assert_eq!(
        host.commands().last().map(String::as_str),
        Some("git reset -q --hard origin/next")
    );
}

// ----------------------------------------------------------- the merge thread

/// A state with the given units in the given statuses, each serving need
/// `n1` on branch `work/<uid>`, and the need owning exactly those units.
fn state_with(units: &[(&str, Status)]) -> State {
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
    let sent = jobs.recv_timeout(Duration::from_secs(5)).unwrap();
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
    let sent = jobs.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(sent.uid, "pumpfix9");
    assert!(jobs.try_recv().is_err());
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
    assert!(host.logs().iter().any(|l| l == "batch of 1: 1 merged"));
    assert!(
        host.logs()
            .iter()
            .any(|l| l == "proof u1 rc=0 0s: ./proof-u1.sh")
    );
    // nothing else is waiting on the channel
    assert!(merger.results.try_recv().is_err());
}
