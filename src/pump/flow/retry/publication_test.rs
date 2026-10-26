//! Publication gate tests over the fake host: a decisive receipt prevents a
//! duplicate execution, git keeps its verified/absent/unclear distinction,
//! and nonpublishing requests never touch git.

use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use super::{Evidence, Publication, gate};
use crate::pump::config::PumpConfig;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::{JobOutcome, JobSpec, JobStatus, PoolJob};

/// The base a fixture job started from.
const BEFORE: &str = "1111111111111111111111111111111111111111";
/// The candidate SHA a fixture runner receipt names.
const CAND: &str = "2222222222222222222222222222222222222222";
/// The commit a fixture merge must contain.
const WANT: &str = "3333333333333333333333333333333333333333";
/// The remote tip git reports for a fixture ref.
const TIP: &str = "4444444444444444444444444444444444444444";

/// The unit branch of one fixture hand attempt.
const HAND_REF: &str = "refs/heads/pump/unit-slice-a1";
/// The attempt branch of a fixture repairer.
const FIX_ONE: &str = "refs/heads/pump/repair-attempt-1";
/// The fresh attempt branch of the fixture repairer's rerun.
const FIX_TWO: &str = "refs/heads/pump/repair-attempt-2";

fn setup() -> (TempDir, Ctx, Arc<FakeHost>) {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    (dir, ctx, host)
}

/// A finished job with `receipt` as its pool receipt.
fn outcome(state: &str, receipt: &str) -> JobOutcome {
    JobOutcome {
        state: state.to_owned(),
        detail: String::from("step exit 0"),
        receipt: receipt.to_owned(),
        at: 0,
    }
}

/// The runner's publish line in a pool receipt.
fn push_receipt(state: &str, reference: &str) -> String {
    format!(
        "agent exit 0 in 9s\nrunner publish: {state} {reference} {CAND} {TIP}\n\n--- receipt\nDone.\n"
    )
}

/// A hand job for its own unit branch, cut from `BEFORE`.
fn hand() -> PoolJob {
    let mut job = PoolJob::from(JobSpec::new("step-x", "slice", "hand", "s", "b"));
    job.status = JobStatus::Finished;
    job.target = Some(String::from("default"));
    job.from_ref = Some(BEFORE.to_owned());
    job.publish_ref = Some(HAND_REF.to_owned());
    job.branch = Some(String::from("pump/unit-slice-a1"));
    job
}

/// A repairer job publishing the attempt branch `reference`, cut from the
/// main tip `BEFORE`.
fn repairer(reference: &str) -> PoolJob {
    let mut job = PoolJob::from(JobSpec::new("step-x", "slice", "repairer", "s", "b"));
    job.status = JobStatus::Finished;
    job.target = Some(String::from("default"));
    job.from_ref = Some(BEFORE.to_owned());
    job.publish_ref = Some(reference.to_owned());
    job.branch = Some(
        reference
            .strip_prefix("refs/heads/")
            .unwrap_or(reference)
            .to_owned(),
    );
    job
}

/// A merge job publishing `refs/heads/main` with `WANT` as its ancestry
/// anchor.
fn merge() -> PoolJob {
    let mut job = PoolJob::from(JobSpec::new("merge-x", "merge", "merge", "s", "b"));
    job.status = JobStatus::Finished;
    job.target = Some(String::from("default"));
    job.from_ref = Some(BEFORE.to_owned());
    job.publish_ref = Some(String::from("refs/heads/main"));
    job.publish_contains = Some(WANT.to_owned());
    job
}

fn ls_remote(host: &FakeHost, reference: &str, sha: &str) {
    host.reply("ls-remote", 0, &format!("{sha}\t{reference}\n"));
}

fn git_settles(host: &FakeHost) {
    host.reply("fetch", 0, "");
    host.reply("merge-base --is-ancestor", 0, "");
}

fn no_git_ran(host: &FakeHost) -> bool {
    host.commands().iter().all(|cmd| !cmd.starts_with("git "))
}

/// Whether the git ancestry checks — fetch and merge-base — never ran: an
/// unmoved ref is answered from the ls-remote alone.
fn no_ancestry_check_ran(host: &FakeHost) -> bool {
    host.commands()
        .iter()
        .all(|cmd| !cmd.contains("fetch") && !cmd.contains("merge-base"))
}

fn unclear_reason(effect: Publication) -> String {
    match effect {
        Publication::Unclear { reason } => reason,
        other => panic!("expected Unclear, got {other:?}"),
    }
}

