//! The `sync` doctor section: `[sync]` config, key, last-sync age, and
//! server reachability.
//!
//! Read-only by construction: it never mints a key (that is `vivi sync join`'s
//! job) and never probes the network itself — the caller passes the
//! reachability result in.

use std::fs;
use std::path::Path;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, Utc};

use crate::doctor::{Finding, Project, Status};
use crate::sync::identity;
use crate::sync::quarantine;
use crate::sync::state::SyncState;
use crate::sync::{SyncConfig, sync_dir};

const SECTION: &str = "sync";

/// A last sync older than this is worth warning about.
const STALE_AFTER_HOURS: i64 = 24;

/// The pull-state file name under `.vivi/sync/` (see `crate::sync::state`).
const STATE_FILE: &str = "state.json";

/// The findings for the `sync` section.
///
/// `reachable` is the injected server reachability: `None` when nobody
/// probed, otherwise whether the probe succeeded.
#[must_use]
pub fn findings(project: &Project, now: DateTime<Utc>, reachable: Option<bool>) -> Vec<Finding> {
    let Some(config) = project.config.sync.as_ref() else {
        return not_configured_findings(&project.dir);
    };
    if unset(config) {
        return not_configured_findings(&project.dir);
    }
    vec![
        config_finding(config),
        key_finding(&project.dir),
        age_finding(&project.dir, project_of(config), now),
        reachability_finding(reachable),
        quarantine_finding(&project.dir),
    ]
}

/// The skipped section, plus held letters when there are any: they stay
/// actionable even with no `[sync]` table.
fn not_configured_findings(vivi_dir: &Path) -> Vec<Finding> {
    let held = quarantine_finding(vivi_dir);
    if held.status == Status::Ok {
        vec![not_configured()]
    } else {
        vec![not_configured(), held]
    }
}

/// One finding for `.vivi/quarantine/`: ok when nothing is held, warn with
/// id and reason per letter while files are present, fail when a receipt
/// cannot be read (a corrupt receipt is surfaced, never skipped).
fn quarantine_finding(vivi_dir: &Path) -> Finding {
    match quarantine::list(vivi_dir) {
        Ok(held) if held.is_empty() => Finding::ok(SECTION, "quarantine", "no letters held unread"),
        Ok(held) => {
            let each = held
                .iter()
                .map(|letter| format!("{} ({})", letter.id, letter.reason))
                .collect::<Vec<_>>()
                .join(", ");
            Finding::warn(
                SECTION,
                "quarantine",
                &format!("{} letter(s) held unread: {each}", held.len()),
                "vivi sync quarantine list, then retry <id> or drop <id>",
            )
        }
        Err(error) => Finding::fail(
            SECTION,
            "quarantine",
            &format!(
                "quarantine unreadable: {}",
                error.to_string().lines().next().unwrap_or_default()
            ),
            "inspect .vivi/quarantine/",
        ),
    }
}

/// Probe the configured server root without credentials or a write request.
/// Any HTTP response proves reachability; transport and timeout errors do not.
#[must_use]
pub fn probe_server(project: &Project) -> Option<bool> {
    let server = project
        .config
        .sync
        .as_ref()
        .and_then(|config| field(config.server.as_deref()))?;
    let client = reqwest::blocking::Client::builder()
        .timeout(StdDuration::from_secs(2))
        .no_proxy()
        .build();
    Some(client.is_ok_and(|client| client.get(server).send().is_ok()))
}

fn not_configured() -> Finding {
    Finding::skipped(SECTION, "config", "sync not configured")
}

fn field(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

/// Whether every key the section needs is unset or blank.
fn unset(config: &SyncConfig) -> bool {
    field(config.server.as_deref()).is_none()
        && field(config.project.as_deref()).is_none()
        && field(config.name.as_deref()).is_none()
}

/// The configured project id, or `""` when it is missing.
fn project_of(config: &SyncConfig) -> &str {
    field(config.project.as_deref()).unwrap_or_default()
}

fn config_finding(config: &SyncConfig) -> Finding {
    let missing: Vec<&str> = [
        ("server", field(config.server.as_deref())),
        ("project", field(config.project.as_deref())),
        ("name", field(config.name.as_deref())),
    ]
    .iter()
    .filter_map(|(key, value)| value.is_none().then_some(*key))
    .collect();
    if missing.is_empty() {
        return Finding::ok(SECTION, "config", "[sync] server, project and name are set");
    }
    Finding::warn(
        SECTION,
        "config",
        &format!("[sync] incomplete: missing {}", missing.join("/")),
        "set [sync] server, project and name in .vivi/mailspace.toml",
    )
}

fn key_finding(vivi_dir: &Path) -> Finding {
    if identity::key_path(vivi_dir).is_file() {
        Finding::ok(SECTION, "key", "sync key present")
    } else {
        Finding::warn(
            SECTION,
            "key",
            "no sync key on disk",
            "vivi sync join mints the key when you join the project",
        )
    }
}

fn age_finding(vivi_dir: &Path, project: &str, now: DateTime<Utc>) -> Finding {
    let dir = sync_dir(vivi_dir);
    match SyncState::load(&dir, project) {
        Ok(_) => {}
        Err(error) => {
            return Finding::fail(
                SECTION,
                "age",
                &format!(
                    "sync state unreadable: {}",
                    error.to_string().lines().next().unwrap_or_default()
                ),
                "vivi sync resync",
            );
        }
    }
    let Ok(modified) = fs::metadata(dir.join(STATE_FILE)).and_then(|meta| meta.modified()) else {
        return never_synced();
    };
    let age = now.signed_duration_since(DateTime::<Utc>::from(modified));
    if age > Duration::hours(STALE_AFTER_HOURS) {
        Finding::warn(
            SECTION,
            "age",
            &format!("last sync {}h ago", age.num_hours()),
            "run vivi sync",
        )
    } else {
        Finding::ok(SECTION, "age", &format!("last sync {}", humanize(age)))
    }
}

fn never_synced() -> Finding {
    Finding::skipped(SECTION, "age", "never synced")
}

/// A short, rounded age for a healthy last sync.
fn humanize(age: Duration) -> String {
    let age = if age < Duration::zero() {
        Duration::zero()
    } else {
        age
    };
    match age.num_minutes() {
        0 => "just now".to_string(),
        1 => "1m ago".to_string(),
        minutes if minutes < 60 => format!("{minutes}m ago"),
        _ => format!("{}h ago", age.num_hours()),
    }
}

fn reachability_finding(reachable: Option<bool>) -> Finding {
    match reachable {
        None => Finding::skipped(SECTION, "server", "server reachability not probed"),
        Some(true) => Finding::ok(SECTION, "server", "server reachable"),
        Some(false) => Finding::fail(
            SECTION,
            "server",
            "server unreachable",
            "check the [sync] server URL and network, then vivi sync",
        ),
    }
}

#[cfg(test)]
#[path = "sync_section_test.rs"]
mod tests;
