use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt::Write;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::boot::probe::ProbeConfig;
use crate::error::VivariumError;
use crate::judgment::Judgment;
use crate::pool::PoolConfig;
use crate::storage::Storage;
use crate::store::secure_create_dir_all;
use crate::sync::SyncConfig;
pub(crate) use config_file::PREFIX_LEN;
use config_file::{ensure_id, write_config};

mod address;
mod adoption;
mod archive;
mod backlog;
mod backlog_audit;
pub mod board;
mod body;
pub mod config_edit;
mod config_file;
mod delegation;
mod delivery;
mod dependencies;
mod dump;
mod event_log;
mod gitignore;
mod goals;
mod graph;
mod graph_mutate;
mod holding;
mod identity;
pub mod inherit;
mod item_events;
mod kind;
mod lifecycle;
mod marks;
pub mod memos;
mod merge;
mod mermaid;
#[cfg(test)]
#[path = "mailspace/message_id_test.rs"]
mod message_id_test;
mod pool_record;
mod pump_record;
mod reply;
mod report;
mod rules;
pub mod shipped;
mod starter_pump;
pub use starter_pump::STARTER_PUMP;
mod step;
#[cfg(test)]
mod tests;
mod thread;
mod trace;
mod watch;

pub use address::{Recipient, prefix_of_domain};
pub use archive::ArchiveExportReport;
pub use backlog::BacklogBoundUnit;
pub use backlog_audit::{BacklogAuditFinding, BacklogAuditReport, print_backlog_audit};
pub use body::{read_body_arg, read_body_input};
pub use config_edit::{IdentityPatch, PoolAdd};
pub use dump::{
    DumpFilters, DumpRecord, MailDumpRequest, TaskDumpRequest, TaskDumpStatus, parse_time_bound,
};
pub use goals::GoalView;
pub use graph::{
    GraphActionReceipt, GraphBlockedNode, GraphEdgeView, GraphFrontier, GraphFrontierGate,
    GraphImportReceipt, GraphNodeView, GraphShow, action_receipt_from_show, format_gates,
    frontier_from_show, print_action_receipt, print_blocked, print_frontier, print_frontiers,
    print_import_report, split_ready_gates,
};
pub use graph_mutate::{GraphApplyReceipt, print_apply_report};
pub use holding::is_foreign_account;
pub use identity::{
    LocalIdentity, ROLE_HARNESS_SUBAGENT, ROLE_STATUS_ACTIVE, ROLE_STATUS_PARKED,
    ROLE_STATUS_RETIRED, RoleUpdate, RoleView,
};
pub(crate) use item_events::EVENT_HEADER;
#[cfg(test)]
pub(crate) use item_events::with_clock_skew;
pub use item_events::{EventTally, ItemEventKind, ItemEventState};
pub(crate) use kind::effective_kind;
pub use lifecycle::{
    AbsorbConfig, CycleIntake, MailAbsorbFilter, SourceTaskRequest, SourceTaskResult,
    WantListOptions, WantListRecord, WantMetadataUpdate,
};
pub use marks::{MarkWrite, folded_marks, marks_include, validate_marks, validate_marks_filter};
pub use merge::{MailspaceImportOptions, MailspaceImportReport, import_mailspace};
pub use mermaid::is_gate_kind;
pub use pool_record::{POOL_EVENT_TYPES, PoolEventRow, PoolTask};
pub use pump_record::{PUMP_NEED_HEADER, PumpRecord, PumpState};
pub use report::MoveOutcome;
pub use rules::{RuleChange, RuleDetail, RuleRecord, RuleSupersession};
pub use step::{StepDispatch, StepException, StepManifest, body_has_labeled_clause};
pub use thread::{
    MailspaceThreadMessage, ThreadOutlineRow, print_thread, print_thread_newest,
    print_thread_outline,
};
pub use trace::{print_trace, print_trace_json};
pub use watch::{MailspaceWatchRequest, run_watch};

pub(crate) const MAILSPACE_DIR: &str = ".vivi";
pub(crate) const MAILSPACE_CONFIG: &str = "mailspace.toml";

pub struct Mailspace {
    pub root: PathBuf,
    pub dir: PathBuf,
    pub config: MailspaceConfig,
    /// One lazily-opened storage connection, reused by every `storage()`
    /// call on this mailspace. Opening per call re-ran pragmas, permission
    /// checks, and schema verification for each caller in one command.
    storage: RefCell<Option<Storage>>,
}

