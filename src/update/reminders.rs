//! Restart reminders for every installed vivi service unit.
//!
//! `vivi update` replaces the binary under a running unit; launchd and
//! systemd keep the old process until the unit is restarted. Each installed
//! unit is named with the command that restarts it. A missing or unreadable
//! unit directory is not an error — it just yields no reminder.

use std::path::{Path, PathBuf};

use crate::skills::{self, SkillCopyStatus};

/// The two user-level unit directories a vivi install can occupy.
pub(crate) struct UnitDirs {
    /// macOS launchd agents (`~/Library/LaunchAgents`).
    pub launch_agents: PathBuf,
    /// Linux user units (`~/.config/systemd/user`).
    pub systemd_user: PathBuf,
}

/// Derive both unit directories from a home directory.
///
/// Without a home directory the paths are empty, so neither directory
/// exists and no reminder is produced.
pub(crate) fn unit_dirs_from_home(home: Option<&Path>) -> UnitDirs {
    match home {
        Some(home) => UnitDirs {
            launch_agents: home.join("Library/LaunchAgents"),
            systemd_user: home.join(".config/systemd/user"),
        },
        None => UnitDirs {
            launch_agents: PathBuf::new(),
            systemd_user: PathBuf::new(),
        },
    }
}

/// One reminder line per installed vivi unit, sorted by path.
pub(crate) fn service_restart_reminders(dirs: &UnitDirs) -> Vec<String> {
    let mut lines: Vec<(PathBuf, String)> = Vec::new();
    for (path, _name) in unit_names(&dirs.launch_agents, "com.vivi", ".plist") {
        lines.push((path.clone(), launchd_reminder(&path)));
    }
    for (path, name) in unit_names(&dirs.systemd_user, "vivi", ".service") {
        lines.push((path.clone(), systemd_reminder(&path, &name)));
    }
    lines.sort();
    lines.into_iter().map(|(_, line)| line).collect()
}

/// Every directory an agent client may hold an installed skill copy in,
/// most common first.
///
/// Without a home directory the list is empty, so no copy is reported.
pub(crate) fn skills_candidate_dirs(home: Option<&Path>) -> Vec<PathBuf> {
    match home {
        Some(home) => vec![
            home.join(".agents/skills"),
            home.join(".claude/skills"),
            home.join(".codex/skills"),
        ],
        None => Vec::new(),
    }
}

/// One reminder line per installed skill copy that no longer matches this
/// binary's embedded catalog, each naming the reinstall command.
pub(crate) fn stale_skills_reminders(candidates: &[PathBuf]) -> Vec<String> {
    let mut lines = Vec::new();
    for dir in candidates {
        if let SkillCopyStatus::Stale(names) = skills::copy_status(dir) {
            lines.push(format!(
                "note: installed skills at {} are stale ({}); refresh: \
                 vivi skills install {} --force",
                dir.display(),
                names.join(", "),
                dir.display()
            ));
        }
    }
    lines
}

fn launchd_reminder(path: &Path) -> String {
    format!(
        "note: installed vivi unit {}; restart it to pick up the new binary: \
         launchctl bootout gui/$(id -u) {} && launchctl bootstrap gui/$(id -u) {}",
        path.display(),
        path.display(),
        path.display()
    )
}

fn systemd_reminder(path: &Path, name: &str) -> String {
    format!(
        "note: installed vivi unit {}; restart it to pick up the new binary: \
         systemctl --user restart {name}",
        path.display()
    )
}

/// Every regular file in `dir` whose name starts with `prefix` and ends
/// with `suffix`, as (full path, file name) pairs.
fn unit_names(dir: &Path, prefix: &str, suffix: &str) -> Vec<(PathBuf, String)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with(prefix) || !name.ends_with(suffix) {
            continue;
        }
        if !entry.path().is_file() {
            continue;
        }
        found.push((entry.path(), name.to_owned()));
    }
    found
}

#[cfg(test)]
#[path = "reminders_test.rs"]
mod tests;
