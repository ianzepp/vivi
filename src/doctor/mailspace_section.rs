//! The mailspace section of `vivi doctor`, using only read-only project and
//! storage access.

use std::collections::HashSet;
use std::path::Path;

use super::{Finding, Project};
use crate::storage::schema::STORAGE_SCHEMA_VERSION;

const SECTION: &str = "mailspace";
const CONFIG_FIX: &str =
    "repair .vivi/mailspace.toml (vivi mailspace init --project <root> creates one)";
const STORE_FIX: &str = "run any vivi command once to create it";
const NEWER_FIX: &str = "upgrade the vivi binary (vivi update)";
const OLDER_FIX: &str = "run one write command (e.g. vivi board) to migrate";
const DONE_WHEN_FIX: &str = "add a done_when: clause to each need body (vivi need show <handle>)";

/// Report project config, storage, unabsorbed inbox mail, and open needs with
/// no `done_when:` clause. No check creates or migrates project data.
#[must_use]
pub fn findings(project: Option<&Path>) -> Vec<Finding> {
    let project = match Project::discover(project) {
        Ok(project) => project,
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
        &format!(
            "mailspace config parsed; store {}",
            project.dir.join("mail.sqlite").display()
        ),
    );
    if crate::storage::Storage::read_only_wal_index_missing(&project.dir) {
        let reason = "read-only WAL inspection unavailable: mail.sqlite-shm is missing; doctor will not create it";
        return vec![
            config,
            Finding::skipped(SECTION, "store", reason),
            Finding::skipped(SECTION, "unabsorbed", reason),
            Finding::skipped(SECTION, "done_when", reason),
        ];
    }
    let store = store_finding(&project);
    let mut out = vec![config, store];
    match project.open_storage_read_only() {
        Ok(storage) => {
            out.push(unabsorbed_finding(&project, &storage));
            out.push(done_when_finding(&project, &storage));
        }
        Err(error) => {
            let detail = format!("mail store checks unavailable: {}", first_line(&error));
            out.push(Finding::fail(SECTION, "unabsorbed", &detail, STORE_FIX));
            out.push(Finding::fail(SECTION, "done_when", &detail, STORE_FIX));
        }
    }
    out
}

fn store_finding(project: &Project) -> Finding {
    let path = project.dir.join("mail.sqlite");
    let connection = match rusqlite::Connection::open_with_flags(
        &path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) {
        Ok(connection) => connection,
        Err(_) => return Finding::fail(SECTION, "store", "store missing or unreadable", STORE_FIX),
    };
    let stored = match connection.query_row(
        "SELECT value FROM storage_metadata WHERE key = 'schema_version'",
        [],
        |row| row.get::<_, String>(0),
    ) {
        Ok(version) => version,
        Err(_) => return Finding::fail(SECTION, "store", "store missing or unreadable", STORE_FIX),
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
        Finding::fail(SECTION, "store", "store written by a newer vivi", NEWER_FIX)
    } else if stored_number < current {
        Finding::warn(
            SECTION,
            "store",
            "schema older than this binary; a write command will migrate it",
            OLDER_FIX,
        )
    } else {
        Finding::ok(
            SECTION,
            "store",
            &format!("schema {stored} matches this binary"),
        )
    }
}

fn unabsorbed_finding(project: &Project, storage: &crate::storage::Storage) -> Finding {
    let inbox = match storage.list_messages_by_role("inbox") {
        Ok(messages) => messages,
        Err(error) => {
            return Finding::fail(
                SECTION,
                "unabsorbed",
                &format!(
                    "unabsorbed mail check failed: {}",
                    first_line(&error.to_string())
                ),
                "vivi mail list --status unabsorbed",
            );
        }
    };
    let accounts = local_accounts(project);
    let count = inbox
        .iter()
        .filter(|message| accounts.contains(&message.account) && message.absorbed_at.is_none())
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

fn done_when_finding(project: &Project, storage: &crate::storage::Storage) -> Finding {
    let manifest = match project.mailspace_view().step_shadow_with_storage(storage) {
        Ok(manifest) => manifest,
        Err(error) => {
            return Finding::fail(
                SECTION,
                "done_when",
                &format!("done_when check failed: {}", first_line(&error.to_string())),
                DONE_WHEN_FIX,
            );
        }
    };
    let missing: Vec<&str> = manifest
        .exceptions
        .iter()
        .filter(|exception| exception.reason == "no_done_when")
        .map(|exception| exception.item.as_str())
        .collect();
    if missing.is_empty() {
        return Finding::ok(SECTION, "done_when", "every ready item declares done_when");
    }
    Finding::warn(
        SECTION,
        "done_when",
        &format!(
            "{} ready item(s) declare no done_when: {}",
            missing.len(),
            missing
                .iter()
                .take(5)
                .copied()
                .collect::<Vec<_>>()
                .join(", ")
        ),
        DONE_WHEN_FIX,
    )
}

fn local_accounts(project: &Project) -> HashSet<String> {
    project
        .config
        .identities
        .iter()
        .flat_map(|identity| {
            std::iter::once(identity.name.clone()).chain(identity.aliases.iter().cloned())
        })
        .collect()
}

fn first_line(error: &str) -> String {
    error.lines().next().unwrap_or_default().trim().to_string()
}

#[cfg(test)]
#[path = "mailspace_section_test.rs"]
mod tests;
