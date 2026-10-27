//! Private value types shared by the hosted-index pipeline modules.
//!
//! These are the candidate rows, groups, discovery, frontier, preparation,
//! selection, and send results the server-only incremental path passes
//! between its stages. They are undecorated on purpose: no wire form, no
//! persistence, no behavior beyond what the stage seams need. The live pass
//! is still `push::push` in the parent module, and the scope ruling holds:
//! "The generic CLI sync implementation is not changed."

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use crate::storage::{ChangeWatermark, RemoteBindingInput};
use crate::sync::push::{PushReport, Refused};
use crate::sync::select::{Letter, Skipped};

/// One message row a pass may classify, narrowed to what the pipeline
/// reads: identity, content, account, RFC identity, and any remote binding.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(super) struct CandidateRow {
    /// The handle the sender knows the letter by.
    pub handle: String,
    /// The content id every copy of the letter shares.
    pub content_id: String,
    /// The row's account: where a fresh binding would be written.
    pub account: String,
    /// The row's normalized RFC Message-ID, when it carries one: the
    /// identity dependent events are woken by.
    pub rfc_id: Option<String>,
    /// The row's remote binding, when it has one.
    pub remote: Option<RemoteBindingInput>,
}

/// Every row holding one content id: one letter, however many copies the
/// mailspace holds.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(not(test), allow(dead_code))]
pub(super) struct ContentGroup {
    pub content_id: String,
    pub rows: Vec<CandidateRow>,
}

/// What one pass's discovery saw: the change watermark sampled when
/// discovery began (the upper mark the work found under it covers, and the
/// mark a completed frontier may be advanced to), and the groups not yet
/// settled in the index.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(super) struct Discovery {
    pub upper: ChangeWatermark,
    pub groups: Vec<ContentGroup>,
}

/// How far a project's index work has come: the host root and mailspace
/// identity the work covered, and the watermark it reached. A frontier
/// compares equal only for the same mailspace, so a root re-pointed at a
/// different mailspace never reads as reached.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(super) struct Frontier {
    /// The host mailspace root the work ran against.
    pub root: PathBuf,
    /// The mailspace's own id, read from the root the frontier names.
    pub mailspace_id: String,
    /// The watermark the completed work covers.
    pub upper: ChangeWatermark,
}

/// One letter ready to send: the wire letter plus the blob path its bytes
/// already live at, so a send never re-reads the store.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(super) struct PreparedLetter {
    pub letter: Letter,
    pub blob_path: PathBuf,
}

/// One content id whose send waits on another letter, named by the
/// normalized RFC Message-ID that wakes it when that letter arrives.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(super) struct WaitingOn {
    pub wakes_on: String,
}

/// What one pass would send, keyed by content id, with what it refuses to
/// send and what it waits for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(not(test), allow(dead_code))]
pub(super) struct Selection {
    /// Letters ready to send, keyed by content id.
    pub ready: BTreeMap<String, PreparedLetter>,
    /// Why each content id is not sent (policy, size, a foreign From),
    /// keyed by content id.
    pub skipped: BTreeMap<String, Vec<Skipped>>,
    /// Content ids waiting on another letter, keyed by content id.
    pub waiting: BTreeMap<String, WaitingOn>,
}

/// What one send over the prepared letters did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(not(test), allow(dead_code))]
pub(super) struct SendOutcome {
    /// The push report the send produced.
    pub report: PushReport,
    /// Letters the index refused, keyed by content id.
    pub refused: BTreeMap<String, Vec<Refused>>,
    /// Content ids newly bound to this project by the send.
    pub bound: BTreeSet<String>,
}