/// The brief's named sweep: for a publishing job, one successful receipt,
/// a git-verified ref, an absent effect, a conflicting ref and an
/// unavailable git each answer exactly once, so no replay executes the
/// work again.
#[test]
fn publication_evidence_prevents_duplicate_execution() {
    // A published hand: the decisive receipt wins and git is not consulted.
    {
        let (_dir, ctx, host) = setup();
        let mut job = hand();
        job.outcome = Some(outcome("finished", &push_receipt("pushed", HAND_REF)));
        assert_eq!(
            gate(&ctx, &job),
            Publication::Published {
                sha: TIP.into(),
                via: Evidence::Receipt
            }
        );
        assert!(no_git_ran(&host));
    }
    // A merge that already landed: its own receipt closes it without
    // executing the merge again.
    {
        let (_dir, ctx, host) = setup();
        let mut job = merge();
        job.outcome = Some(outcome(
            "finished",
            &push_receipt("pushed", "refs/heads/main"),
        ));
        assert_eq!(
            gate(&ctx, &job),
            Publication::Published {
                sha: TIP.into(),
                via: Evidence::Receipt
            }
        );
        assert!(no_git_ran(&host));
    }
    // A hand whose receipt did not succeed: git decides from the receipt's
    // candidate and verifies the ancestry.
    {
        let (_dir, ctx, host) = setup();
        let mut job = hand();
        job.outcome = Some(outcome("finished", &push_receipt("cas_failed", HAND_REF)));
        ls_remote(&host, HAND_REF, TIP);
        git_settles(&host);
        assert_eq!(
            gate(&ctx, &job),
            Publication::Published {
                sha: TIP.into(),
                via: Evidence::Git
            }
        );
    }
    // An isolated repair branch that did not move: absent effect, replay
    // cannot duplicate work.
    {
        let (_dir, ctx, host) = setup();
        let mut job = repairer(FIX_ONE);
        job.outcome = Some(outcome("lost", &push_receipt("cas_failed", FIX_ONE)));
        ls_remote(&host, FIX_ONE, BEFORE);
        assert_eq!(gate(&ctx, &job), Publication::Absent);
        assert!(no_ancestry_check_ran(&host));
    }
    // A moved ref without the anchored commit: conflicting evidence, held.
    {
        let (_dir, ctx, host) = setup();
        ls_remote(&host, "refs/heads/main", TIP);
        host.reply("fetch", 0, "");
        host.reply("merge-base --is-ancestor", 1, "");
        let reason = unclear_reason(gate(&ctx, &merge()));
        assert!(reason.contains("without"), "{reason}");
    }
    // Git cannot answer at all: held, never replayed on a guess.
    {
        let (_dir, ctx, host) = setup();
        host.reply("ls-remote", 128, "fatal: unable to access");
        let reason = unclear_reason(gate(&ctx, &merge()));
        assert!(reason.contains("ls-remote"), "{reason}");
    }
}

/// A hand whose pool lost the job after the push, leaving no final prose:
/// the receipt still publishes, so the slice merge proceeds without a
/// duplicate execution and without a contentless envelope-fix request.
#[test]
fn a_published_hand_with_no_final_prose_merges_without_rerunning() {
    let (_dir, ctx, host) = setup();
    let mut job = hand();
    job.status = JobStatus::Lost;
    job.outcome = Some(outcome(
        "lost",
        &format!(
            "agent exit 0 in 9s\nrunner publish: pushed {HAND_REF} {CAND} {TIP}\n\n--- receipt\n\n"
        ),
    ));
    assert_eq!(
        gate(&ctx, &job),
        Publication::Published {
            sha: TIP.into(),
            via: Evidence::Receipt
        }
    );
    assert!(no_git_ran(&host));
}

/// A job that publishes nothing — no publish ref — is not a publication
/// question: the gate answers None and runs no git verification.
#[test]
fn nonpublishing_requests_do_not_perform_git_verification() {
    let (_dir, ctx, host) = setup();
    let mut job = PoolJob::from(JobSpec::new("step-x", "slice", "researcher", "s", "b"));
    job.status = JobStatus::Finished;
    job.target = Some(String::from("default"));
    job.outcome = Some(outcome(
        "finished",
        "agent exit 1 in 9s\n--- stderr tail\nprovider: connection refused\n",
    ));
    assert_eq!(gate(&ctx, &job), Publication::None);
    assert!(no_git_ran(&host));
}

/// A publishing job with neither a receipt nor an ancestry anchor: git
/// could not prove the publish absent or present, so the job is held for
/// the operator instead of replayed.
#[test]
fn a_publishing_job_with_no_evidence_is_held_not_replayed() {
    let (_dir, ctx, host) = setup();
    let job = hand();
    let reason = unclear_reason(gate(&ctx, &job));
    assert!(reason.contains("no commit to verify"), "{reason}");
    assert!(no_git_ran(&host));
}

/// One repair attempt's success never releases another attempt's replay:
/// a receipt naming the previous attempt's branch is not decisive, and the
/// rerun's own branch decides through git — where an unmoved ref permits
/// the isolated retry.
#[test]
fn another_attempts_receipt_never_releases_a_repair_replay() {
    let (_dir, ctx, host) = setup();
    let mut job = repairer(FIX_TWO);
    job.outcome = Some(outcome("finished", &push_receipt("pushed", FIX_ONE)));
    ls_remote(&host, FIX_TWO, BEFORE);
    assert_eq!(gate(&ctx, &job), Publication::Absent);
    // Its own ref's success is decisive without any git.
    let (_dir, ctx, host) = setup();
    let mut job = repairer(FIX_TWO);
    job.outcome = Some(outcome("finished", &push_receipt("pushed", FIX_TWO)));
    assert_eq!(
        gate(&ctx, &job),
        Publication::Published {
            sha: TIP.into(),
            via: Evidence::Receipt
        }
    );
    assert!(no_git_ran(&host));
}
