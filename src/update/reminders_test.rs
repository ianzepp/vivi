//! Tests for `update::reminders`.

use super::{UnitDirs, service_restart_reminders, unit_dirs_from_home};

#[test]
fn service_restart_reminders_name_installed_units() {
    let temp = tempfile::tempdir().expect("tmp");
    let launch_agents = temp.path().join("LaunchAgents");
    let systemd_user = temp.path().join("systemd-user");
    std::fs::create_dir_all(&launch_agents).expect("launch dir");
    std::fs::create_dir_all(&systemd_user).expect("systemd dir");
    for name in [
        "com.vivi.pump-service.plist",
        "com.vivi.sync.plist",
        "com.vivi.proxy.plist",
        "com.other.plist",
        "notes.txt",
    ] {
        std::fs::write(launch_agents.join(name), "unit").expect("launchd unit");
    }
    std::fs::write(systemd_user.join("vivi-sync.service"), "unit").expect("systemd unit");

    let dirs = UnitDirs {
        launch_agents,
        systemd_user,
    };
    let joined = service_restart_reminders(&dirs).join("\n");

    assert!(
        joined.contains("com.vivi.pump-service.plist"),
        "the pump service unit is named: {joined}"
    );
    assert!(
        joined.contains("com.vivi.sync.plist"),
        "the sync unit is named: {joined}"
    );
    for line in joined.lines().filter(|line| line.contains(".plist")) {
        assert!(
            line.contains("launchctl bootout gui/$(id -u)"),
            "launchd reminder carries the restart command: {line}"
        );
    }
    let systemd = joined
        .lines()
        .find(|line| line.contains("vivi-sync.service"))
        .expect("the systemd unit is named");
    assert!(
        systemd.contains("systemctl --user restart vivi-sync.service"),
        "systemd reminder carries the restart command: {systemd}"
    );
    assert!(
        !joined.contains("com.other") && !joined.contains("notes.txt"),
        "unrelated files are not reported: {joined}"
    );
}

#[test]
fn service_restart_reminders_tolerate_missing_dirs() {
    let home = tempfile::tempdir().expect("tmp");
    let dirs = unit_dirs_from_home(Some(home.path()));
    assert!(service_restart_reminders(&dirs).is_empty());

    let dirs = unit_dirs_from_home(None);
    assert!(service_restart_reminders(&dirs).is_empty());
}