impl Clone for Mailspace {
    fn clone(&self) -> Self {
        Self {
            root: self.root.clone(),
            dir: self.dir.clone(),
            config: self.config.clone(),
            storage: RefCell::new(None),
        }
    }
}

impl std::fmt::Debug for Mailspace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mailspace")
            .field("root", &self.root)
            .field("dir", &self.dir)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailspaceConfig {
    pub name: String,
    /// This mailspace's id: a canonical lowercase hyphenated UUID, minted on
    /// first open and never changed. Its first 8 characters are the domain
    /// part of every address the mailspace mints. The file is machine-local
    /// and uncommitted, so the id never travels with the source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// `base = true` marks this file as a base config that projects below it
    /// inherit from (see [`inherit`]). Never inherited itself; a base is not
    /// a project, and the registry and pump refuse it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub base: bool,
    #[serde(default)]
    pub description: Option<String>,
    /// Path to a dedicated git repo that receives absorbed records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive: Option<String>,
    #[serde(default)]
    pub identities: Vec<LocalIdentity>,
    /// Project-declared boot probes.
    ///
    /// Boot renders the facts Vivi owns. Facts it cannot own — git ancestry,
    /// lane state, the live seat count of a subagent harness — arrive through
    /// these executables, so the command stays project-agnostic and Vivi never
    /// learns what a repository is. See [`crate::boot::probe`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub probes: Vec<ProbeConfig>,
    /// Judgment provider settings. Absent means the shadow screen is off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judgment: Option<Judgment>,
    /// Project letter sync settings. Absent means `vivi sync` is not set up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync: Option<SyncConfig>,
    /// Absorb ruling settings (the `[absorb]` table). Absent means the
    /// defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub absorb: Option<AbsorbConfig>,
    /// Memo listing settings (the `[memo]` table): hidden tags and per-tag
    /// retention. Absent means the defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memo: Option<memos::MemoConfig>,
    /// `[pump]`: settings of `vivi pump`. Absent means the pump is not
    /// configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pump: Option<crate::pump::config::PumpConfig>,
    /// Remote lane pools, by name: one `[pools.<name>]` table each. Absent
    /// means no pool.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pools: BTreeMap<String, PoolConfig>,
    /// The retired single `[pool]` table. Kept only so a `vivi pool` command
    /// can reject it with a message naming `[pools.<name>]`, and so a config
    /// rewrite does not erase it.
    #[serde(default, rename = "pool", skip_serializing_if = "Option::is_none")]
    pub retired_pool: Option<toml::Value>,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, Serialize)]
pub struct MailspaceStatus {
    pub found: bool,
    /// Version of the vivi binary producing this status — surfaces stale
    /// binaries (e.g. an old Homebrew install shadowing PATH) that predate
    /// backlog minting and silently skip graph citizenship.
    pub vivi: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive: Option<String>,
    pub root: PathBuf,
    pub store: PathBuf,
    pub identities: Vec<IdentityStatus>,
    pub totals: StatusTotals,
}

#[derive(Debug, Clone, Serialize)]
pub struct IdentityStatus {
    pub identity: String,
    pub address: String,
    pub aliases: Vec<String>,
    pub actionable_open: usize,
    pub inbox_unread: usize,
    pub tasks_open: usize,
    pub needs_open: usize,
    pub wants_open: usize,
    pub memos_open: usize,
    pub done: usize,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct StatusTotals {
    pub actionable_open: usize,
    pub inbox_unread: usize,
    pub tasks_open: usize,
    pub needs_open: usize,
    pub wants_open: usize,
    pub memos_open: usize,
}

#[derive(Debug, Clone)]
pub struct SendRequest {
    pub from: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: String,
    pub body: String,
    pub role: String,
    pub kind: Option<String>,
    pub reply_to: Option<String>,
    pub depends_on: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct DeliveryResult {
    pub delivered: Vec<DeliveredMessage>,
    pub sent: String,
    /// Recipients in other mailspaces (`role@<prefix>.local`, lowercased,
    /// sorted). They got no delivered copy; the letter reaches them when
    /// `vivi sync` pushes the sent copy.
    pub remote: Vec<String>,
}

/// What `mail deliver` stored for one raw letter.
#[derive(Debug, Clone)]
pub struct RawDelivery {
    /// One delivered copy per local recipient.
    pub delivered: Vec<DeliveredMessage>,
    /// The handle of the sender's own copy, kept only when no recipient is a
    /// role of this mailspace: the letter then has to live somewhere for
    /// `vivi sync` to carry it.
    pub sent: Option<String>,
    /// Recipients in other mailspaces, as in [`DeliveryResult::remote`].
    pub remote: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeliveredMessage {
    pub identity: String,
    pub handle: String,
}

impl Mailspace {
    /// Construct a mailspace view from already-parsed config without running
    /// open-time identity or ignore-file repairs.
    pub(crate) fn from_read_only_config(root: PathBuf, config: MailspaceConfig) -> Self {
        Self {
            dir: root.join(MAILSPACE_DIR),
            root,
            config,
            storage: RefCell::new(None),
        }
    }

