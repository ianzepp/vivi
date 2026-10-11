use super::*;
use crate::pool::doctor::Status;

fn facts() -> DockerFacts {
    DockerFacts {
        host: None,
        image: "ubuntu:24.04".into(),
        memory_limit: "4g".into(),
        unset_env: Vec::new(),
        harness_env: 0,
        cli: Ok("Docker version 29.8.0, build abc".into()),
        daemon: Some(Ok(Daemon {
            version: "29.8.0".into(),
            platform: "linux/arm64".into(),
            memory: 8_317_267_968,
        })),
        image_state: Some(Ok(ImageState::Present("arm64".into()))),
    }
}

fn status_of(checks: &[Check], name: &str) -> Status {
    checks.iter().find(|c| c.name == name).unwrap().status
}

fn check<'a>(checks: &'a [Check], name: &str) -> &'a Check {
    checks.iter().find(|c| c.name == name).unwrap()
}

#[test]
fn a_healthy_local_daemon_passes_every_check() {
    let all = checks(&facts());
    assert_eq!(all.len(), 5);
    assert!(all.iter().all(|c| c.status == Status::Ok), "{all:?}");
    assert!(
        check(&all, "daemon")
            .detail
            .contains("local daemon, Docker 29.8.0, linux/arm64")
    );
    assert!(check(&all, "image").detail.contains("present (arm64)"));
}

#[test]
fn a_remote_daemon_is_named_and_its_failure_points_at_ssh() {
    let mut f = facts();
    f.host = Some("ssh://ci@box".into());
    f.daemon = Some(Err("Cannot connect".into()));
    f.image_state = None;
    let all = checks(&f);
    let daemon = check(&all, "daemon");
    assert_eq!(daemon.status, Status::Fail);
    assert!(
        daemon.detail.contains("daemon at ssh://ci@box unreachable"),
        "{daemon:?}"
    );
    assert!(daemon.fix.as_deref().unwrap().contains("ssh ci@box"));
    assert_eq!(status_of(&all, "image"), Status::Skipped);
    assert_eq!(status_of(&all, "memory"), Status::Skipped);
}

#[test]
fn no_docker_binary_fails_and_skips_the_rest() {
    let mut f = facts();
    f.cli = Err("failed to run `docker`".into());
    f.daemon = None;
    f.image_state = None;
    let all = checks(&f);
    assert_eq!(status_of(&all, "docker"), Status::Fail);
    assert_eq!(status_of(&all, "daemon"), Status::Skipped);
}

#[test]
fn an_image_missing_locally_but_in_a_registry_is_fine_and_one_nowhere_fails() {
    let mut f = facts();
    f.image_state = Some(Ok(ImageState::Pullable));
    let all = checks(&f);
    assert_eq!(status_of(&all, "image"), Status::Ok);
    assert!(check(&all, "image").detail.contains("pulls it"));
    f.image_state = Some(Err("no such manifest".into()));
    let all = checks(&f);
    assert_eq!(status_of(&all, "image"), Status::Fail);
    assert!(
        check(&all, "image")
            .fix
            .as_deref()
            .unwrap()
            .contains("docker pull ubuntu:24.04")
    );
}

#[test]
fn a_daemon_with_less_memory_than_a_slot_may_use_warns() {
    let mut f = facts();
    f.memory_limit = "16g".into();
    let all = checks(&f);
    let memory = check(&all, "memory");
    assert_eq!(memory.status, Status::Warn);
    assert!(
        memory.detail.contains("7.7 GiB") && memory.detail.contains("16g"),
        "{memory:?}"
    );
    assert!(memory.fix.is_some());
}

#[test]
fn unset_harness_env_names_warn_with_the_export_and_never_show_values() {
    let mut f = facts();
    f.harness_env = 2;
    f.unset_env = vec!["A_KEY".into(), "B_KEY".into()];
    let all = checks(&f);
    let env = check(&all, "harness env");
    assert_eq!(env.status, Status::Warn);
    assert!(env.detail.contains("A_KEY, B_KEY"));
    assert_eq!(
        env.fix.as_deref(),
        Some("export A_KEY=<value> B_KEY=<value> before `vivi pool create`")
    );
    f.unset_env.clear();
    assert_eq!(status_of(&checks(&f), "harness env"), Status::Ok);
}

#[test]
fn docker_sizes_are_binary_and_bad_ones_are_none() {
    assert_eq!(parse_size("4g"), Some(4 << 30));
    assert_eq!(parse_size("512M"), Some(512 << 20));
    assert_eq!(parse_size("4096k"), Some(4096 << 10));
    assert_eq!(parse_size("4"), None);
    assert_eq!(parse_size("g"), None);
}
