//! Private, lossless receipts for dispatched pool jobs.
//!
//! [`save_receipt`] stores each job's exact pool reply under the pump home
//! at `job-replies/<job>.txt`, so a diagnostic caller can read the full
//! worker receipt without the truncation the extracted-message pipeline
//! applies. [`ReplyContext`] carries the dispatch coordinates a caller
//! pairs with a saved receipt when it reports on one.

mod storage;

pub use storage::save_receipt;

/// The directory under the pump home where job receipts are saved.
pub(crate) const REPLY_DIR: &str = "job-replies";

/// The dispatch coordinates one saved receipt belongs to, borrowed for a
/// diagnostic caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplyContext<'a> {
    /// The need thread the job ran for.
    pub need: &'a str,
    /// The run number inside the need.
    pub run: u32,
    /// The flow-step node the dispatch came from.
    pub node: &'a str,
    /// The mailspace task handle, once the dispatch carried one.
    pub task: Option<&'a str>,
}