    /// Initialise a new mailspace at the given project path, or the current
    /// directory. Creates the `.vivi` directory, config, and store.
    ///
    /// # Errors
    /// Returns an error if directory creation, config writing, or store
    /// initialisation fails.
    pub fn init(project: Option<&Path>) -> Result<Self, VivariumError> {
        let root = project_root(project)?;
        let dir = root.join(MAILSPACE_DIR);
        secure_create_dir_all(&dir)?;
        secure_create_dir_all(&dir.join("blobs"))?;
        gitignore::ensure_ignore_file(&dir);
        let path = dir.join(MAILSPACE_CONFIG);
        if path.exists() {
            return Self::open_root(root);
        }
        // The id is minted by the open below, under the config lock.
        let config = MailspaceConfig {
            name: default_name(&root),
            id: None,
            base: false,
            description: None,
            archive: None,
            identities: Vec::new(),
            probes: Vec::new(),
            judgment: None,
            sync: None,
            absorb: None,
            memo: None,
            pump: None,
            pools: BTreeMap::new(),
            retired_pool: None,
        };
        write_config(&path, &config)?;
        Storage::open_mailspace(&dir)?;
        Self::open_root(root)
    }

    /// Discover a mailspace by searching ancestors from the given project
    /// path, or the current directory.
    ///
    /// # Errors
    /// Returns an error if no mailspace config is found in the ancestor chain,
    /// or if reading the config file fails.
    pub fn discover(project: Option<&Path>) -> Result<Self, VivariumError> {
        if let Some(project) = project {
            return Self::open_root(project.to_path_buf());
        }
        let cwd = std::env::current_dir()?;
        let Some(root) = find_root(&cwd) else {
            return Err(missing_mailspace_error(&cwd));
        };
        Self::open_root(root)
    }

    /// Open a mailspace at an explicit root path.
    ///
    /// The config is the project's file merged over any base configs above
    /// it (see [`inherit`]). A config without an id gets one minted and persisted here, under the
    /// config lock, so the mailspace's addresses are stable from the first
    /// open.
    ///
    /// # Errors
    /// Returns an error if the config file cannot be read or parsed, its id
    /// is malformed, or a new id cannot be persisted.
    pub fn open_root(root: PathBuf) -> Result<Self, VivariumError> {
        let dir = root.join(MAILSPACE_DIR);
        let path = dir.join(MAILSPACE_CONFIG);
        let raw = fs::read_to_string(&path).map_err(|e| {
            VivariumError::Message(format!(
                "failed to read mailspace config at {}: {e}",
                path.display()
            ))
        })?;
        let mut config = inherit::load_config(&path, &raw)?;
        ensure_id(&path, &mut config)?;
        if gitignore::ensure_ignore_file(&dir) {
            eprintln!(
                "vivi: wrote {} (nothing in .vivi is committed)",
                dir.join(".gitignore").display()
            );
        }
        Ok(Self {
            root,
            dir,
            config,
            storage: RefCell::new(None),
        })
    }

    /// A warning line when git tracks `.vivi/mailspace.toml`, which would
    /// make every clone share this mailspace's id. Best effort and silent
    /// when `git` is missing or the project is not a repository. Runs git,
    /// so only status-style commands call it.
    #[must_use]
    pub fn tracked_config_warning(&self) -> Option<String> {
        let relpath = format!("{MAILSPACE_DIR}/{MAILSPACE_CONFIG}");
        gitignore::tracked_config_warning(&self.root, &relpath)
    }

