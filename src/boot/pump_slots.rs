//! Cheap local slot reads for the boot pump snapshot.
//!
//! Only the `local` provider is walked: project-relative roots use
//! `<root>/<pool>/slot-N`, while shared roots include the mailspace id.
//! Remote providers are never contacted; boot records them as remote.

use std::path::{Path, PathBuf};

use crate::mailspace::MailspaceConfig;
use crate::pool::config::PoolConfig;
use crate::pool::local_slots::{self, CHECKOUT};
use crate::pool::reset::{MARK_BASE_REF, MARK_BASE_SHA};
use crate::pool::slot::{META_CLAIMED, META_TASK, space_key};

/// One slot known from a local disk walk, or a remote placeholder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalSlotMark {
    pub pool: String,
    /// `<pool>/slot-N` for project-relative roots; otherwise
    /// `<mailspace-id>/<pool>/slot-N`.
    pub id: String,
    pub dir: PathBuf,
    pub checkout: Option<PathBuf>,
    pub task: Option<String>,
    pub claimed_at: Option<u64>,
    pub base_ref: Option<String>,
    pub base_sha: Option<String>,
}

/// Walk every local pool's slot directories. Remote pools contribute nothing.
#[must_use]
pub fn scan(config: &MailspaceConfig, project: &Path) -> Vec<LocalSlotMark> {
    let key = space_key(&config.name, config.id.as_deref().unwrap_or_default());
    let mut marks = Vec::new();
    for (name, pool) in &config.pools {
        if !is_local(pool) {
            continue;
        }
        let Ok(site) =
            crate::pool::local_site::resolve(pool.local.root_or_default(), project, &key, name)
        else {
            continue;
        };
        if let Ok(records) = local_slots::list(&site) {
            marks.extend(
                records
                    .iter()
                    .map(|record| mark_from(&site.root, name, record)),
            );
        }
    }
    marks
}

fn is_local(pool: &PoolConfig) -> bool {
    pool.provider.eq_ignore_ascii_case("local")
}

fn mark_from(
    root: &Path,
    pool: &str,
    record: &crate::pool::provider::MachineRecord,
) -> LocalSlotMark {
    let dir = root.join(&record.id);
    let checkout = dir.join(CHECKOUT);
    LocalSlotMark {
        pool: pool.to_string(),
        id: record.id.clone(),
        checkout: checkout.is_dir().then_some(checkout),
        task: record.metadata.get(META_TASK).cloned(),
        claimed_at: record
            .metadata
            .get(META_CLAIMED)
            .and_then(|t| Result::ok(t.parse())),
        base_ref: record.metadata.get(MARK_BASE_REF).cloned(),
        base_sha: record.metadata.get(MARK_BASE_SHA).cloned(),
        dir,
    }
}

/// The slot this process occupies: `VIVI_SLOT_ROOT`, else a `cwd` inside a
/// scanned slot directory.
#[must_use]
pub fn detect_here<'a>(
    slots: &'a [LocalSlotMark],
    cwd: Option<&Path>,
) -> Option<&'a LocalSlotMark> {
    if let Ok(root) = std::env::var("VIVI_SLOT_ROOT") {
        let path = PathBuf::from(root);
        if let Some(found) = match_path(slots, &path) {
            return Some(found);
        }
    }
    cwd.and_then(|path| match_path(slots, path))
}

fn match_path<'a>(slots: &'a [LocalSlotMark], path: &Path) -> Option<&'a LocalSlotMark> {
    let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    slots.iter().find(|slot| path_in_slot(&canon, slot))
}

fn path_in_slot(path: &Path, slot: &LocalSlotMark) -> bool {
    let dir = slot.dir.canonicalize().unwrap_or_else(|_| slot.dir.clone());
    if path.starts_with(&dir) {
        return true;
    }
    slot.checkout.as_ref().is_some_and(|checkout| {
        let checkout = checkout.canonicalize().unwrap_or_else(|_| checkout.clone());
        path.starts_with(checkout)
    })
}

#[cfg(test)]
#[path = "pump_slots_test.rs"]
mod tests;
