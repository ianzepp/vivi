//! Side-effect verification tests over the fake host.

use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use super::{RunnerPublishReceipt, SideEffect, VerifyReq, check};
use crate::pump::config::PumpConfig;
use crate::pump::host::{Ctx, FakeHost, test_ctx};

const BEFORE: &str = "1111111111111111111111111111111111111111";
const TIP: &str = "2222222222222222222222222222222222222222";
const WANT: &str = "3333333333333333333333333333333333333333";

fn setup() -> (TempDir, Ctx, Arc<FakeHost>) {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    (dir, ctx, host)
}

fn req(receipt: Option<&str>) -> VerifyReq {
    VerifyReq {
        target: String::from("default"),
        remote_ref: String::from("main"),
        before: Some(BEFORE.to_owned()),
        must_contain: WANT.to_owned(),
        receipt: receipt.map(|sha| RunnerPublishReceipt {
            sha: sha.to_owned(),
        }),
    }
}

fn ls_remote(host: &FakeHost, sha: &str) {
    host.reply("ls-remote", 0, &format!("{sha}\trefs/heads/main\n"));
}

fn unclear_reason(effect: SideEffect) -> String {
    match effect {
        SideEffect::Unclear { reason } => reason,
        other => panic!("expected Unclear, got {other:?}"),
    }
}

#[test]
fn a_receipt_sha_at_the_ref_tip_is_verified() {
    let (_dir, ctx, host) = setup();
    ls_remote(&host, TIP);
    assert_eq!(
        check(&ctx, &req(Some(TIP))),
        SideEffect::Verified { sha: TIP.into() }
    );
    assert!(host.commands().iter().all(|c| !c.contains("fetch")));
}

#[test]
fn a_receipt_sha_behind_the_ref_tip_is_verified_after_a_fetch() {
    let (_dir, ctx, host) = setup();
    ls_remote(&host, TIP);
    host.reply("merge-base --is-ancestor", 0, "");
    assert_eq!(
        check(&ctx, &req(Some(WANT))),
        SideEffect::Verified { sha: WANT.into() }
    );
    let commands = host.commands();
    assert!(commands.iter().any(|c| c.contains("fetch")));
    assert!(
        commands
            .iter()
            .any(|c| c.contains(&format!("merge-base --is-ancestor {WANT} {TIP}")))
    );
}

#[test]
fn no_receipt_and_an_unmoved_ref_is_absent() {
    let (_dir, ctx, host) = setup();
    ls_remote(&host, BEFORE);
    assert_eq!(check(&ctx, &req(None)), SideEffect::Absent);
}

#[test]
fn no_receipt_and_a_moved_ref_containing_the_commit_is_verified_at_the_ref() {
    let (_dir, ctx, host) = setup();
    ls_remote(&host, TIP);
    host.reply("merge-base --is-ancestor", 0, "");
    assert_eq!(
        check(&ctx, &req(None)),
        SideEffect::Verified { sha: TIP.into() }
    );
}

#[test]
fn no_receipt_and_a_moved_ref_without_the_commit_is_unclear() {
    let (_dir, ctx, host) = setup();
    ls_remote(&host, TIP);
    host.reply("merge-base --is-ancestor", 1, "");
    let reason = unclear_reason(check(&ctx, &req(None)));
    assert!(reason.contains("without"), "{reason}");
}

#[test]
fn a_git_failure_is_unclear_never_absent() {
    let (_dir, ctx, host) = setup();
    host.reply("ls-remote", 128, "fatal: unable to access");
    let reason = unclear_reason(check(&ctx, &req(None)));
    assert!(reason.contains("ls-remote"), "{reason}");
}

#[test]
fn a_timeout_is_unclear() {
    let (_dir, ctx, host) = setup();
    host.reply("ls-remote", 124, "[timeout after 30s]");
    unclear_reason(check(&ctx, &req(None)));
}

#[test]
fn a_failed_fetch_or_ancestry_query_is_unclear() {
    let (_dir, ctx, host) = setup();
    ls_remote(&host, TIP);
    host.reply("fetch", 1, "no route");
    unclear_reason(check(&ctx, &req(None)));
    host.reply("fetch", 0, "");
    host.reply("merge-base --is-ancestor", 128, "bad object");
    unclear_reason(check(&ctx, &req(None)));
}

#[test]
fn a_receipt_the_ref_does_not_show_never_reads_as_absent() {
    let (_dir, ctx, host) = setup();
    ls_remote(&host, BEFORE);
    host.reply("merge-base --is-ancestor", 1, "");
    let reason = unclear_reason(check(&ctx, &req(Some(TIP))));
    assert!(reason.contains("did not move"), "{reason}");
}

#[test]
fn a_receipt_off_the_ref_still_verifies_from_git_when_the_ref_holds_the_commit() {
    let (_dir, ctx, host) = setup();
    ls_remote(&host, TIP);
    host.reply_once("merge-base --is-ancestor", 1, "");
    host.reply("merge-base --is-ancestor", 0, "");
    assert_eq!(
        check(&ctx, &req(Some(BEFORE))),
        SideEffect::Verified { sha: TIP.into() }
    );
}

#[test]
fn a_missing_ref_with_no_before_is_absent_and_with_a_before_is_unclear() {
    let (_dir, ctx, host) = setup();
    host.reply("ls-remote", 0, "");
    let mut request = req(None);
    request.before = None;
    assert_eq!(check(&ctx, &request), SideEffect::Absent);
    unclear_reason(check(&ctx, &req(None)));
}

#[test]
fn an_unknown_target_or_bad_sha_is_unclear() {
    let (_dir, ctx, _host) = setup();
    let mut request = req(None);
    request.target = String::from("nope");
    unclear_reason(check(&ctx, &request));
    let mut request = req(None);
    request.must_contain = String::from("main; rm -rf /");
    unclear_reason(check(&ctx, &request));
}