    #[must_use]
    pub fn store_path(&self) -> PathBuf {
        self.dir.join("mail.sqlite")
    }

    /// Open the `SQLite` storage for this mailspace.
    ///
    /// The connection is opened once and cached on this mailspace; repeated
    /// calls share it. The guard derefs to [`Storage`], whose methods all
    /// take `&self`.
    ///
    /// # Errors
    /// Returns an error if the database cannot be opened or migrations fail.
    pub fn storage(&self) -> Result<std::cell::Ref<'_, Storage>, VivariumError> {
        if self.storage.borrow().is_none() {
            let opened = Storage::open_mailspace(&self.dir)?;
            *self.storage.borrow_mut() = Some(opened);
        }
        std::cell::Ref::filter_map(self.storage.borrow(), Option::as_ref)
            .map_err(|_| VivariumError::Other("storage is not available".into()))
    }

    /// Path of `.vivi/mailspace.toml`.
    #[must_use]
    pub fn config_path(&self) -> PathBuf {
        self.dir.join(MAILSPACE_CONFIG)
    }

    /// Set the mailspace description.
    ///
    /// # Errors
    /// Returns an error if the config cannot be written.
    pub fn set_description(&mut self, description: Option<String>) -> Result<(), VivariumError> {
        self.config.description.clone_from(&description);
        config_edit::apply(&self.config_path(), |doc| {
            match &description {
                Some(text) => doc["description"] = toml_edit::value(text.as_str()),
                None => {
                    doc.as_table_mut().remove("description");
                }
            }
            Ok(())
        })
    }

    /// Set or clear the historical archive git repository path.
    ///
    /// Relative paths resolve against the mailspace project root. `~` expands.
    /// The resolved path must be an existing git repository.
    ///
    /// # Errors
    /// Returns an error if the path is not a git repo or the config cannot be written.
    pub fn set_archive(&mut self, archive: Option<String>) -> Result<(), VivariumError> {
        if let Some(raw) = archive.as_deref() {
            archive::validate_archive_repo(&archive::resolve_archive_path(&self.root, raw)?)?;
        }
        self.config.archive.clone_from(&archive);
        config_edit::apply(&self.config_path(), |doc| {
            match &archive {
                Some(text) => doc["archive"] = toml_edit::value(text.as_str()),
                None => {
                    doc.as_table_mut().remove("archive");
                }
            }
            Ok(())
        })
    }

    /// Write every absorbed record into the configured archive repo.
    ///
    /// Existing files are left untouched when the rendered bytes match.
    ///
    /// # Errors
    /// Returns an error if no archive is configured, the archive path is not a
    /// git repo, or a storage or filesystem operation fails.
    pub fn export_archive(&self) -> Result<ArchiveExportReport, VivariumError> {
        archive::export_all(self)
    }

