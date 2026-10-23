//! `ref_contains` tests over the fake host.

use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use super::ref_contains;
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
    host.reply("merge-base --is-ancestor", 1, "");
    assert_eq!(ref_contains(&ctx, "default", REF, WANT), Ok(None));
}

#[test]
fn a_merge_base_error_is_an_error() {
    let (_dir, ctx, host) = setup();
    ls_remote(&host, TIP);
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
