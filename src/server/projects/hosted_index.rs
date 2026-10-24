//! Hosted projects, S4 and S5: keeping the transport index current with the
//! host mailspace, and filing a member's append into it.
//!
//! **The indexer (S4).** A hosted project's letters already live in the host
//! mailspace; nothing is copied. The index pass drives vivi's own push
//! selection (`vivi::sync::push::push`, so `[sync] send` decides what
//! travels) through [`IndexTransport`], an in-process `Transport` whose
//! append only checks that the blob store holds the letter's hash and
//! inserts the transport row. Push then binds the letter in the mailspace
//! under the project id, so the next pass skips it. The hub's host config
//! sets `send = "addressed"` so only letters addressed off the host are
//! indexed; the server never overrides the mailspace's own setting.
//!
//! A pass runs on a timer ([`crate::hosted::start_indexer`]) and before a
//! list, lookup, or fetch read whose mailspace `change_watermark` moved
//! since the last pass. It skips when another pass for the project is
//! running, when the host's own CLI holds the mailspace's sync lock, and
//! when the watermark has not moved. The cost of a pass that has work or
//! whose watermark moved is one `select`: it lists every message row and
//! classifies each unbound letter, so it grows with the mailspace, not with
//! the number of new letters.
//!
//! **Member appends (S5).** The append path (already serialized under the
//! append mutex) takes the next sequence, files the letter through
//! `vivi::sync::pull::file_records` exactly as a client pull files a wire
//! record, then writes the transport row. It never takes the sync lock.
//! Filing binds the rows under the same project id, so the indexer never
//! picks the letter up again. A letter the filer quarantines (a format-1
//! letter that will not parse) is refused with the parser's reason and its
//! held pair discarded: a server copy must not hold a letter it cannot
//! read.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions, TryLockError};
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};

use vivi::VivariumError;
use vivi::b64;
use vivi::mailspace::Mailspace;
use vivi::storage::ChangeWatermark;
use vivi::sync::pull::file_records;
use vivi::sync::quarantine;
use vivi::sync::state::SyncState;
use vivi::sync::transport::{
    AppendFailure, Appended, Envelope, ListQuery, Page, Record, Transport,
};
use vivi::sync::{SendFilter, SyncSettings, push, sync_dir};

use super::{
    AppendItem, AppendOutcome, Project, ProjectError, ProjectMeta, ProjectStore, append_lock,
    monotone_stamp, sha256_hex, storage_err,
};
use crate::ids::normalize_room_id;

/// Where a hosted project's pull-style filing state lives, beside
/// `project.json`: `DATA_DIR/projects/<id>/sync/state.json`.
const SYNC_STATE_DIR: &str = "sync";

/// Why an index pass did not run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexSkip {
    /// Not a registered hosted project.
    NotHosted,
    /// Another pass for this project is running.
    Busy,
    /// The host's own CLI (or anything else) holds the mailspace sync lock.
    SyncLocked,
    /// The mailspace has not changed since the last pass.
    Unchanged,
}

/// What one index pass did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexOutcome {
    Skipped(IndexSkip),
    Ran {
        /// Letters newly indexed.
        indexed: usize,
        /// Letters the index already held (bound all the same).
        duplicates: usize,
        /// Letters the index refused (for example more recipients than the
        /// wire allows); retried when the mailspace next changes.
        refused: usize,
        /// Why the pass stopped early, if it did.
        stopped: Option<String>,
    },
}

/// Per-project pass state, behind the pass mutex.
#[derive(Debug, Default)]
pub(super) struct PassState {
    /// The mailspace root and watermark the last complete pass saw.
    mark: Option<(std::path::PathBuf, ChangeWatermark)>,
    /// Handles refused by the last pass, so each is logged once.
    refused: HashSet<String>,
}

/// A registered hosted project: who its letters are from, and its pass state.
#[derive(Debug, Clone)]
pub(super) struct HostedEntry {
    sender: String,
    prefix: Option<String>,
    pass: Arc<Mutex<PassState>>,
}