    /// Return a status summary for the mailspace and all identities.
    ///
    /// # Errors
    /// Returns an error if the storage cannot be opened or queries fail.
    pub fn status(&self) -> Result<MailspaceStatus, VivariumError> {
        let storage = self.storage()?;
        let mut identities = Vec::new();
        let mut totals = StatusTotals::default();
        for identity in &self.config.identities {
            let names = self.identity_names(&identity.name);
            let mut inbox_unread = 0;
            let mut tasks_open = 0;
            let mut needs_open = 0;
            let mut wants_open = 0;
            let mut memos_open = 0;
            let mut done = 0;
            for name in &names {
                inbox_unread +=
                    storage.count_messages_for_account_role(name, "inbox", Some(false))?;
                tasks_open += storage.count_messages_for_account_role(name, "tasks", None)?;
                needs_open += storage.count_messages_for_account_role(name, "needs", None)?;
                wants_open += storage.count_messages_for_account_role(name, "wants", None)?;
                memos_open += storage.count_messages_for_account_role(name, "memos", None)?;
                done += storage.count_messages_for_account_role(name, "done", None)?;
            }
            let actionable_open = tasks_open + needs_open;
            totals.actionable_open += actionable_open;
            totals.inbox_unread += inbox_unread;
            totals.tasks_open += tasks_open;
            totals.needs_open += needs_open;
            totals.wants_open += wants_open;
            totals.memos_open += memos_open;
            identities.push(IdentityStatus {
                identity: identity.name.clone(),
                address: self.address_for(&identity.name),
                aliases: identity.aliases.clone(),
                actionable_open,
                inbox_unread,
                tasks_open,
                needs_open,
                wants_open,
                memos_open,
                done,
            });
        }
        Ok(MailspaceStatus {
            found: true,
            vivi: env!("CARGO_PKG_VERSION").to_string(),
            name: self.config.name.clone(),
            description: self.config.description.clone(),
            archive: self.config.archive.clone(),
            root: self.root.clone(),
            store: self.store_path(),
            identities,
            totals,
        })
    }
}

pub fn print_status(status: &MailspaceStatus) {
    println!("mailspace {}", status.name);
    println!("vivi      {}", status.vivi);
    if let Some(description) = &status.description {
        println!("descr     {description}");
    }
    if let Some(archive) = &status.archive {
        println!("archive   {archive}");
    }
    println!("root      {}", status.root.display());
    println!("store     {}", status.store.display());
    println!();
    println!(
        "identity  actionable  tasks open  needs open  wants open  memos open  inbox unread  done"
    );
    for identity in &status.identities {
        println!(
            "{:<9} {:<11} {:<11} {:<11} {:<11} {:<11} {:<13} {}",
            identity.identity,
            identity.actionable_open,
            identity.tasks_open,
            identity.needs_open,
            identity.wants_open,
            identity.memos_open,
            identity.inbox_unread,
            identity.done
        );
        if !identity.aliases.is_empty() {
            println!("  formerly: {}", identity.aliases.join(", "));
        }
    }
    println!();
    println!("total actionable open: {}", status.totals.actionable_open);
    println!("total unread mail: {}", status.totals.inbox_unread);
    println!("total open tasks: {}", status.totals.tasks_open);
    println!("total open needs: {}", status.totals.needs_open);
    println!("total open wants: {}", status.totals.wants_open);
    println!("total open memos: {}", status.totals.memos_open);
}

/// Canonicalise a local folder/role name (e.g. `inbox`, `tasks`, `done`).
///
/// # Errors
/// Returns an error if the role name is not recognised.
pub fn canonical_local_role(role: &str) -> Result<String, VivariumError> {
    match role.to_ascii_lowercase().as_str() {
        "inbox" => Ok("inbox".into()),
        "archive" | "all" => Ok("archive".into()),
        "trash" | "deleted" => Ok("trash".into()),
        "sent" => Ok("sent".into()),
        "draft" | "drafts" => Ok("drafts".into()),
        "task" | "tasks" | "open" => Ok("tasks".into()),
        "need" | "needs" => Ok("needs".into()),
        "want" | "wants" => Ok("wants".into()),
        "pump" | "pumps" => Ok("pumps".into()),
        "memo" | "memos" => Ok("memos".into()),
        "rule" | "rules" => Ok("rules".into()),
        "done" | "closed" => Ok("done".into()),
        _ => Err(VivariumError::Message(format!(
            "unsupported local folder '{role}'; expected inbox, archive, trash, sent, drafts, tasks, needs, wants, pumps, memos, rules, or done"
        ))),
    }
}

fn project_root(project: Option<&Path>) -> Result<PathBuf, VivariumError> {
    match project {
        Some(path) => Ok(path.to_path_buf()),
        None => std::env::current_dir().map_err(Into::into),
    }
}

fn find_root(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|path| path.join(MAILSPACE_DIR).join(MAILSPACE_CONFIG).is_file())
        .map(Path::to_path_buf)
}

fn missing_mailspace_error(cwd: &Path) -> VivariumError {
    let mut message = format!("No Vivi mailspace found.\ncwd: {}", cwd.display());
    if let Some(git) = nearest_git_root(cwd) {
        let _ = write!(
            message,
            "\nnearest git root: {}\ninit: vivi mailspace init --project {}",
            git.display(),
            git.display()
        );
    } else {
        message.push_str("\ninit: vivi mailspace init");
    }
    VivariumError::Message(message)
}

fn nearest_git_root(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|path| path.join(".git").exists())
        .map(Path::to_path_buf)
}

fn default_name(root: &Path) -> String {
    let name = root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("project");
    sanitize_project_name(name)
}

fn sanitize_project_name(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if matches!(ch, '-' | '_' | '.') && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        "project".into()
    } else {
        out
    }
}
