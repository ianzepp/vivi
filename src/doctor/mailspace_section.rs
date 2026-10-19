//! The mailspace section of `vivi doctor`: config, storage schema, unabsorbed
//! mail, and ready items that declare no `done_when`.
//!
//! Every check is read-only. The store is opened with
//! [`rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY`] so doctor can never migrate
//! or repair anything.

use std::path::Path;

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OpenFlags};

use super::Finding;
use crate::error::VivariumError;
use crate::mailspace::{DumpFilters, MailAbsorbFilter, MailDumpRequest, Mailspace};
use crate::storage::schema::STORAGE_SCHEMA_VERSION;

const SECTION: &str = "mailspace";
const CONFIG_FIX: &str =
    "repair .vivi/mailspace.toml (vivi mailspace init --project <root> creates one)";
const UNREADABLE_FIX: &str = "run any vivi command once to create it";
const UNREADABLE_DETAIL: &str = "store missing or unreadable";
const NEWER_FIX: &str = "upgrade the vivi binary (vivi update)";
const OLDER_FIX: &str = "run one write command (e.g. vivi board) to migrate";
const DONE_WHEN_FIX: &str =
    "add a done_when: clause to each need body (vivi need show <handle>)";

/// Report the mailspace section of `vivi doctor`.
#[must_use]
pub fn findings(project: Option<&Path>, _now: DateTime<Utc>) -> Vec<Finding> {
    let mailspace = match Mailspace::discover(project) {
        Ok(mailspace) => mailspace,
        Err(error) => {
            return vec![Finding::fail(
                SECTION,
                "config",
                &format!("mailspace cannot be opened: {}", first_line(&error)),
                CONFIG_FIX,
            )];
        }
    };
    let config = Finding::ok(
        SECTION,
        "config",
        &format!("mailspace opened; store {}", mailspace.store_path().display()),
    );
    vec![
        config,
        schema_finding(&mailspace),
        unabsorbed_finding(&mailspace),
        done_when_finding(&mailspace),
    ]
}

/// Compare the store's recorded schema version with this binary's.
fn schema_finding(mailspace: &Mailspace) -> Finding {
    let path = mailspace.store_path();
    let stored = match read_schema_version(&path) {
        Ok(version) => version,
        Err(reason) => {
            return Finding::fail(
                SECTION,
                "store",
                &format!("{UNREADABLE_DETAIL}: {reason}"),
                UNREADABLE_FIX,
            );
        }
    };
    let Ok(current) = STORAGE_SCHEMA_VERSION.parse::<u32>() else {
        return Finding::fail(
            SECTION,
            "store",
            "this binary has no numeric storage schema version",
            NEWER_FIX,
        );
    };
    let Ok(stored_number) = stored.parse::<u32>() else {
        return Finding::warn(
            SECTION,
            "store",
            &format!("schema version {stored} is not numeric; this binary expects {current}"),
            NEWER_FIX,
        );
    };
    if stored_number > current {
        Finding::fail(
            SECTION,
            "store",
            "store written by a newer vivi",
            NEWER_FIX,
        )
    } else if stored_number < current {
        Finding::warn(
            SECTION,
            "store",
            "schema older than this binary; the next write command migrates it",
            OLDER_FIX,
        )
    } else {
        Finding::ok(SECTION, "store", &format!("schema {stored} matches this binary"))
    }
}

/// Read the recorded schema version without touching the writable store.
fn read_schema_version(path: &Path) -> Result<String, String> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| error.to_string())?;
    conn.query_row(
        "SELECT value FROM storage_metadata WHERE key = 'schema_version'",
        [],
        |row| row.get::<_, String>(0),
    )
    .map_err(|error| error.to_string())
}

/// Count the inbox records this mailspace holds and nobody has absorbed.
fn unabsorbed_finding(mailspace: &Mailspace) -> Finding {
    let request = MailDumpRequest {
        folder: "inbox".into(),
        kind: None,
        filters: DumpFilters {
            absorb_status: MailAbsorbFilter::Unabsorbed,
            ..Default::default()
        },
    };
    let records = match mailspace.dump_mail(request) {
        Ok(records) => records,
        Err(error) => {
            return Finding::fail(
                SECTION,
                "unabsorbed",
                &format!("unabsorbed mail check failed: {}", first_line(&error)),
                "vivi mail list --status unabsorbed",
            );
        }
    };
    let count = records
        .iter()
        .filter(|record| mailspace.is_local_account(&record.account))
        .count();
    if count == 0 {
        Finding::ok(SECTION, "unabsorbed", "unabsorbed mail 0")
    } else {
        Finding::warn(
            SECTION,
            "unabsorbed",
            &format!("unabsorbed mail {count}"),
            "vivi mail list --status unabsorbed, then vivi mail absorb <handle> --for <identity>",
        )
    }
}

/// List the ready items whose body declares no `done_when:` clause.
fn done_when_finding(mailspace: &Mailspace) -> Finding {
    let manifest = match mailspace.step_shadow() {
        Ok(manifest) => manifest,
        Err(error) => {
            return Finding::fail(
                SECTION,
                "done_when",
                &format!("done_when check failed: {}", first_line(&error)),
                DONE_WHEN_FIX,
            );
        }
    };
    let handles: Vec<&str> = manifest
        .exceptions
        .iter()
        .filter(|exception| exception.reason == "no_done_when")
        .map(|exception| exception.item.as_str())
        .collect();
    if handles.is_empty() {
        return Finding::ok(SECTION, "done_when", "every ready need declares done_when");
    }
    let listed: Vec<&str> = handles.iter().take(5).copied().collect();
    Finding::warn(
        SECTION,
        "done_when",
        &format!(
            "{} ready item(s) declare no done_when: {}",
            handles.len(),
            listed.join(", ")
        ),
        DONE_WHEN_FIX,
    )
}

/// The first line of an error. Parse errors echo the offending file line back,
/// so only the headline (which may name a path) reaches a finding.
fn first_line(error: &VivariumError) -> String {
    error
        .to_string()
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_string()
}

#[cfg(test)]
#[path = "mailspace_section_test.rs"]
mod tests;