pub(super) type HostedMap = Arc<RwLock<HashMap<String, HostedEntry>>>;

impl ProjectStore {
    /// Register (or re-register) a hosted project and the creator member its
    /// indexed letters are attributed to. Pass state survives a re-register.
    pub(crate) fn register_hosted(&self, project_id: &str, sender: &str, prefix: Option<&str>) {
        let mut hosted = self.hosted.write().unwrap_or_else(|e| e.into_inner());
        let pass = hosted
            .get(project_id)
            .map(|entry| entry.pass.clone())
            .unwrap_or_default();
        hosted.insert(
            project_id.to_owned(),
            HostedEntry {
                sender: sender.to_owned(),
                prefix: prefix.map(str::to_owned),
                pass,
            },
        );
    }

    fn hosted_entry(&self, raw_id: &str) -> Option<(String, HostedEntry)> {
        let id = normalize_room_id(raw_id).ok()?;
        let hosted = self.hosted.read().unwrap_or_else(|e| e.into_inner());
        hosted.get(&id).cloned().map(|entry| (id, entry))
    }

    /// Ids of the registered hosted projects.
    pub fn hosted_ids(&self) -> Vec<String> {
        let hosted = self.hosted.read().unwrap_or_else(|e| e.into_inner());
        let mut ids: Vec<String> = hosted.keys().cloned().collect();
        ids.sort();
        ids
    }

    /// One index pass for every registered hosted project that is still
    /// served. A failure is logged and does not stop the others.
    pub fn index_all_hosted(&self) {
        for id in self.hosted_ids() {
            match self.index_hosted(&id) {
                Ok(IndexOutcome::Ran {
                    indexed, stopped, ..
                }) => {
                    if indexed > 0 {
                        tracing::info!("indexed {indexed} letter(s) of hosted project {id}");
                    }
                    if let Some(reason) = stopped {
                        tracing::warn!("index pass for hosted project {id} stopped: {reason}");
                    }
                }
                Ok(IndexOutcome::Skipped(_)) => {}
                // A retired project answers as unknown: nothing to index.
                Err(ProjectError::ProjectNotFound) => {}
                Err(error) => tracing::warn!("index pass for hosted project {id}: {error:?}"),
            }
        }
    }

