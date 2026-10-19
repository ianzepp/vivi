//! The `sync` doctor section: `[sync]` config, key, last-sync age, and
//! server reachability.
//!
//! Read-only by construction: it never mints a key (that is `vivi sync join`'s
//! job) and never probes the network itself — the caller passes the
//! reachability result in.

use std::fs;
use std::path::Path;

use chrono::{DateTime, Duration, Utc};

use crate::doctor::Finding;
use crate::mailspace::Mailspace;
use crate::sync::identity;
use crate::sync::state::SyncState;
use crate::sync::{sync_dir, SyncConfig};

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
pub fn findings(
    mailspace: &Mailspace,
    now: DateTime<Utc>,
    reachable: Option<bool>,
) -> Vec<Finding> {
    let Some(config) = mailspace.config.sync.as_ref() else {
        return vec![not_configured()];
    };
    if unset(config) {
        return vec![not_configured()];
    }
    vec![
        config_finding(config),
        key_finding(&mailspace.dir),
        age_finding(&mailspace.dir, project_of(config), now),
        reachability_finding(reachable),
    ]
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
    if SyncState::load(&dir, project).is_err() {
        return never_synced();
    }
    let Some(modified) = fs::metadata(dir.join(STATE_FILE))
        .and_then(|meta| meta.modified())
        .ok()
    else {
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
