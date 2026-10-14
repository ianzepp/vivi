//! Tests for [`super::merge`] part 1: `failure_text`, `reset_repo` and
//! `merge_work`.

use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use crate::pump::config::PumpConfig;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::merge::{failure_text, merge_work};
use crate::pump::state::MergeJob;

/// A context whose integration branch is `next`, with one shared check,
/// an optional formatter, and a fake host.
fn setup(format: &str) -> (TempDir, Ctx, Arc<FakeHost>) {
    let dir = tempdir().unwrap();
    let mut cfg = PumpConfig::default();
    cfg.branch = String::from("next");
    cfg.merge.format = format.to_string();
    cfg.merge.shared_checks = vec![String::from("cargo test --lib")];
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