    /// Run one index pass for hosted project `raw_id`, unless it has nothing
    /// to do or cannot run now (see [`IndexSkip`]). Never takes the append
    /// mutex except for the instant of each row insert.
    pub fn index_hosted(&self, raw_id: &str) -> Result<IndexOutcome, ProjectError> {
        let Some((id, entry)) = self.hosted_entry(raw_id) else {
            return Ok(IndexOutcome::Skipped(IndexSkip::NotHosted));
        };
        let mut state = match entry.pass.try_lock() {
            Ok(state) => state,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                return Ok(IndexOutcome::Skipped(IndexSkip::Busy));
            }
        };
        let dir = self.existing_project_dir(&id)?;
        let meta = self.load_meta(&id)?;
        let Some(root) = meta.mailspace_root.clone() else {
            return Ok(IndexOutcome::Skipped(IndexSkip::NotHosted));
        };
        let mailspace = open_host(&root)?;
        let mark = host_watermark(&mailspace)?;
        if state
            .mark
            .as_ref()
            .is_some_and(|(r, m)| *r == root && *m == mark)
        {
            return Ok(IndexOutcome::Skipped(IndexSkip::Unchanged));
        }
        let Some(_lock) = SyncGuard::try_acquire(&sync_dir(&mailspace.dir))? else {
            return Ok(IndexOutcome::Skipped(IndexSkip::SyncLocked));
        };
        let settings = host_settings(&mailspace, &id, &entry.sender)?;
        let transport = IndexTransport {
            project: RefCell::new(Project::open(&dir, &meta.mailspace_dir(&dir))?),
            sender: &entry.sender,
            prefix: entry.prefix.as_deref(),
            clock: &self.clock,
            appended: Cell::new(0),
        };
        let report = push::push(&mailspace, &settings, &transport)
            .map_err(storage_err("hosted index pass failed"))?;
        let refused: HashSet<String> = report.refused.iter().map(|r| r.handle.clone()).collect();
        for letter in report
            .refused
            .iter()
            .filter(|r| !state.refused.contains(&r.handle))
        {
            tracing::warn!(
                "hosted project {id}: letter {} not indexed: {}",
                letter.handle,
                letter.reason
            );
        }
        state.refused = refused;
        let stopped = report.stopped.map(|stop| stop.reason);
        if stopped.is_none() {
            // The mark read before the pass: a change during it runs again.
            state.mark = Some((root, mark));
        }
        if report.sent > 0 {
            self.wake.notify(&id);
        }
        Ok(IndexOutcome::Ran {
            indexed: report.sent,
            duplicates: report.duplicates,
            refused: state.refused.len(),
            stopped,
        })
    }

    /// Before a read of a hosted project: when the mailspace's change
    /// watermark moved since the last pass, run one. A pass already running
    /// is not waited for. Failures are logged; the read goes on.
    pub(super) fn refresh_hosted(&self, raw_id: &str, project: &Project) {
        let Some((id, entry)) = self.hosted_entry(raw_id) else {
            return;
        };
        let Ok(mark) = project.storage.change_watermark() else {
            return;
        };
        let moved = match entry.pass.try_lock() {
            Ok(state) => state.mark.as_ref().map(|(_, seen)| *seen) != Some(mark),
            Err(std::sync::TryLockError::Poisoned(_)) => true,
            Err(std::sync::TryLockError::WouldBlock) => false,
        };
        if moved && let Err(error) = self.index_hosted(&id) {
            tracing::warn!("pre-read index pass for hosted project {id}: {error:?}");
        }
    }

    /// Append to a hosted project (S5). The caller holds the append mutex.
    /// The decision matrix is the same as a server-owned project's: replays
    /// answer from the index; a fresh letter takes the next sequence, is
    /// filed in the host mailspace as a client pull files a record, and is
    /// then recorded in the transport index. Both writes are idempotent, so
    /// a crash between them converges on replay.
    pub(super) fn append_hosted(
        &self,
        raw_id: &str,
        meta: &ProjectMeta,
        item: &AppendItem,
    ) -> Result<AppendOutcome, ProjectError> {
        let dir = self.existing_project_dir(raw_id)?;
        let project_id = dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_owned();
        let (_, entry) = self.hosted_entry(&project_id).ok_or_else(|| {
            ProjectError::Storage("the hosted project is not registered; reconcile first".into())
        })?;
        let root = meta.mailspace_root.as_deref().ok_or_else(|| {
            ProjectError::Storage("a hosted project names no mailspace root".into())
        })?;
        let content_sha256 = sha256_hex(&item.raw_eml);
        let mut project = self.open_project(raw_id)?;
        if let Some(outcome) = project.find_replay(&item.id, &item.from, &content_sha256)? {
            return Ok(outcome);
        }
        let (sequence, previous) = project.tail()?;
        let appended_at = monotone_stamp(previous.as_deref(), (self.clock.0)());
        if item.is_governance() {
            // A vouch admits a key; the letter is kept verbatim like any
            // other, so the index can serve it. A pull files no governance
            // letter, so it does not go through `file_records`.
            self.process_vouch(&project_id, item)?;
            project.file_letter(&project_id, item)?;
        } else {
            let record = wire_record(item, &content_sha256, sequence, &appended_at);
            let filed = file_in_host(&dir, root, &project_id, &entry.sender, &record)?;
            if let Some(reason) = filed.refused {
                return Err(refusal(item, &reason));
            }
            if filed.governance {
                // To/Cc names postmaster though the envelope did not: the
                // pull counts it and files nothing, but the index must be
                // able to serve the bytes.
                project.file_letter(&project_id, item)?;
            }
        }
        project.record(item, &content_sha256, sequence, &appended_at)?;
        self.wake.notify(&project_id);
        Ok(AppendOutcome {
            message_id: item.id.clone(),
            sequence,
            content_sha256,
            duplicate: false,
        })
    }
}

