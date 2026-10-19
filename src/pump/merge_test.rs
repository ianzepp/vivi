//! Tests for [`super::merge`] part 1: `failure_text`, `reset_repo` and
//! `merge_work`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tempfile::{TempDir, tempdir};

use crate::pump::config::{Generated, MergeConfig, PumpConfig, PumpTarget};
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::merge::{batch_work, clear_stale_lock, failure_text, merge_work};
use crate::pump::state::{MergeJob, MergeResult};

/// A context whose integration branch is `next`, with one shared check,
/// an optional formatter, and a fake host.
pub(super) fn setup(format: &str) -> (TempDir, Ctx, Arc<FakeHost>) {
    let dir = tempdir().unwrap();
    let cfg = PumpConfig {
        branch: String::from("next"),
        committer_name: String::from("Pump Tester"),
        committer_email: String::from("pump@example.com"),
        merge: MergeConfig {
            format: format.to_string(),
            shared_checks: vec![String::from("cargo test --lib")],
            ..MergeConfig::default()
        },
        ..PumpConfig::default()
    };
    let (ctx, host) = test_ctx(dir.path(), cfg);
    host.reply("merge-base --is-ancestor origin/work/", 1, "");
    (dir, ctx, host)
}

/// A plain unit job for branch `work/u1`.
fn job() -> MergeJob {
    MergeJob {
        uid: String::from("u1"),
        target: String::from("default"),
        branch: String::from("work/u1"),
        title: String::from("A unit"),
        need: String::from("n1"),
        proof: vec![String::from("./proof-u1.sh")],
        tools: String::from("plain"),
        write_scope: vec![
            String::from("src/pump/merge.rs"),
            String::from("src/pump/merge_test.rs"),
        ],
        source: None,
    }
}

/// The identity options every commit-making command carries.
const IDENTITY: &str = "-c 'user.name=Pump Tester' -c user.email=pump@example.com";

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
    assert_eq!(result.ok, Some(true), "{result:?}\n{:?}", host.commands());
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
            "git merge-base --is-ancestor origin/work/u1 origin/next",
            &format!(
                "git {IDENTITY} merge --no-ff --no-edit -m 'merge u1: A unit (need n1)' origin/work/u1"
            )[..],
            "git diff --name-only HEAD~1 HEAD",
            "cargo test --lib",
            "./proof-u1.sh",
            "git status --porcelain",
            "git fetch -q origin next",
            "git merge-base --is-ancestor origin/next HEAD",
            "git push origin HEAD:refs/heads/next",
            "git rev-parse HEAD",
        ]
    );
    assert_eq!(
        host.logs(),
        vec![
            "merge u1 1/1 ok",
            "check 1/1 start: cargo test --lib",
            "proof u1 rc=0 0s: cargo test --lib",
            "proof u1 start: ./proof-u1.sh",
            "proof u1 rc=0 0s: ./proof-u1.sh",
            "publish: fetch",
            "publish: push attempt 1",
            "publish: pushed tip123",
        ]
    );
}

#[test]
fn a_job_with_a_pool_source_fetches_from_the_pool_repository() {
    let (_dir, ctx, host) = setup("");
    host.reply("rev-parse HEAD", 0, "tip123\n");
    let mut j = unit("u1", "work/u1");
    j.source = Some(PathBuf::from("/wtree/repo"));
    let result = merge_work(&ctx, &j);
    assert_eq!(result.ok, Some(true), "{result:?}\n{:?}", host.commands());
    let commands = host.commands();
    assert!(
        commands.contains(
            &"git fetch /wtree/repo +refs/heads/work/u1:refs/remotes/origin/work/u1".to_string()
        ),
        "{commands:?}"
    );
    assert!(
        !commands
            .iter()
            .any(|c| c.contains("fetch origin refs/heads/work/u1"))
    );
}

#[test]
fn a_named_local_target_merges_its_local_unit_branch() {
    let dir = tempdir().unwrap();
    let cfg = PumpConfig {
        targets: std::collections::BTreeMap::from([(
            String::from("tela"),
            PumpTarget {
                path: String::from("tela"),
                branch: String::from("main"),
                merge: Some(MergeConfig {
                    shared_checks: vec![String::from("true")],
                    ..MergeConfig::default()
                }),
                gate: Some(crate::pump::config::GateConfig {
                    command: String::from("true"),
                    ..crate::pump::config::GateConfig::default()
                }),
                ..PumpTarget::default()
            },
        )]),
        committer_name: String::from("Pump Tester"),
        committer_email: String::from("pump@example.com"),
        ..PumpConfig::default()
    };
    let (ctx, host) = test_ctx(dir.path(), cfg);
    host.reply("show-ref --verify --quiet refs/heads/work/u1", 0, "");
    host.reply("merge-base --is-ancestor origin/work/u1 origin/main", 1, "");
    host.reply("rev-parse HEAD", 0, "tiplocal123\n");
    let mut local_job = job();
    local_job.target = String::from("tela");
    let target_ctx = ctx.for_target("tela").unwrap();
    let result = merge_work(&target_ctx, &local_job);
    assert_eq!(result.ok, Some(true), "{result:?}\n{:?}", host.commands());
    let commands = host.commands();
    assert!(commands.iter().any(|command| {
        command == "git update-ref refs/remotes/origin/work/u1 refs/heads/work/u1"
    }));
    assert!(
        !commands
            .iter()
            .any(|command| { command.contains("fetch origin refs/heads/work/u1") })
    );
}

