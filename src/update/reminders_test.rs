//! Tests for `update::reminders`.

use super::{
    UnitDirs, release_skills_changed, release_skills_reminders, service_restart_reminders,
    skills_candidate_dirs, stale_skills_reminders, unit_dirs_from_home,
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

    let candidates = skills_candidate_dirs(Some(home.path()));
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
    assert!(skills_candidate_dirs(None).is_empty());
    assert!(stale_skills_reminders(&skills_candidate_dirs(None)).is_empty());
}

#[test]
fn release_skills_reminder_matches_the_release_answer() {
    let home = tempfile::tempdir().expect("tmp");
    let dir = home.path().join(".agents/skills");
    skills::install(&dir, false).expect("install copy");
    let candidates = vec![dir];

    let changed = release_skills_reminders(&candidates, Some(true));
    assert_eq!(changed.len(), 1, "one up-to-date copy is named: {changed:?}");
    let line = &changed[0];
    assert!(
        line.contains("this release changed skills") && line.contains("may be stale"),
        "a known change states it: {line}"
    );
    assert!(
        line.contains("vivi skills install") && line.contains("--force"),
        "the reminder carries the reinstall command: {line}"
    );

    let unknown = release_skills_reminders(&candidates, None);
    assert_eq!(unknown.len(), 1, "one up-to-date copy is named: {unknown:?}");
    let line = &unknown[0];
    assert!(
        line.contains("if this release changed skills"),
        "an unknown answer asks instead of stating: {line}"
    );
    assert!(
        !line.contains("may be stale"),
        "an unknown answer does not claim the copy is stale: {line}"
    );

    assert!(
        release_skills_reminders(&candidates, Some(false)).is_empty(),
        "an unchanged release adds no reminder"
    );
}

#[cfg(unix)]
fn write_script(path: &std::path::Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;

    std::fs::write(path, body).expect("write script");
    let mut permissions = std::fs::metadata(path).expect("script metadata").permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).expect("make executable");
}

#[cfg(unix)]
fn catalog_json(version: Option<&str>) -> String {
    let listings: Vec<serde_json::Value> = skills::catalog()
        .iter()
        .map(|skill| {
            let summary = skill.summary();
            serde_json::json!({
                "name": summary.name,
                "version": version.unwrap_or(summary.version),
                "description": summary.description,
                "files": summary.files,
            })
        })
        .collect();
    serde_json::to_string(&listings).expect("json")
}

#[cfg(unix)]
#[test]
fn release_skills_changed_runs_staged_binary() {
    let temp = tempfile::tempdir().expect("tmp");

    let same = temp.path().join("vivi-same");
    let script = format!("#!/bin/sh\necho '{}'\n", catalog_json(None));
    write_script(&same, &script);
    assert_eq!(
        release_skills_changed(&same),
        Some(false),
        "a matching catalog means the release kept skills"
    );

    let bumped = temp.path().join("vivi-bumped");
    let script = format!("#!/bin/sh\necho '{}'\n", catalog_json(Some("999.0.0")));
    write_script(&bumped, &script);
    assert_eq!(
        release_skills_changed(&bumped),
        Some(true),
        "a version-bumped catalog means the release changed skills"
    );

    let garbage = temp.path().join("vivi-garbage");
    std::fs::write(&garbage, b"\x7fnot an executable").expect("garbage");
    assert_eq!(
        release_skills_changed(&garbage),
        None,
        "a binary that cannot run yields no answer"
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