/// The wire record a pull would read for this append.
fn wire_record(item: &AppendItem, content_sha256: &str, sequence: u64, at: &str) -> Record {
    Record {
        sequence,
        message_id: item.id.clone(),
        from: item.from.clone(),
        to: item.to.clone(),
        kind: item.kind.clone(),
        content_sha256: content_sha256.to_owned(),
        from_prefix: item.from_prefix.clone(),
        appended_at: Some(at.to_owned()),
        raw_eml: b64::encode(&item.raw_eml),
    }
}

/// What filing one record did that the caller must act on.
struct Filed {
    refused: Option<String>,
    governance: bool,
}

/// File `record` in the host mailspace under the project's persistent sync
/// state (`<project dir>/sync/state.json`).
fn file_in_host(
    dir: &Path,
    root: &Path,
    project_id: &str,
    sender: &str,
    record: &Record,
) -> Result<Filed, ProjectError> {
    let mailspace = open_host(root)?;
    let settings = host_settings(&mailspace, project_id, sender)?;
    let state_dir = dir.join(SYNC_STATE_DIR);
    let mut state = SyncState::load(&state_dir, project_id)
        .map_err(storage_err("failed to read the hosted sync state"))?;
    let before = state.clone();
    let report = file_records(
        &mailspace,
        &settings,
        &mut state,
        std::slice::from_ref(record),
    )
    .map_err(storage_err(
        "failed to file the letter in the host mailspace",
    ))?;
    if state != before {
        state
            .save(&state_dir)
            .map_err(storage_err("failed to save the hosted sync state"))?;
    }
    // A letter the filer quarantined is refused, never held: the server
    // discards its copy (a server copy must not hold a letter it cannot
    // read) and the member learns at once from the push answer, which
    // carries the quarantine reason in the refused-item shape.
    let refused = match report.quarantined.as_slice() {
        [] => report.refused.first().map(|r| r.reason.clone()),
        held => {
            for letter in held {
                quarantine::discard(&mailspace.dir, &letter.id)
                    .map_err(storage_err("failed to discard a quarantined letter"))?;
            }
            Some(
                held.iter()
                    .map(|letter| letter.reason.as_str())
                    .collect::<Vec<_>>()
                    .join("; "),
            )
        }
    };
    Ok(Filed {
        refused,
        governance: report.governance > 0,
    })
}

/// A record the filer refused, as the append error a client sees.
fn refusal(item: &AppendItem, reason: &str) -> ProjectError {
    if reason.contains("already names different content") {
        ProjectError::Conflict(format!(
            "item id {} already names a different letter in this project",
            item.id
        ))
    } else {
        ProjectError::BadRequest(format!("letter refused: {reason}"))
    }
}

fn open_host(root: &Path) -> Result<Mailspace, ProjectError> {
    Mailspace::open_root(root.to_path_buf())
        .map_err(storage_err("failed to open the host mailspace"))
}

fn host_watermark(mailspace: &Mailspace) -> Result<ChangeWatermark, ProjectError> {
    let mark = mailspace
        .storage()
        .and_then(|storage| storage.change_watermark());
    mark.map_err(storage_err("failed to read the host change watermark"))
}