#[test]
fn an_already_integrated_replay_restores_only_the_receipt() {
    let (_dir, ctx, host) = setup("cargo fmt --all");
    host.reply("merge-base --is-ancestor origin/work/u1 origin/next", 0, "");
    host.reply("rev-parse origin/next", 0, "landed-tip\n");
    host.reply("./proof-u1.sh", 1, "must not rerun");
    host.reply("diff --name-only HEAD~1 HEAD", 0, "unrelated.rs\n");
    let result = merge_work(&ctx, &job());
    assert_eq!(result.ok, Some(true));
    assert_eq!(result.tip.as_deref(), Some("landed-tip"));
    assert!(result.outside.is_empty());
    assert_eq!(
        host.commands().len(),
        7,
        "only reset, fetch and ancestry lookup"
    );
    assert_eq!(host.commands().last().unwrap(), "git rev-parse origin/next");
    assert_eq!(host.logs(), vec!["merge u1 1/1 already integrated"]);
}

#[test]
fn an_ancestry_error_retries_without_merging_checking_or_pushing() {
    let (_dir, ctx, host) = setup("cargo fmt --all");
    host.reply(
        "merge-base --is-ancestor origin/work/u1 origin/next",
        128,
        "bad ref",
    );
    let result = merge_work(&ctx, &job());
    assert_eq!(result.ok, None);
    assert_eq!(result.why.as_deref(), Some("integration-check-failed"));
    assert_eq!(result.detail.as_deref(), Some("bad ref"));
    assert!(result.tip.is_none());
    assert!(result.outside.is_empty());
    assert_eq!(host.commands().len(), 6, "stop at the ancestry lookup");
    assert_eq!(
        host.commands().last().unwrap(),
        "git merge-base --is-ancestor origin/work/u1 origin/next"
    );
    assert_eq!(host.logs(), vec!["merge u1 1/1 integration check failed"]);
}

#[test]
fn an_already_integrated_batch_skips_all_checks_and_pushes() {
    let (_dir, ctx, host) = setup("cargo fmt --all");
    host.reply("merge-base --is-ancestor origin/work/", 0, "");
    host.reply("rev-parse origin/next", 0, "landed-tip\n");
    let results = batch_work(&ctx, &batch());
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|r| r.ok == Some(true)
        && r.tip.as_deref() == Some("landed-tip")
        && r.outside.is_empty()));
    assert!(!host.commands().iter().any(|c| c.contains(" merge --no-ff")
        || c.contains("push ")
        || c.contains("HEAD~1")
        || !c.starts_with("git ")));
}

