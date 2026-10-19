//! Tests for `update::reminders`.

use super::{
    UnitDirs, service_restart_reminders, skills_candidate_dirs, stale_skills_reminders,
    unit_dirs_from_home,
};
use crate::skills;

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
fn stale_skills_reminder_names_dir_and_command() {
    let home = tempfile::tempdir().expect("tmp");
    let stale_dir = home.path().join(".agents/skills");
    let fresh_dir = home.path().join(".claude/skills");

    skills::install(&stale_dir, false).expect("install stale copy");
    let modified = stale_dir.join("vivi/SKILL.md");
    let mut bytes = std::fs::read(&modified).expect("read skill");
    bytes.extend_from_slice(b"\n<!-- local edit -->\n");
    std::fs::write(&modified, bytes).expect("edit skill");

    skills::install(&fresh_dir, false).expect("install fresh copy");

    let candidates = skills_candidate_dirs(Some(home.path()), None);
    assert_eq!(
        candidates,
        vec![
            home.path().join(".agents/skills"),
            home.path().join(".claude/skills"),
            home.path().join(".codex/skills"),
        ],
        "candidate directories keep a fixed order"
    );

    let lines = stale_skills_reminders(&candidates);
    assert_eq!(
        lines.len(),
        1,
        "exactly one stale copy is reported: {lines:?}"
    );
    let line = &lines[0];
    assert!(
        line.contains(".agents/skills"),
        "the reminder names the stale directory: {line}"
    );
    assert!(
        line.contains("vivi skills install") && line.contains("--force"),
        "the reminder carries the reinstall command: {line}"
    );
    assert!(
        !line.contains(".claude/skills"),
        "an up-to-date copy is not reported: {line}"
    );
}

#[test]
fn skills_candidate_dirs_without_home_are_empty() {
    assert!(skills_candidate_dirs(None, None).is_empty());
    assert!(stale_skills_reminders(&skills_candidate_dirs(None, None)).is_empty());
}

#[test]
fn skills_candidate_dirs_merge_recorded_targets() {
    let home = tempfile::tempdir().expect("tmp");
    let config = tempfile::tempdir().expect("tmp");
    let extra = tempfile::tempdir().expect("tmp");
    let known = home.path().join(".agents/skills");
    let record = config.path().join("vivi/skills-targets.txt");
    std::fs::create_dir_all(record.parent().expect("parent")).expect("config dir");
    std::fs::write(
        &record,
        format!(
            "{}\n\n{}\n{}\n",
            extra.path().display(),
            extra.path().display(),
            known.display()
        ),
    )
    .expect("record file");

    let dirs = skills_candidate_dirs(Some(home.path()), Some(config.path()));
    assert_eq!(
        dirs,
        vec![
            known.clone(),
            home.path().join(".claude/skills"),
            home.path().join(".codex/skills"),
            extra.path().to_path_buf(),
        ],
        "known dirs stay first and the recorded extra dir follows once"
    );

    let mut unique = dirs.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), dirs.len(), "no duplicate entries: {dirs:?}");
}

#[test]
fn service_restart_reminders_tolerate_missing_dirs() {
    let home = tempfile::tempdir().expect("tmp");
    let dirs = unit_dirs_from_home(Some(home.path()));
    assert!(service_restart_reminders(&dirs).is_empty());

    let dirs = unit_dirs_from_home(None);
    assert!(service_restart_reminders(&dirs).is_empty());
}
