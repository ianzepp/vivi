use std::path::Path;

use super::{
    launch_agent_plist, parse_launchd_pid, prefer_stable_link, systemd_apply_steps, systemd_unit,
};

#[test]
fn systemd_unit_limits_crash_loops() {
    let unit = systemd_unit(Path::new("/opt/vivi"), None);
    assert!(unit.contains("proxy run"));
    assert!(unit.contains("Restart=on-failure"));
    assert!(unit.contains("StartLimitIntervalSec=60"));
    assert!(unit.contains("StartLimitBurst=5"));
    assert!(!unit.contains("Restart=always"));
}

#[test]
fn systemd_apply_steps_restarts_active_unit() {
    let steps = systemd_apply_steps();
    assert_eq!(steps.first(), Some(&vec!["--user", "daemon-reload"]));
    assert!(steps.contains(&vec!["--user", "enable", "vivi-proxy.service"]));
    assert_eq!(
        steps.last(),
        Some(&vec!["--user", "restart", "vivi-proxy.service"])
    );
    assert!(steps.iter().all(|step| !step.contains(&"--now")));
}

#[test]
fn launchd_plist_throttles_restarts() {
    let plist = launch_agent_plist(Path::new("/opt/vivi"), Path::new("/tmp/logs"), "/bin", None);
    assert!(plist.contains("com.vivi.proxy"));
    assert!(plist.contains("<key>KeepAlive</key><true/>"));
    assert!(plist.contains("<key>ThrottleInterval</key><integer>10</integer>"));
}

#[test]
fn launchd_print_pid_is_parsed() {
    let running =
        "gui/501/com.vivi.proxy = {\n\tactive count = 1\n\tpid = 85306\n\tstate = running\n}";
    assert_eq!(parse_launchd_pid(running), Some(85306));
    let stopped = "gui/501/com.vivi.proxy = {\n\tactive count = 0\n\tstate = not running\n}";
    assert_eq!(parse_launchd_pid(stopped), None);
}

#[cfg(unix)]
#[test]
fn prefer_stable_link_uses_sibling_vivi_symlink() {
    let dir = tempfile::tempdir().expect("tmp");
    let versioned = dir.path().join("vivi-14.1.1");
    let link = dir.path().join("vivi");
    std::fs::write(&versioned, b"bin").expect("write");
    std::os::unix::fs::symlink("vivi-14.1.1", &link).expect("symlink");
    assert_eq!(prefer_stable_link(&versioned), link);
}

#[test]
fn prefer_stable_link_keeps_plain_or_unrelated_names() {
    let plain = Path::new("/opt/vivi");
    assert_eq!(prefer_stable_link(plain), plain);
    let cargo = Path::new("/home/me/.cargo/bin/vivi");
    assert_eq!(prefer_stable_link(cargo), cargo);
}
