use super::*;
use crate::pool::doctor::Status;

const PROBE: &str = "user=ci\nhost=build-box\nsystem=Linux x86_64\nroot=/home/ci/vivi-pool\nroot_exists=1\nroot_writable=1\nroot_anchor=/home/ci/vivi-pool\nfree_kb=52428800\n";

fn facts() -> SshFacts {
    SshFacts {
        host: "ci@build-box".into(),
        root: "~/vivi-pool".into(),
        harness_env: 0,
        unset_env: Vec::new(),
        client: Ok("OpenSSH_9.6".into()),
        probe: Some(Ok(Probe::parse(PROBE))),
    }
}

fn check<'a>(list: &'a [Check], name: &str) -> &'a Check {
    list.iter().find(|c| c.name == name).unwrap()
}

fn with(change: impl FnOnce(&mut Probe)) -> SshFacts {
    let mut f = facts();
    if let Some(Ok(p)) = f.probe.as_mut() {
        change(p);
    }
    f
}

#[test]
fn a_healthy_host_passes_every_check_and_reports_user_system_and_space() {
    let list = checks(&facts());
    assert!(list.iter().all(|c| c.status == Status::Ok), "{list:?}");
    assert!(check(&list, "host").detail.contains("ci@build-box"));
    assert_eq!(check(&list, "arch").detail, "Linux x86_64");
    assert!(check(&list, "disk").detail.starts_with("50.0 GiB free"));
    assert!(list.iter().all(|c| c.group == GROUP));
}

#[test]
fn the_probe_text_is_read_field_by_field() {
    let p = Probe::parse(&format!("{PROBE}missing=git\nmissing=setsid\nnoise\n"));
    assert_eq!(p.user, "ci");
    assert_eq!(p.system, "Linux x86_64");
    assert_eq!(p.missing, ["git", "setsid"]);
    assert!(p.root_exists && p.root_writable);
    assert_eq!(p.free_kb, Some(52_428_800));
}

#[test]
fn an_unreachable_host_fails_with_the_non_interactive_check_and_skips_the_rest() {
    let mut f = facts();
    f.probe = Some(Err("Permission denied (publickey)".into()));
    let list = checks(&f);
    let host = check(&list, "host");
    assert_eq!(host.status, Status::Fail);
    assert!(host.detail.contains("Permission denied"));
    assert!(
        host.fix
            .as_deref()
            .unwrap()
            .contains("BatchMode=yes ci@build-box true")
    );
    for name in ["system", "tools", "root", "disk", "arch", "harness env"] {
        assert_eq!(check(&list, name).status, Status::Skipped, "{name}");
    }
}

#[test]
fn a_missing_local_client_fails_and_the_host_is_not_asked() {
    let mut f = facts();
    f.client = Err("cannot run `ssh`".into());
    f.probe = None;
    let list = checks(&f);
    assert_eq!(check(&list, "ssh").status, Status::Fail);
    assert_eq!(check(&list, "host").status, Status::Skipped);
}

#[test]
fn missing_tools_fail_and_the_fix_says_nothing_is_installed_for_you() {
    let list = checks(&with(|p| p.missing = vec!["git".into(), "setsid".into()]));
    let tools = check(&list, "tools");
    assert_eq!(tools.status, Status::Fail);
    assert!(tools.detail.contains("git, setsid"));
    assert!(tools.fix.as_deref().unwrap().contains("installs nothing"));
}

#[test]
fn a_host_that_is_not_linux_fails() {
    let list = checks(&with(|p| p.system = "Darwin arm64".into()));
    assert_eq!(check(&list, "system").status, Status::Fail);
    assert!(check(&list, "system").detail.contains("not Linux"));
}

#[test]
fn a_root_that_will_be_created_is_fine_and_one_that_cannot_be_is_not() {
    let list = checks(&with(|p| p.root_exists = false));
    assert_eq!(check(&list, "root").status, Status::Ok);
    assert!(check(&list, "root").detail.contains("will be created"));
    let list = checks(&with(|p| {
        p.root_exists = false;
        p.root_writable = false;
        p.anchor = "/srv".into();
    }));
    let root = check(&list, "root");
    assert_eq!(root.status, Status::Fail);
    assert!(root.detail.contains("/srv") && root.detail.contains("not writable"));
}

#[test]
fn low_free_space_warns_and_unreadable_space_warns() {
    let list = checks(&with(|p| p.free_kb = Some(2 * 1024 * 1024)));
    assert_eq!(check(&list, "disk").status, Status::Warn);
    assert!(check(&list, "disk").detail.starts_with("2.0 GiB free"));
    let list = checks(&with(|p| p.free_kb = None));
    assert_eq!(check(&list, "disk").status, Status::Warn);
}

#[test]
fn harness_names_unset_on_the_host_are_named_without_a_value() {
    let mut f = facts();
    f.harness_env = 2;
    f.unset_env = vec!["SOME_KEY".into()];
    let list = checks(&f);
    let env = check(&list, "harness env");
    assert_eq!(env.status, Status::Warn);
    assert!(env.detail.contains("SOME_KEY") && env.detail.contains("ci@build-box"));
    assert!(env.fix.as_deref().unwrap().contains("~/.profile"));
    f.unset_env.clear();
    assert!(check(&checks(&f), "harness env").detail.contains("all 2"));
}
