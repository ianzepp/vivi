//! Creating the integration branch, over the fake host.

use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use super::ensure_all;
use crate::pump::config::PumpConfig;
use crate::pump::host::{Ctx, FakeHost, test_ctx};

const DEFAULT_TIP: &str = "1111111111111111111111111111111111111111";

fn setup(main: &str) -> (TempDir, Ctx, Arc<FakeHost>) {
    let dir = tempdir().unwrap();
    let cfg = PumpConfig {
        main: main.into(),
        ..PumpConfig::default()
    };
    let (ctx, host) = test_ctx(dir.path(), cfg);
    (dir, ctx, host)
}

fn remote_has_default_branch(host: &FakeHost) {
    host.reply(
        "ls-remote --symref",
        0,
        &format!("ref: refs/heads/main\tHEAD\n{DEFAULT_TIP}\tHEAD\n"),
    );
}

fn pushes(host: &FakeHost) -> Vec<String> {
    host.commands()
        .into_iter()
        .filter(|command| command.contains("git push"))
        .collect()
}

#[test]
fn a_missing_integration_branch_is_created_from_the_remote_default() {
    let (_dir, ctx, host) = setup("vivi/main");
    remote_has_default_branch(&host);
    ensure_all(&ctx).unwrap();
    let commands = host.commands();
    assert!(
        commands
            .iter()
            .any(|command| command.contains("git fetch origin main")),
        "{commands:?}"
    );
    assert_eq!(
        pushes(&host)
            .iter()
            .map(|command| command.rsplit("git ").next().unwrap().to_owned())
            .collect::<Vec<_>>(),
        ["push origin FETCH_HEAD:refs/heads/vivi/main"]
    );
}

#[test]
fn an_existing_integration_branch_is_left_alone() {
    let (_dir, ctx, host) = setup("vivi/main");
    host.reply(
        "ls-remote origin vivi/main",
        0,
        &format!("{DEFAULT_TIP}\trefs/heads/vivi/main\n"),
    );
    ensure_all(&ctx).unwrap();
    assert!(pushes(&host).is_empty());
    assert!(host.commands().iter().all(|c| !c.contains("--symref")));
}

#[test]
fn a_main_outside_the_pump_namespace_is_never_created() {
    let (_dir, ctx, host) = setup("develop");
    remote_has_default_branch(&host);
    ensure_all(&ctx).unwrap();
    assert!(pushes(&host).is_empty());
    assert!(host.commands().is_empty(), "{:?}", host.commands());
}

#[test]
fn an_empty_remote_has_nothing_to_branch_from() {
    let (_dir, ctx, host) = setup("vivi/main");
    host.reply("ls-remote --symref", 0, "");
    ensure_all(&ctx).unwrap();
    assert!(pushes(&host).is_empty());
}

#[test]
fn a_failed_push_is_reported_with_the_target() {
    let (_dir, ctx, host) = setup("vivi/main");
    remote_has_default_branch(&host);
    host.reply("git push", 1, "remote: permission denied");
    let error = ensure_all(&ctx).unwrap_err().to_string();
    assert!(error.contains("target default"), "{error}");
    assert!(error.contains("push vivi/main"), "{error}");
    assert!(error.contains("permission denied"), "{error}");
}
