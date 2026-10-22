//! MCP tools for a hosted project: the mailspace is the host's own, so a
//! tool acts on it through the `vivi` library exactly as the `vivi` CLI
//! would, as a vivi identity, and not through the server's transport index.
//!
//! The acting identity is the API key's `identity`, else the server's
//! `HOSTED_DEFAULT_IDENTITY` (default `operator`). It must be a role of the
//! hosted mailspace. A member-token caller (not an API key) never reaches
//! this module; it keeps the transport tools.
//!
//! Reads (`overview`, `list`, `show`, `thread`, `graph`, `board`) are live
//! queries of the mailspace store: the folder, status and marks the CLI
//! shows. Writes (`send`, `reply`, `done`, `reopen`, `memo`, `mark`,
//! `unmark`) call the same library entry points the CLI calls, so letters
//! land in the mailspace's role folders where the pump sees them next tick.
//! Each call opens the mailspace afresh (a tool call is short-lived and the
//! pump is another process on the same files).

mod pump;
mod read;
mod write;

use std::path::Path;

use serde_json::Value;
use vivi::mailspace::Mailspace;
use vivi::storage::{MailspaceEvent, Storage, StoredMessageView};

use crate::projects::ProjectStore;
use crate::safety::RateLimiter;

use super::tools::ToolError;

pub(super) use pump::pump_control;

/// The tools a hosted project answers through the library.
pub(super) fn handles(name: &str) -> bool {
    matches!(
        name,
        "overview"
            | "list"
            | "show"
            | "thread"
            | "graph"
            | "board"
            | "send"
            | "reply"
            | "done"
            | "reopen"
            | "memo"
            | "mark"
            | "unmark"
    )
}

/// The hosted mailspace root of `project_id`, `None` for a project the
/// server owns end to end.
pub(super) fn hosted_root(store: &ProjectStore, project_id: &str) -> Option<std::path::PathBuf> {
    store.load_meta(project_id).ok()?.mailspace_root
}

/// An opened hosted mailspace and the identity a call acts as.
pub(super) struct Hosted {
    pub(super) ms: Mailspace,
    /// The canonical role name the call acts as.
    pub(super) actor: String,
}

impl Hosted {
    /// Open the mailspace at `root` and resolve `requested` to one of its
    /// roles. An identity the mailspace does not have is an error that says
    /// so, never a silent fall back to another identity.
    pub(super) fn open(root: &Path, requested: &str) -> Result<Self, ToolError> {
        let ms = Mailspace::open_root(root.to_path_buf()).map_err(|error| {
            ToolError::new(format!("cannot open the project mailspace: {error}"))
        })?;
        let actor = ms.resolve_identity(requested).map_err(|error| {
            ToolError::new(format!(
                "this key acts as identity '{requested}', which is not a role of the project \
                 mailspace ({error}); mint the key with an identity the mailspace has"
            ))
        })?;
        Ok(Self { ms, actor })
    }
}

pub(super) fn lib_error(error: impl std::fmt::Display) -> ToolError {
    ToolError::new(error.to_string())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn dispatch(
    limiter: &RateLimiter,
    project_id: &str,
    root: &Path,
    requested: &str,
    name: &str,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let mut hosted = Hosted::open(root, requested)?;
    // `done`, `reopen`, `mark` and `unmark` take `--for <holder>` in the CLI:
    // the role the command runs as, which must hold the item. The library
    // applies the same authority rules to it as it does for the CLI.
    if matches!(name, "done" | "reopen" | "mark" | "unmark") {
        if let Some(holder) = super::tools::opt_str(arguments, "for")? {
            hosted.actor = hosted.ms.resolve_identity(holder).map_err(|error| {
                ToolError::new(format!(
                    "for: '{holder}' is not a role of the project mailspace ({error})"
                ))
            })?;
        }
    }
    match name {
        "overview" => read::overview(&hosted, project_id),
        "list" => read::list(&hosted, arguments),
        "show" => read::show(&hosted, arguments),
        "thread" => read::thread(&hosted, arguments),
        "graph" => read::graph(&hosted, arguments),
        "board" => read::board(&hosted, arguments),
        "send" => write::send(&hosted, limiter, project_id, arguments),
        "reply" => write::reply(&hosted, limiter, project_id, arguments),
        "done" => write::settle(&hosted, limiter, project_id, arguments, true),
        "reopen" => write::settle(&hosted, limiter, project_id, arguments, false),
        "memo" => write::memo(&hosted, limiter, project_id, arguments),
        "mark" => write::mark(&hosted, limiter, project_id, arguments, true),
        "unmark" => write::mark(&hosted, limiter, project_id, arguments, false),
        other => Err(ToolError::new(format!("unknown tool '{other}'"))),
    }
}

/// The work kinds a handle can name.
pub(super) fn is_work(kind: &str) -> bool {
    matches!(kind, "need" | "task" | "want")
}

/// `done` for the done folder, `open` for every other.
pub(super) fn status_for_role(role: &str) -> &'static str {
    if role == "done" { "done" } else { "open" }
}

/// The item's kind as the mailspace derives it: its folder, else the kind
/// the letter declares, else the kind its lifecycle events name, else mail.
pub(super) fn kind_of(raw: &[u8], role: &str, events: &[MailspaceEvent]) -> String {
    match role {
        "needs" => return "need".into(),
        "tasks" => return "task".into(),
        "wants" => return "want".into(),
        "memos" => return "memo".into(),
        _ => {}
    }
    if let Some(declared) = super::letters::parse_headers(raw).kind_header {
        let declared = declared.to_ascii_lowercase();
        if is_work(&declared) || declared == "memo" {
            return declared;
        }
    }
    for event in events.iter().rev() {
        if let Some(kind) = event.command.split_whitespace().next() {
            if is_work(kind) || kind == "memo" {
                return kind.to_owned();
            }
        }
    }
    "mail".into()
}

/// One stored message: its row, bytes and event log, as a tool reads them.
pub(super) struct Located {
    pub(super) view: StoredMessageView,
    pub(super) raw: Vec<u8>,
    pub(super) events: Vec<MailspaceEvent>,
    pub(super) kind: String,
}

/// Resolve a handle (or unambiguous prefix) to its message.
pub(super) fn locate(storage: &Storage, handle: &str) -> Result<Located, ToolError> {
    let token = storage.resolve_message_token(handle).map_err(lib_error)?;
    let view = storage
        .message_by_id(&token)
        .map_err(lib_error)?
        .ok_or_else(|| ToolError::new(format!("not found: {handle}")))?;
    let raw = storage.read_message(&view.message_id).map_err(lib_error)?;
    let events = storage
        .list_mailspace_events(&view.message_id)
        .map_err(lib_error)?;
    let kind = kind_of(&raw, &view.local_role, &events);
    Ok(Located {
        view,
        raw,
        events,
        kind,
    })
}