/// The sync settings a hosted project's passes use: the project id, the
/// mailspace's own id, and its own `[sync] send` (never overridden).
fn host_settings(
    mailspace: &Mailspace,
    project_id: &str,
    sender: &str,
) -> Result<SyncSettings, ProjectError> {
    let send = mailspace
        .config
        .sync
        .as_ref()
        .and_then(|sync| sync.send.as_deref());
    Ok(SyncSettings {
        server: String::new(),
        project: project_id.to_owned(),
        name: sender.to_owned(),
        send: SendFilter::parse(send).map_err(storage_err("invalid [sync] send"))?,
        mailspace_id: mailspace.mailspace_id().to_owned(),
    })
}

/// The in-process transport an index pass pushes through: an append checks
/// the blob store holds the letter's hash and inserts the transport row.
struct IndexTransport<'a> {
    project: RefCell<Project>,
    sender: &'a str,
    prefix: Option<&'a str>,
    clock: &'a super::Clock,
    appended: Cell<usize>,
}

impl IndexTransport<'_> {
    fn index(&self, message_id: &str, envelope: &Envelope<'_>) -> Result<Appended, AppendFailure> {
        let sha = sha256_hex(envelope.raw_eml);
        let item = AppendItem::new(
            message_id,
            self.sender,
            Some(envelope.to),
            Some(envelope.kind),
            envelope.raw_eml.to_vec(),
        )
        .map_err(|error| AppendFailure::Refused(format!("{error:?}")))?
        .with_from_prefix(self.prefix);
        let stop = |error: ProjectError| AppendFailure::Stop(format!("{error:?}"));
        let mut project = self.project.borrow_mut();
        // The index is the only thing this writes; a letter whose blob the
        // store cannot back must never get a row.
        if !project.storage.blob_exists(&sha).unwrap_or(false) {
            return Err(AppendFailure::Refused(
                "the mailspace holds no blob under the letter's hash".into(),
            ));
        }
        let _guard = append_lock();
        if let Some(held) = project
            .find_replay(message_id, self.sender, &sha)
            .map_err(stop)?
        {
            return Ok(Appended {
                message_id: held.message_id,
                sequence: held.sequence,
                duplicate: true,
                content_sha256: Some(held.content_sha256),
            });
        }
        let (sequence, previous) = project.tail().map_err(stop)?;
        let at = monotone_stamp(previous.as_deref(), (self.clock.0)());
        project.record(&item, &sha, sequence, &at).map_err(stop)?;
        self.appended.set(self.appended.get() + 1);
        Ok(Appended {
            message_id: message_id.to_owned(),
            sequence,
            duplicate: false,
            content_sha256: Some(sha),
        })
    }
}

impl Transport for IndexTransport<'_> {
    fn append(&self, message_id: &str, envelope: &Envelope<'_>) -> Result<Appended, AppendFailure> {
        self.index(message_id, envelope)
    }

    fn fetch(&self, _query: &ListQuery<'_>) -> Result<Page, VivariumError> {
        Err(VivariumError::Message(
            "the hosted index transport only appends".into(),
        ))
    }
}

/// The host mailspace's sync lock (`<.vivi>/sync/sync.lock`), taken without
/// waiting. It is the same file and the same advisory lock as
/// `vivi::sync::lock::SyncLock`, which only offers a ten-second wait; the
/// host's own `vivi sync` and this pass exclude each other. Dropping it
/// releases the lock.
struct SyncGuard {
    _file: File,
}

impl SyncGuard {
    fn try_acquire(dir: &Path) -> Result<Option<Self>, ProjectError> {
        let io = |context: &'static str| {
            move |error: std::io::Error| {
                ProjectError::Storage(format!("{context} {}: {error}", dir.display()))
            }
        };
        std::fs::create_dir_all(dir).map_err(io("failed to create the sync dir"))?;
        let gitignore = dir.join(".gitignore");
        if !gitignore.exists() {
            std::fs::write(&gitignore, "*\n").map_err(io("failed to write the sync dir ignore"))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join("sync.lock"))
            .map_err(io("failed to open the sync lock in"))?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self { _file: file })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(error)) => Err(io("failed to lock the sync lock in")(error)),
        }
    }
}
