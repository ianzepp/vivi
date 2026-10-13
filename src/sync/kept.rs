//! Kept-local letters: the content ids push stops trying, in
//! `<project>/.vivi/sync/kept-local.json`.
//!
//! A letter push refuses (over the size cap, or a From that claims another
//! mailspace) can never be delivered, and it fails every push until it is
//! cleared. Clearing does not trash or move the letter — it may be a task or
//! need with a backlog node — so the operator records it as kept local and
//! push stops selecting it. The letter stays in the mailspace untouched.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::VivariumError;
use crate::mailspace::Mailspace;
use crate::storage::Storage;
use crate::sync::select::{self, Skipped};
use crate::sync::{SyncSettings, sync_dir};

const KEPT_FILE: &str = "kept-local.json";

/// The content ids of letters the operator has told push to stop trying.
/// Persisted as a sorted JSON array; a set, so re-keeping a letter is a
/// no-op.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeptLocal {
    ids: BTreeSet<String>,
}

impl KeptLocal {
    /// Load the kept-local list. A missing file is the empty list.
    ///
    /// # Errors
    /// Returns an error naming the file when it exists but cannot be read
    /// or parsed.
    pub fn load(dir: &Path) -> Result<Self, VivariumError> {
        let path = kept_path(dir);
        let unreadable = |cause: &str| {
            VivariumError::Message(format!("kept-local list {} {cause}", path.display()))
        };
        let raw = match fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(unreadable(&format!("cannot be read ({e})"))),
        };
        let ids: Vec<String> = serde_json::from_str(&raw)
            .map_err(|_| unreadable("is corrupt (it is not a kept-local list)"))?;
        Ok(Self {
            ids: ids.into_iter().collect(),
        })
    }

    /// Write the list atomically (temp file, then rename), creating the sync
    /// dir with a self-gitignore on first use.
    ///
    /// # Errors
    /// Returns an error when the directory or file cannot be written.
    pub fn save(&self, dir: &Path) -> Result<(), VivariumError> {
        super::state::ensure_sync_dir(dir)?;
        let raw = serde_json::to_string_pretty(&self.ids)
            .map_err(|e| VivariumError::Other(format!("failed to encode kept-local list: {e}")))?;
        let path = kept_path(dir);
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, raw)?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Whether a content id is kept local.
    #[must_use]
    pub fn contains(&self, content_id: &str) -> bool {
        self.ids.contains(content_id)
    }

    /// How many content ids are kept local.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// Whether nothing is kept local.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    fn insert(&mut self, content_id: String) -> bool {
        self.ids.insert(content_id)
    }

    fn remove(&mut self, content_id: &str) -> bool {
        self.ids.remove(content_id)
    }
}

fn kept_path(dir: &Path) -> PathBuf {
    dir.join(KEPT_FILE)
}

/// Record (or, with `undo`, clear) letters as kept local: one line per
/// handle for the command to print.
///
/// Without `undo`, a handle must name a letter push currently refuses (over
/// the size cap, or a From that claims another mailspace): those are the
/// only letters clearing helps. Any other letter is refused with an error,
/// and unknown or ambiguous handles are errors; in both cases nothing is
/// written for the whole call. With `undo`, the letter's content id is
/// removed from the list (no stuck check); a handle that was not kept says
/// so in its line and is not an error.
///
/// # Errors
/// Returns an error on a storage failure, an unresolvable handle, a letter
/// push does not refuse, or a failure writing the list.
pub fn keep_local(
    mailspace: &Mailspace,
    settings: &SyncSettings,
    handles: &[String],
    undo: bool,
) -> Result<Vec<String>, VivariumError> {
    let storage = mailspace.storage()?;
    let dir = sync_dir(&mailspace.dir);
    let mut kept = KeptLocal::load(&dir)?;
    let mut rows = Vec::new();
    for handle in handles {
        let message_id = storage.resolve_message_token(handle)?;
        let row = storage
            .message_by_id(&message_id)?
            .ok_or_else(|| VivariumError::Message(format!("message not found: {handle}")))?;
        rows.push((handle.clone(), row));
    }
    let mut lines = Vec::new();
    let mut changed = false;
    if undo {
        for (handle, row) in rows {
            if kept.remove(&row.content_id) {
                changed = true;
                lines.push(format!(
                    "no longer kept local {handle}; push tries it again"
                ));
            } else {
                lines.push(format!("not kept local {handle}; nothing to undo"));
            }
        }
    } else {
        let stuck = stuck_letters(&storage, settings)?;
        for (handle, row) in rows {
            let Some(skipped) = stuck.get(&row.content_id) else {
                return Err(VivariumError::Message(format!(
                    "{handle} is not a letter push refuses (too large, or a From that claims \
                     another mailspace); nothing to keep local"
                )));
            };
            changed |= kept.insert(row.content_id);
            lines.push(format!(
                "kept local {handle}: {}; it stays in this mailspace and push no longer tries it",
                skipped.reason.label()
            ));
        }
    }
    if changed {
        kept.save(&dir)?;
    }
    Ok(lines)
}

/// The letters a push would skip as undeliverable, by content id: the only
/// letters keeping local helps. The selection is taken with an empty kept
/// list, so a letter already kept still counts.
fn stuck_letters(
    storage: &Storage,
    settings: &SyncSettings,
) -> Result<BTreeMap<String, Skipped>, VivariumError> {
    let selection = select::select(storage, settings, &KeptLocal::default(), None)?;
    let mut stuck = BTreeMap::new();
    for skipped in selection
        .skipped
        .iter()
        .filter(|s| s.reason.undeliverable())
    {
        let message_id = storage.resolve_message_token(&skipped.handle)?;
        if let Some(row) = storage.message_by_id(&message_id)? {
            stuck.insert(row.content_id, skipped.clone());
        }
    }
    Ok(stuck)
}

#[cfg(test)]
#[path = "kept_test.rs"]
mod tests;