#[test]
fn a_network_fetch_failure_is_reported_as_undecided_fetch_failed() {
    let (_dir, ctx, host) = setup("");
    host.reply("refs/heads/work/u1", 1, "fatal: could not fetch\n");
    let result = merge_work(&ctx, &job());
    assert_eq!(result.ok, None, "an infrastructure failure retries later");
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
fn a_missing_remote_branch_is_a_decided_branch_missing_failure() {
    let (_dir, ctx, host) = setup("");
    let text = "fatal: couldn't find remote ref refs/heads/work/u1\n";
    host.reply("refs/heads/work/u1", 128, text);
    let result = merge_work(&ctx, &job());
    assert_eq!(result.ok, Some(false), "retrying cannot make it appear");
    assert_eq!(result.why.as_deref(), Some("branch-missing"));
    assert_eq!(result.detail.as_deref(), Some(text));
    let batch = batch_work(&ctx, &batch());
    let u1 = for_uid(&batch, "u1");
    assert_eq!(u1.ok, Some(false));
    assert_eq!(u1.why.as_deref(), Some("branch-missing"));
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
    assert_eq!(index_of(&host.commands(), "merge --no-ff"), 6);
}

#[test]
fn a_lock_error_during_merge_is_an_env_failure_not_a_conflict() {
    let (_dir, ctx, host) = setup("");
    host.reply(
        "merge --no-ff",
        128,
        "fatal: Unable to create '.git/index.lock': File exists.\n",
    );
    let result = merge_work(&ctx, &job());
    assert_eq!(result.ok, None, "{result:?}");
    assert_eq!(result.why.as_deref(), Some("merge-env-failed"));
    let detail = result.detail.as_deref().unwrap_or_default();
    assert!(detail.contains("index.lock"), "{detail:?}");
    assert!(
        host.logs().iter().any(|l| l == "merge u1 1/1 env"),
        "{:?}",
        host.logs()
    );
}

#[test]
fn a_merge_failure_with_no_unmerged_files_is_an_env_failure() {
    let (_dir, ctx, host) = setup("");
    host.reply(
        "merge --no-ff",
        1,
        "fatal: refs/heads/work/u1 is not a commit\n",
    );
    let result = merge_work(&ctx, &job());
    assert_eq!(result.ok, None, "{result:?}");
    assert_eq!(result.why.as_deref(), Some("merge-env-failed"));
    let detail = result.detail.as_deref().unwrap_or_default();
    assert!(detail.contains("is not a commit"), "{detail:?}");
    assert!(
        host.logs().iter().any(|l| l == "merge u1 1/1 env"),
        "{:?}",
        host.logs()
    );
    assert_eq!(
        host.commands().last().map(String::as_str),
        Some("git reset -q --hard origin/next")
    );
}

/// A context whose merge config regenerates the e2e coverage ledger.
fn setup_generated() -> (TempDir, Ctx, Arc<FakeHost>) {
    let (_dir, mut ctx, host) = setup("");
    let mut cfg = (*ctx.cfg).clone();
    cfg.merge.generated.push(Generated {
        paths: vec![String::from("scripta/e2e-coverage-ledger.toml")],
        regenerate: String::from("./regen-ledger.sh"),
        drift: vec![],
    });
    ctx.cfg = Arc::new(cfg);
    (_dir, ctx, host)
}

#[test]
fn a_conflict_only_on_generated_files_is_resolved_and_merged() {
    let (_dir, ctx, host) = setup_generated();
    host.reply(
        "merge --no-ff",
        1,
        "Auto-merging scripta/e2e-coverage-ledger.toml\nCONFLICT (content): merge conflict in scripta/e2e-coverage-ledger.toml\n",
    );
    host.reply(
        "diff --name-only --diff-filter=U",
        0,
        "scripta/e2e-coverage-ledger.toml\n",
    );
    host.reply("rev-parse HEAD", 0, "tip123\n");
    let result = merge_work(&ctx, &job());
    assert_eq!(result.ok, Some(true), "{result:?}\n{:?}", host.commands());
    assert_eq!(result.tip.as_deref(), Some("tip123"));
    let cmds = host.commands();
    let merge = index_of(&cmds, "merge --no-ff");
    let checkout = index_of(
        &cmds,
        "git checkout --ours -- scripta/e2e-coverage-ledger.toml",
    );
    let regen = index_of(&cmds, "./regen-ledger.sh");
    let add = index_of(&cmds, "git add -- scripta/e2e-coverage-ledger.toml");
    let commit = cmds
        .iter()
        .position(|c| c.contains("commit --no-edit"))
        .unwrap();
    assert!(merge < checkout, "{cmds:?}");
    assert!(checkout < regen, "{cmds:?}");
    assert!(regen < add, "{cmds:?}");
    assert!(add < commit, "{cmds:?}");
    assert!(
        !cmds[commit + 1..]
            .iter()
            .any(|c| c.contains("reset -q --hard")),
        "{cmds:?}"
    );
    assert!(
        host.logs().iter().any(
            |l| l.contains("resolved generated conflicts u1: scripta/e2e-coverage-ledger.toml")
        )
    );
}

#[test]
fn a_conflict_on_generated_and_source_files_still_fails() {
    let (_dir, ctx, host) = setup_generated();
    host.reply(
        "merge --no-ff",
        1,
        "Auto-merging src/a.rs\nCONFLICT (content): merge conflict in src/a.rs\n",
    );
    host.reply(
        "diff --name-only --diff-filter=U",
        0,
        "src/a.rs\nscripta/e2e-coverage-ledger.toml\n",
    );
    let result = merge_work(&ctx, &job());
    assert_eq!(result.ok, Some(false));
    assert_eq!(result.why.as_deref(), Some("merge-conflict"));
    assert!(
        !host
            .commands()
            .iter()
            .any(|c| c.contains("checkout --ours"))
    );
    assert_eq!(
        host.commands().last().map(String::as_str),
        Some("git reset -q --hard origin/next")
    );
}

/// A context whose generated ledger carries a drift signature.
fn setup_drift() -> (TempDir, Ctx, Arc<FakeHost>) {
    let (dir, mut ctx, host) = setup("");
    let mut cfg = (*ctx.cfg).clone();
    cfg.merge.generated.push(Generated {
        paths: vec![String::from("scripta/e2e-coverage-ledger.toml")],
        regenerate: String::from("./regen-ledger.sh"),
        drift: vec![String::from("coverage_rows ")],
    });
    ctx.cfg = Arc::new(cfg);
    (dir, ctx, host)
}

#[test]
fn a_drift_signature_failure_regenerates_and_passes() {
    let (_dir, ctx, host) = setup_drift();
    host.reply_once("cargo test --lib", 1, "coverage_rows 810 != 811\n");
    host.reply("rev-parse HEAD", 0, "tip123\n");
    let result = merge_work(&ctx, &job());
    assert_eq!(result.ok, Some(true), "{result:?}\n{:?}", host.commands());
    let cmds = host.commands();
    assert!(
        cmds.iter().any(|c| c.contains("./regen-ledger.sh")),
        "{cmds:?}"
    );
    assert!(
        cmds.iter()
            .any(|c| c.contains("commit -q -m 'regen: scripta/e2e-coverage-ledger.toml after u1'")),
        "{cmds:?}"
    );
    assert_eq!(
        cmds.iter()
            .filter(|c| c.contains("cargo test --lib"))
            .count(),
        2,
        "{cmds:?}"
    );
    assert!(host.logs().iter().any(|l| {
        l.contains("drift u1: regenerated scripta/e2e-coverage-ledger.toml; retrying once")
    }));
}

#[test]
fn a_drift_retry_that_fails_again_is_proof_failed() {
    let (_dir, ctx, host) = setup_drift();
    host.reply_once("cargo test --lib", 1, "coverage_rows 810 != 811");
    host.reply("cargo test --lib", 1, "coverage_rows 810 != 811");
    let result = merge_work(&ctx, &job());
    assert_eq!(result.ok, Some(false));
    assert_eq!(result.why.as_deref(), Some("proof-failed"));
    let cmds = host.commands();
    assert_eq!(
        cmds.iter()
            .filter(|c| c.contains("./regen-ledger.sh"))
            .count(),
        1,
        "{cmds:?}"
    );
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
fn a_generated_path_does_not_count_as_outside() {
    let (_dir, mut ctx, host) = setup("");
    let mut cfg = (*ctx.cfg).clone();
    cfg.merge.generated.push(Generated {
        paths: vec![String::from("Cargo.lock")],
        regenerate: String::from("cargo generate-lockfile"),
        drift: vec![],
    });
    ctx.cfg = Arc::new(cfg);
    host.reply(
        "diff --name-only HEAD~1 HEAD",
        0,
        "src/pump/merge.rs\nREADME.md\nCargo.lock\n",
    );
    host.reply("rev-parse HEAD", 0, "tip123\n");
    let result = merge_work(&ctx, &job());
    assert_eq!(result.ok, Some(true), "{result:?}");
    assert_eq!(result.outside, vec![String::from("README.md")]);
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
        host.logs().get(2).map(String::as_str),
        Some("proof u1 rc=1 0s: cargo test --lib")
    );
    assert_eq!(
        host.commands().last().map(String::as_str),
        Some("git reset -q --hard origin/next")
    );
}

#[test]
fn a_proof_exit_127_leaves_the_unit_undecided() {
    let (_dir, ctx, host) = setup("");
    host.reply(
        "deno test x.test.ts",
        127,
        "bash: deno: command not found\n",
    );
    let mut unit = job();
    unit.proof = vec![String::from("deno test x.test.ts")];
    let result = merge_work(&ctx, &unit);
    assert_eq!(result.ok, None, "{result:?}\n{:?}", host.commands());
    assert_eq!(result.why.as_deref(), Some("proof-missing-tool"));
    assert_ne!(result.why.as_deref(), Some("proof-failed"));
    let detail = result.detail.unwrap_or_default();
    assert!(
        detail.contains("could not run (exit 127):\ndeno test x.test.ts"),
        "{detail}"
    );
    assert!(detail.contains("command not found"), "{detail}");
    assert_eq!(
        host.commands().last().map(String::as_str),
        Some("git reset -q --hard origin/next")
    );
}

#[test]
fn proof_exit_127_contrast_exit_1_is_still_a_decided_proof_failure() {
    let (_dir, ctx, host) = setup("");
    host.reply("deno test x.test.ts", 1, "1 test failed\n");
    let mut unit = job();
    unit.proof = vec![String::from("deno test x.test.ts")];
    let result = merge_work(&ctx, &unit);
    assert_eq!(result.ok, Some(false), "{result:?}");
    assert_eq!(result.why.as_deref(), Some("proof-failed"));
    let detail = result.detail.unwrap_or_default();
    assert!(
        detail.contains("this command failed (exit 1):\ndeno test x.test.ts"),
        "{detail}"
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
fn a_unit_proof_that_fails_as_a_usage_error_is_proof_usage() {
    let (_dir, ctx, host) = setup("");
    host.reply(
        "./proof-u1.sh",
        1,
        "error: unexpected argument 'an_event_applied_by_pull' found\n\nUsage: cargo test [OPTIONS] [TESTNAME] [-- [ARGS]...]\n",
    );
    let result = merge_work(&ctx, &job());
    assert_eq!(result.ok, Some(false));
    assert_eq!(result.why.as_deref(), Some("proof-usage"));
    let detail = result.detail.unwrap_or_default();
    assert!(detail.contains("./proof-u1.sh"));
    assert!(detail.contains("failed as a usage error before anything ran"));
    assert!(detail.contains("Last output:\nerror: unexpected argument"));
}

#[test]
fn a_shared_check_that_fails_as_a_usage_error_is_still_proof_failed() {
    let (_dir, ctx, host) = setup("");
    host.reply(
        "cargo test --lib",
        1,
        "error: unexpected argument 'an_event_applied_by_pull' found\n\nUsage: cargo test [OPTIONS] [TESTNAME] [-- [ARGS]...]\n",
    );
    let result = merge_work(&ctx, &job());
    assert_eq!(result.ok, Some(false));
    assert_eq!(result.why.as_deref(), Some("proof-failed"));
    assert!(
        result
            .detail
            .unwrap_or_default()
            .contains("this command failed")
    );
}

#[test]
fn a_skip_proof_kind_skips_its_own_proof() {
    let (_dir, mut ctx, host) = setup("");
    let mut cfg = (*ctx.cfg).clone();
    cfg.dispatch.kinds.insert(
        String::from("full"),
        crate::pump::config::Kind {
            pools: vec![String::from("p")],
            skip_proof: true,
            ..Default::default()
        },
    );
    ctx.cfg = Arc::new(cfg);
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
    let commit = index_of(&cmds, "commit -q -am 'style: format after u1'");
    let check = index_of(&cmds, "cargo test --lib");
    assert!(fmt < commit);
    assert!(commit < check);
    assert_eq!(
        cmds[commit],
        format!("git {IDENTITY} commit -q -am 'style: format after u1'")
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
pub(super) fn unit(uid: &str, branch: &str) -> MergeJob {
    MergeJob {
        uid: uid.to_string(),
        target: String::from("default"),
        branch: branch.to_string(),
        title: format!("Unit {uid}"),
        need: String::from("n1"),
        proof: vec![format!("./proof-{uid}.sh")],
        tools: String::from("plain"),
        write_scope: vec![
            String::from("src/pump/merge.rs"),
            String::from("src/pump/merge_test.rs"),
        ],
        source: None,
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
fn a_batch_touching_the_glob_runs_its_scoped_check_once() {
    let (_dir, ctx, host) = setup_scoped();
    host.reply(
        "diff --name-only HEAD~1 HEAD",
        0,
        "src/pump/merge.rs\nREADME.md\n",
    );
    host.reply("rev-parse HEAD", 0, "tip9\n");
    let mut touched = unit("u1", "work/u1");
    touched.write_scope = vec![String::from("corpus/x.fab")];
    let mut untouched = unit("u2", "work/u2");
    untouched.write_scope = vec![String::from("src/a.rs")];
    let results = batch_work(&ctx, &[touched, untouched]);
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|r| r.ok == Some(true)), "{results:?}");
    assert_eq!(
        results
            .iter()
            .filter(|r| r.tip.as_deref() == Some("tip9"))
            .count(),
        2
    );
    let cmds = host.commands();
    assert_eq!(count(&cmds, "./corpus-check.sh"), 1, "{cmds:?}");
    let scoped = index_of(&cmds, "./corpus-check.sh");
    assert!(index_of(&cmds, "merge u1") < scoped, "{cmds:?}");
    assert!(index_of(&cmds, "merge u2") < scoped, "{cmds:?}");
    assert!(scoped < index_of(&cmds, "./proof-u1.sh"), "{cmds:?}");
    assert!(
        !cmds.iter().any(|c| c.contains("./locale-check.sh")),
        "{cmds:?}"
    );
}

#[test]
fn a_batch_not_touching_the_glob_skips_its_scoped_check() {
    let (_dir, ctx, host) = setup_scoped();
    host.reply("rev-parse HEAD", 0, "tip9\n");
    let results = batch_work(&ctx, &batch());
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|r| r.ok == Some(true)), "{results:?}");
    let cmds = host.commands();
    assert_eq!(count(&cmds, "./corpus-check.sh"), 0, "{cmds:?}");
    assert!(
        !cmds.iter().any(|c| c.contains("./locale-check.sh")),
        "{cmds:?}"
    );
}

#[test]
fn a_failing_scoped_check_in_a_batch_fails_the_unit_back() {
    let (_dir, ctx, host) = setup_scoped();
    host.reply("./corpus-check.sh", 1, "error[E0308]: mismatched types\n");
    let mut solo = unit("u1", "work/u1");
    solo.write_scope = vec![String::from("corpus/x.fab")];
    let results = batch_work(&ctx, &[solo]);
    assert_eq!(results.len(), 1);
    let result = for_uid(&results, "u1");
    assert_eq!(result.ok, Some(false), "{result:?}");
    assert_eq!(result.why.as_deref(), Some("proof-failed"));
    let detail = result.detail.as_deref().unwrap_or_default();
    assert!(detail.contains("./corpus-check.sh"), "{detail:?}");
    assert!(
        !host.commands().iter().any(|c| c.contains("./proof-u1.sh")),
        "{:?}",
        host.commands()
    );
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
fn a_batch_lock_error_returns_one_unit_undecided_and_the_rest_merge() {
    let (_dir, ctx, host) = setup("");
    host.reply_once(
        "merge u1",
        128,
        "fatal: Unable to create '.git/index.lock': File exists.\n",
    );
    host.reply("diff --name-only HEAD~1 HEAD", 0, "src/pump/merge.rs\n");
    host.reply("rev-parse HEAD", 0, "tip7\n");
    let results = batch_work(&ctx, &batch());
    assert_eq!(results.len(), 2, "{results:?}");
    let u1 = for_uid(&results, "u1");
    assert_eq!(u1.ok, None, "{u1:?}");
    assert_eq!(u1.why.as_deref(), Some("merge-env-failed"));
    let detail = u1.detail.as_deref().unwrap_or_default();
    assert!(detail.contains("index.lock"), "{detail:?}");
    let u2 = for_uid(&results, "u2");
    assert_eq!(u2.ok, Some(true), "{u2:?}");
    assert_eq!(u2.tip.as_deref(), Some("tip7"));
    assert!(
        host.logs().iter().any(|l| l == "merge u1 1/2 env"),
        "{:?}",
        host.logs()
    );
}

#[test]
fn a_batch_conflict_only_on_generated_files_merges_both_units() {
    let (_dir, ctx, host) = setup_generated();
    host.reply("merge u1:", 0, "");
    host.reply(
        "merge u2:",
        1,
        "Auto-merging scripta/e2e-coverage-ledger.toml\nCONFLICT (content): merge conflict in scripta/e2e-coverage-ledger.toml\n",
    );
    host.reply(
        "diff --name-only --diff-filter=U",
        0,
        "scripta/e2e-coverage-ledger.toml\n",
    );
    host.reply("rev-parse HEAD", 0, "tip9\n");
    let results = batch_work(&ctx, &batch());
    assert_eq!(results.len(), 2);
    assert!(
        results
            .iter()
            .all(|r| r.ok == Some(true) && r.tip.as_deref() == Some("tip9")),
        "{results:?}\n{:?}",
        host.commands()
    );
    assert!(
        !results
            .iter()
            .any(|r| r.why.as_deref() == Some("merge-conflict"))
    );
    let cmds = host.commands();
    assert_eq!(count(&cmds, "./regen-ledger.sh"), 1, "{cmds:?}");
    let commit = cmds
        .iter()
        .position(|c| c.contains("commit --no-edit"))
        .unwrap();
    assert!(index_of(&cmds, "merge u2:") < commit, "{cmds:?}");
    assert!(index_of(&cmds, "./regen-ledger.sh") < commit, "{cmds:?}");
    // Each unit merged once: neither failed, so no attempt was consumed.
    assert_eq!(count(&cmds, "merge --no-ff"), 2, "{cmds:?}");
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
fn a_job_whose_own_proof_is_a_usage_error_is_the_culprit_with_proof_usage() {
    let (_dir, ctx, host) = setup("");
    host.reply(
        "./proof-u1.sh",
        1,
        "error: unexpected argument 'an_event_applied_by_pull' found\n\nUsage: cargo test [OPTIONS] [TESTNAME] [-- [ARGS]...]\n",
    );
    host.reply("diff --name-only HEAD~1 HEAD", 0, "src/pump/merge.rs\n");
    host.reply("rev-parse HEAD", 0, "tip5\n");
    let results = batch_work(&ctx, &batch());
    assert_eq!(results.len(), 2);
    let u1 = for_uid(&results, "u1");
    assert_eq!(u1.ok, Some(false));
    assert_eq!(u1.why.as_deref(), Some("proof-usage"));
    let detail = u1.detail.as_deref().unwrap_or_default();
    assert!(detail.contains("./proof-u1.sh"));
    assert!(detail.contains("failed as a usage error before anything ran"));
    let u2 = for_uid(&results, "u2");
    assert_eq!(u2.ok, Some(true));
    assert_eq!(u2.tip.as_deref(), Some("tip5"));
    let cmds = host.commands();
    assert_eq!(count(&cmds, "merge u2"), 2);
}

#[test]
fn a_shared_check_failure_with_several_jobs_quarantines_the_batch_not_one_at_a_time() {
    let (_dir, ctx, host) = setup("");
    host.reply("cargo test --lib", 1, "error[E0308]: mismatched types\n");
    let results = batch_work(&ctx, &batch());
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|r| r.ok.is_none()));
    assert!(
        results
            .iter()
            .all(|r| r.why.as_deref() == Some("quarantined"))
    );
    let q = for_uid(&results, "u1").quarantine.as_ref().unwrap();
    assert!(q.text.contains("error[E0308]: mismatched types"));
    // The batch is bisected (2 solo merges) and then rebuilt before the
    // quarantine push: 2 batch merges plus 2 bisect merges plus 2 rebuilds.
    let cmds = host.commands();
    assert_eq!(count(&cmds, "merge --no-ff"), 6);
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
    let logs = host.logs();
    assert_eq!(logs[0], "merge u1 1/1 ok");
    assert_eq!(logs[1], "check 1/1 start: cargo test --lib");
    assert_eq!(logs[2], "batch 1 shared rc=1 0s: cargo test --lib");
    assert_eq!(logs[3], "  error: boom");
    assert!(logs[4..].iter().all(|l| l.starts_with("  ")));
    let cmds = host.commands();
    assert!(cmds.iter().all(|c| !c.contains("./proof-u1.sh")));
    assert!(cmds.iter().all(|c| !c.contains("push")));
    assert_eq!(
        cmds.last().map(String::as_str),
        Some("git reset -q --hard origin/next")
    );
}

#[test]
fn a_batch_drift_signature_failure_regenerates_and_passes() {
    let (_dir, ctx, host) = setup_drift();
    host.reply_once("cargo test --lib", 1, "coverage_rows 810 != 811\n");
    host.reply("cargo test --lib", 0, "");
    host.reply("rev-parse HEAD", 0, "tip9\n");
    let results = batch_work(&ctx, &batch());
    assert_eq!(results.len(), 2);
    assert!(
        results.iter().all(|r| r.ok == Some(true)),
        "{results:?}\n{:?}",
        host.commands()
    );
    let cmds = host.commands();
    assert_eq!(count(&cmds, "./regen-ledger.sh"), 1, "{cmds:?}");
    assert!(
        results
            .iter()
            .all(|r| r.why.as_deref() != Some("quarantined"))
    );
    assert!(
        results
            .iter()
            .all(|r| r.why.as_deref() != Some("proof-failed"))
    );
    assert!(
        host.logs()
            .iter()
            .any(|l| l.contains("drift batch of 2: regenerated"))
    );
    assert!(
        host.logs()
            .iter()
            .any(|l| l == "batch 2 shared rc=0 0s: cargo test --lib")
    );
}

#[test]
fn a_batch_drift_retry_that_fails_again_still_fails() {
    let (_dir, ctx, host) = setup_drift();
    host.reply("cargo test --lib", 1, "coverage_rows 810 != 811\n");
    let results = batch_work(&ctx, &batch());
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|r| r.ok != Some(true)), "{results:?}");
    let cmds = host.commands();
    // The drift retry runs for the batch check and again for each member's
    // solo bisect check; all three still fail, so the batch is set aside.
    assert_eq!(count(&cmds, "./regen-ledger.sh"), 3, "{cmds:?}");
    assert!(
        results
            .iter()
            .all(|r| r.why.as_deref() == Some("quarantined"))
    );
    assert!(
        host.logs()
            .iter()
            .any(|l| l == "batch 2 shared rc=1 0s: cargo test --lib")
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

#[test]
fn missing_tool_shared_check_leaves_the_batch_undecided() {
    let (_dir, ctx, host) = setup("");
    host.reply("cargo test --lib", 127, "bash: cargo: command not found\n");
    let results = batch_work(&ctx, &batch());
    assert_eq!(results.len(), 2, "{results:?}");
    for result in &results {
        assert_eq!(result.ok, None, "{result:?}");
        assert_eq!(result.why.as_deref(), Some("proof-missing-tool"));
        assert!(result.quarantine.is_none(), "{result:?}");
    }
    let pushed = host
        .commands()
        .iter()
        .any(|c| c.contains("pump/quarantine"));
    assert!(!pushed, "a missing tool must not quarantine");
}

#[test]
fn a_batch_proof_exit_127_is_undecided() {
    let (_dir, ctx, host) = setup("");
    host.reply("deno test", 127, "bash: deno: command not found\n");
    let mut jobs = batch();
    jobs[1].proof = vec![String::from("deno test x.test.ts")];
    let results = batch_work(&ctx, &jobs);
    let u2 = for_uid(&results, "u2");
    assert_eq!(u2.ok, None, "{u2:?}\n{:?}", host.commands());
    assert_eq!(u2.why.as_deref(), Some("proof-missing-tool"));
    assert!(u2.quarantine.is_none(), "{u2:?}");
    let u1 = for_uid(&results, "u1");
    assert_eq!(u1.ok, Some(true), "{u1:?}\n{:?}", host.commands());
}

#[test]
fn a_batch_fetch_failure_is_undecided_and_does_not_fail_the_unit() {
    let (_dir, ctx, host) = setup("");
    host.reply("refs/heads/work/u1", 1, "fatal: could not fetch\n");
    let results = batch_work(&ctx, &batch());
    let u1 = for_uid(&results, "u1");
    assert_eq!(u1.ok, None);
    assert_eq!(u1.why.as_deref(), Some("fetch-failed"));
}

#[test]
fn every_commit_making_command_carries_the_configured_identity() {
    let (_dir, ctx, host) = setup("cargo fmt --all");
    host.reply("status --porcelain", 0, " M src/lib.rs\n");
    host.reply("rev-parse HEAD", 0, "tip\n");
    host.reply("merge-base --is-ancestor", 1, "");
    let _single = merge_work(&ctx, &job());
    let _batch = batch_work(&ctx, &batch());
    let commits: Vec<String> = host
        .commands()
        .into_iter()
        .filter(|c| c.contains(" merge --no-ff") || c.contains(" commit -q"))
        .collect();
    assert!(
        commits.len() >= 4,
        "expected merge and style commits: {commits:?}"
    );
    for cmd in &commits {
        assert!(cmd.starts_with(&format!("git {IDENTITY} ")), "{cmd}");
    }
    assert!(commits.iter().any(|c| c.contains(" commit -q")));
    assert!(commits.iter().any(|c| c.contains("merge main into next")));
}

/// Create `<repo>/.git/index.lock` with an mtime `age` seconds before now.
fn lock_file(ctx: &Ctx, age: u64) -> PathBuf {
    let git = ctx.repo.join(".git");
    std::fs::create_dir_all(&git).unwrap();
    let path = git.join("index.lock");
    let file = std::fs::File::options()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .unwrap();
    let at = SystemTime::now() - Duration::from_secs(age);
    file.set_times(std::fs::FileTimes::new().set_modified(at))
        .unwrap();
    path
}

/// Bring the fake clock onto real time, so lock mtimes are comparable.
fn set_real_clock(host: &FakeHost) -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    host.set_now(now);
    now
}

#[test]
fn a_stale_index_lock_is_cleared_and_logged() {
    let (_dir, ctx, host) = setup("");
    set_real_clock(&host);
    let path = lock_file(&ctx, 10 * 60);
    assert_eq!(clear_stale_lock(&ctx), Ok(()));
    assert!(!path.exists(), "the stale lock is gone");
    let last = host.logs().last().cloned().unwrap_or_default();
    assert!(last.contains("cleared stale index.lock"), "{last}");
}

#[test]
fn a_young_index_lock_is_left_alone_and_fails_the_check() {
    let (_dir, ctx, host) = setup("");
    set_real_clock(&host);
    let path = lock_file(&ctx, 0);
    let err = clear_stale_lock(&ctx).unwrap_err();
    assert!(err.contains("not cleared"), "{err}");
    assert!(path.exists(), "the young lock is kept: {err}");
    assert!(host.logs().is_empty(), "{:?}", host.logs());
}

#[test]
fn an_absent_index_lock_is_ok_and_creates_nothing() {
    let (_dir, ctx, host) = setup("");
    set_real_clock(&host);
    assert_eq!(clear_stale_lock(&ctx), Ok(()));
    assert!(!ctx.repo.join(".git/index.lock").exists());
    assert!(host.logs().is_empty(), "{:?}", host.logs());
}

/// A context whose merge config maps two scopes to check commands, on top of
/// the usual shared check.
fn setup_scoped() -> (TempDir, Ctx, Arc<FakeHost>) {
    let (dir, mut ctx, host) = setup("");
    let mut cfg = (*ctx.cfg).clone();
    cfg.merge.checks_for.insert(
        String::from("corpus/**"),
        vec![String::from("./corpus-check.sh")],
    );
    cfg.merge.checks_for.insert(
        String::from("locale/**"),
        vec![String::from("./locale-check.sh")],
    );
    ctx.cfg = Arc::new(cfg);
    (dir, ctx, host)
}

#[test]
fn scoped_checks_run_only_for_units_whose_scope_matches() {
    let (_dir, ctx, host) = setup_scoped();
    host.reply("rev-parse HEAD", 0, "tip123\n");
    let mut unit = job();
    unit.write_scope = vec![String::from("corpus/x.fab")];
    let result = merge_work(&ctx, &unit);
    assert_eq!(result.ok, Some(true), "{result:?}\n{:?}", host.commands());
    let cmds = host.commands();
    let shared = index_of(&cmds, "cargo test --lib");
    let scoped = index_of(&cmds, "./corpus-check.sh");
    let proof = index_of(&cmds, "./proof-u1.sh");
    assert!(shared < scoped && scoped < proof, "{cmds:?}");
    assert!(
        !cmds.iter().any(|c| c.contains("./locale-check.sh")),
        "{cmds:?}"
    );

    let (_dir, ctx, host) = setup_scoped();
    host.reply("rev-parse HEAD", 0, "tip123\n");
    let mut wide = job();
    wide.write_scope = vec![String::new()];
    let result = merge_work(&ctx, &wide);
    assert_eq!(result.ok, Some(true), "{result:?}\n{:?}", host.commands());
    let cmds = host.commands();
    let proof = index_of(&cmds, "./proof-u1.sh");
    assert!(index_of(&cmds, "./corpus-check.sh") < proof, "{cmds:?}");
    assert!(index_of(&cmds, "./locale-check.sh") < proof, "{cmds:?}");
}

#[test]
fn no_scoped_check_runs_when_no_scope_matches() {
    let (_dir, ctx, host) = setup_scoped();
    host.reply("rev-parse HEAD", 0, "tip123\n");
    let result = merge_work(&ctx, &job());
    assert_eq!(result.ok, Some(true), "{result:?}\n{:?}", host.commands());
    assert_eq!(
        host.commands(),
        vec![
            "git merge --abort",
            "git fetch origin next",
            "git checkout -q next",
            "git reset -q --hard origin/next",
            "git fetch origin refs/heads/work/u1:refs/remotes/origin/work/u1",
            "git merge-base --is-ancestor origin/work/u1 origin/next",
            &format!(
                "git {IDENTITY} merge --no-ff --no-edit -m 'merge u1: A unit (need n1)' origin/work/u1"
            )[..],
            "git diff --name-only HEAD~1 HEAD",
            "cargo test --lib",
            "./proof-u1.sh",
            "git status --porcelain",
            "git fetch -q origin next",
            "git merge-base --is-ancestor origin/next HEAD",
            "git push origin HEAD:refs/heads/next",
            "git rev-parse HEAD",
        ]
    );
}

#[test]
fn a_failing_scoped_check_is_proof_failed() {
    let (_dir, ctx, host) = setup_scoped();
    host.reply("./corpus-check.sh", 1, "error[E0308]: mismatched types\n");
    let mut unit = job();
    unit.write_scope = vec![String::from("corpus/x.fab")];
    let result = merge_work(&ctx, &unit);
    assert_eq!(result.ok, Some(false), "{result:?}");
    assert_eq!(result.why.as_deref(), Some("proof-failed"));
    let detail = result.detail.as_deref().unwrap_or_default();
    assert!(detail.contains("./corpus-check.sh"), "{detail:?}");
    assert!(
        !host.commands().iter().any(|c| c.contains("./proof-u1.sh")),
        "{:?}",
        host.commands()
    );
}
