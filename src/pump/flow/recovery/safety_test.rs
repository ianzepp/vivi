//! Publication-safety gate tests over the fake host.

use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use super::{RecoverySafety, code, decide};
use crate::pump::config::PumpConfig;
use crate::pump::flow::recovery::{EMPTY, PROVIDER, RUNNER, transient_class};
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::{JobOutcome, JobSpec, JobStatus, PoolJob};

const BEFORE: &str = "1111111111111111111111111111111111111111";
const TIP: &str = "2222222222222222222222222222222222222222";
const WANT: &str = "3333333333333333333333333333333333333333";

fn setup() -> (TempDir, Ctx, Arc<FakeHost>) {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    (dir, ctx, host)
}

/// A finished job with a reconcile detail and a collected receipt.
fn finished(id: &str, detail: &str, receipt: &str) -> PoolJob {
    let mut job = PoolJob::from(JobSpec::new(id, "hand", "hand", "subject", "body"));
    job.status = JobStatus::Finished;
    job.outcome = Some(JobOutcome {
        state: String::from("finished"),
        detail: detail.to_owned(),
        receipt: receipt.to_owned(),
        at: 7,
    });
    job
}

/// An effectful step job on the default target's main ref: it starts from
/// BEFORE and its work is the WANT commit.
fn effectful(id: &str, detail: &str, receipt: &str) -> PoolJob {
    let mut job = finished(id, detail, receipt);
    job.target = Some(String::from("default"));
    job.from_ref = Some(BEFORE.to_owned());
    job.publish_ref = Some(String::from("refs/heads/main"));
    job.publish_contains = Some(WANT.to_owned());
    job
}

/// A plain collected receipt: an empty reply and empty stderr.
fn plain_receipt() -> String {
    receipt("", "")
}

/// A receipt as the pool writes it: summary lines, then sections.
fn receipt(message: &str, stderr: &str) -> String {
    format!(
        "pool result j1: finished\nagent exit 0 in 3s, job 9s\n\n\
         --- receipt\n{message}\n\n--- stderr tail\n{stderr}\n\n--- runner log tail\nlog line"
    )
}

/// A receipt whose runner pushed WANT to the job's ref (reported at TIP),
/// with the given reply and stderr sections.
fn published_receipt(message: &str, stderr: &str) -> String {
    format!(
        "pool result j1: finished\nagent exit 0 in 3s, job 9s\n\
         runner publish: pushed refs/heads/main {WANT} {TIP}\n\
         runner contains: {TIP} 1\n\n\
         --- receipt\n{message}\n\n--- stderr tail\n{stderr}\n\n--- runner log tail\nlog line"
    )
}

#[test]
fn effectful_transient_recovery_requires_safe_absence() {
    let (_dir, ctx, host) = setup();

    // Pre-execution failure: a runner fetch failure means the agent never
    // ran, so the rerun is safe without consulting git at all.
    let failed = effectful(
        "step-t-a1",
        "fetch_failed could not read from remote",
        &receipt("", "fatal: could not read from remote repository"),
    );
    assert_eq!(transient_class(&failed), Some(RUNNER));
    assert_eq!(
        decide(&ctx, &failed, RUNNER),
        RecoverySafety::Safe {
            code: code::PRE_EXECUTION
        }
    );
    assert!(host.commands().is_empty());

    // Authoritative publication: the runner pushed, so the genuinely empty
    // reply is a lost answer — normal collection owns the step, never a
    // replacement.
    let empty_after_push = effectful(
        "step-t-a1",
        "finished push=pushed",
        &published_receipt("", ""),
    );
    assert_eq!(transient_class(&empty_after_push), Some(EMPTY));
    assert_eq!(
        decide(&ctx, &empty_after_push, EMPTY),
        RecoverySafety::Published {
            code: code::PUBLISHED_BY_RECEIPT
        }
    );

    // Provider diagnostics beside a real push are diagnostics, not evidence
    // of a lost effect: the receipt still decides.
    let noisy = effectful(
        "step-t-a1",
        "finished push=pushed",
        &published_receipt("answer = \"built\"", "429: rate limit reached"),
    );
    assert_eq!(transient_class(&noisy), Some(PROVIDER));
    assert_eq!(
        decide(&ctx, &noisy, PROVIDER),
        RecoverySafety::Published {
            code: code::PUBLISHED_BY_RECEIPT
        }
    );

    // A correction of the same input whose work already published is the
    // same story: the receipt decides, the correction never reruns.
    let correction = effectful(
        "step-t-tr1-c1",
        "finished push=pushed",
        &published_receipt("", ""),
    );
    assert_eq!(
        decide(&ctx, &correction, EMPTY),
        RecoverySafety::Published {
            code: code::PUBLISHED_BY_RECEIPT
        }
    );

    // Verified absence: no publication evidence at all, and git says the ref
    // still sits at the job's start point — the rerun cannot duplicate work.
    host.reply("ls-remote", 0, &format!("{BEFORE}\trefs/heads/main\n"));
    let absent = effectful("step-t-a1", "", &plain_receipt());
    assert_eq!(
        decide(&ctx, &absent, EMPTY),
        RecoverySafety::Safe {
            code: code::VERIFIED_ABSENCE
        }
    );
    assert!(
        host.commands()
            .iter()
            .all(|command| !command.contains("fetch"))
    );

    // Uncertain remote evidence: git cannot settle the ref, so the decision
    // keeps the existing side-effect-unclear path instead of rerunning.
    host.reply("ls-remote", 128, "fatal: unable to access");
    assert_eq!(
        decide(&ctx, &absent, EMPTY),
        RecoverySafety::Unclear {
            code: code::GIT_UNCLEAR
        }
    );

    // A landing git can see without a receipt is not absence either: the
    // ref moved and holds the work, so collection owns it.
    host.reply("ls-remote", 0, &format!("{TIP}\trefs/heads/main\n"));
    host.reply("merge-base --is-ancestor", 0, "");
    assert_eq!(
        decide(&ctx, &absent, EMPTY),
        RecoverySafety::Published {
            code: code::PUBLISHED_IN_GIT
        }
    );

    // A ref that moved without the work is unclear, not absent.
    host.reply("merge-base --is-ancestor", 1, "");
    assert_eq!(
        decide(&ctx, &absent, EMPTY),
        RecoverySafety::Unclear {
            code: code::GIT_UNCLEAR
        }
    );
}

#[test]
fn requests_that_cannot_publish_need_no_remote_evidence() {
    let (_dir, ctx, host) = setup();

    // A read-only request cannot have published anything: its transient
    // failures recover without a receipt or a git query.
    let mut research = finished("step-r-a1", "", &plain_receipt());
    research.read_only = true;
    assert_eq!(
        decide(&ctx, &research, EMPTY),
        RecoverySafety::Safe {
            code: code::READ_ONLY
        }
    );

    // An effectful request without a publication ref had nothing the
    // privileged runner could publish: safe, and still no git query.
    let mut unpublished = effectful("step-u-a1", "", &plain_receipt());
    unpublished.publish_ref = None;
    assert_eq!(
        decide(&ctx, &unpublished, EMPTY),
        RecoverySafety::Safe {
            code: code::NO_PUBLICATION_REF
        }
    );

    assert!(host.commands().is_empty());
}
