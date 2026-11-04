//! `ref_contains` and `commit_contains` tests over the fake host.

use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use super::{commit_contains, ref_contains};
use crate::pump::config::PumpConfig;
use crate::pump::host::{Ctx, FakeHost, test_ctx};

const TIP: &str = "2222222222222222222222222222222222222222";
const WANT: &str = "3333333333333333333333333333333333333333";
const REF: &str = "refs/heads/vivi/need/n1";

fn setup() -> (TempDir, Ctx, Arc<FakeHost>) {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    (dir, ctx, host)
}

fn ls_remote(host: &FakeHost, sha: &str) {
    host.reply("ls-remote", 0, &format!("{sha}\t{REF}\n"));
}

#[test]
fn a_missing_ref_contains_nothing() {
    let (_dir, ctx, host) = setup();
    host.reply("ls-remote", 0, "");
    assert_eq!(ref_contains(&ctx, "default", REF, WANT), Ok(None));
}

#[test]
fn a_tip_equal_to_the_sha_is_contained_without_a_fetch() {
    let (_dir, ctx, host) = setup();
    ls_remote(&host, TIP);
    assert_eq!(
        ref_contains(&ctx, "default", REF, TIP),
        Ok(Some(TIP.to_owned()))
    );
    assert!(host.commands().iter().all(|c| !c.contains("fetch")));
}

#[test]
fn an_ancestor_of_the_tip_is_contained() {
    let (_dir, ctx, host) = setup();
    ls_remote(&host, TIP);
    host.reply("cat-file -e", 0, "");
    host.reply("merge-base --is-ancestor", 0, "");
    assert_eq!(
        ref_contains(&ctx, "default", REF, WANT),
        Ok(Some(TIP.to_owned()))
    );
    assert!(host.commands().iter().any(|c| c.contains("fetch")));
}

#[test]
fn a_moved_ref_without_the_commit_is_a_plain_no() {
    let (_dir, ctx, host) = setup();
    ls_remote(&host, TIP);
    host.reply("cat-file -e", 0, "");
    host.reply("merge-base --is-ancestor", 1, "");
    assert_eq!(ref_contains(&ctx, "default", REF, WANT), Ok(None));
}

#[test]
fn a_commit_absent_after_the_fetch_is_not_contained() {
    let (_dir, ctx, host) = setup();
    ls_remote(&host, TIP);
    host.reply("cat-file -e", 128, "fatal: Not a valid object name");
    assert_eq!(ref_contains(&ctx, "default", REF, WANT), Ok(None));
    assert!(host.commands().iter().all(|c| !c.contains("merge-base")));
}

#[test]
fn a_merge_base_error_is_an_error() {
    let (_dir, ctx, host) = setup();
    ls_remote(&host, TIP);
    host.reply("cat-file -e", 0, "");
    host.reply("merge-base --is-ancestor", 128, "fatal: bad object");
    let err = ref_contains(&ctx, "default", REF, WANT).unwrap_err();
    assert!(err.contains("merge-base"), "{err}");
}

#[test]
fn a_failed_fetch_is_an_error() {
    let (_dir, ctx, host) = setup();
    ls_remote(&host, TIP);
    host.reply("fetch", 1, "no route");
    let err = ref_contains(&ctx, "default", REF, WANT).unwrap_err();
    assert!(err.contains("fetch"), "{err}");
}

#[test]
fn an_unknown_target_or_bad_sha_is_an_error() {
    let (_dir, ctx, _host) = setup();
    assert!(ref_contains(&ctx, "nope", REF, WANT).is_err());
    assert!(ref_contains(&ctx, "default", REF, "not-a-sha").is_err());
}

#[test]
fn commit_contains_equal_commits_need_no_git() {
    let (_dir, ctx, host) = setup();
    assert_eq!(commit_contains(&ctx, "default", REF, TIP, TIP), Ok(true));
    assert!(host.commands().is_empty());
}

#[test]
fn commit_contains_an_ancestor_is_true() {
    let (_dir, ctx, host) = setup();
    host.reply("cat-file -e", 0, "");
    host.reply("merge-base --is-ancestor", 0, "");
    assert_eq!(commit_contains(&ctx, "default", REF, TIP, WANT), Ok(true));
    assert!(host.commands().iter().any(|c| c.contains("fetch")));
}

#[test]
fn commit_contains_a_non_ancestor_is_false() {
    let (_dir, ctx, host) = setup();
    host.reply("cat-file -e", 0, "");
    host.reply("merge-base --is-ancestor", 1, "");
    assert_eq!(commit_contains(&ctx, "default", REF, TIP, WANT), Ok(false));
}

#[test]
fn commit_contains_an_absent_commit_is_an_error() {
    let (_dir, ctx, host) = setup();
    host.reply("cat-file -e", 128, "fatal: Not a valid object name");
    let err = commit_contains(&ctx, "default", REF, TIP, WANT).unwrap_err();
    assert!(err.contains("not in the local object store"), "{err}");
    assert!(host.commands().iter().all(|c| !c.contains("merge-base")));
}

#[test]
fn commit_contains_git_failures_and_bad_input_are_errors() {
    let (_dir, ctx, host) = setup();
    host.reply("fetch", 1, "no route");
    let err = commit_contains(&ctx, "default", REF, TIP, WANT).unwrap_err();
    assert!(err.contains("fetch"), "{err}");
    assert!(commit_contains(&ctx, "nope", REF, TIP, WANT).is_err());
    assert!(commit_contains(&ctx, "default", REF, TIP, "not-a-sha").is_err());

    let (_dir, ctx, host) = setup();
    host.reply("cat-file -e", 0, "");
    host.reply("merge-base --is-ancestor", 128, "fatal: bad object");
    let err = commit_contains(&ctx, "default", REF, TIP, WANT).unwrap_err();
    assert!(err.contains("merge-base"), "{err}");
}
