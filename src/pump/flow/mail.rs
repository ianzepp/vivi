//! The flow engine's port onto the mailspace: every read and write a run needs.

use crate::error::VivariumError;
use crate::mailspace::Mailspace;

pub mod close;
pub mod read;
pub mod send;

/// The mailspace and the identity the pump writes as.
pub struct MailspacePort {
    pub mailspace: Mailspace,
    pub pump_identity: String,
}

/// Open or closed, as read from a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Open,
    Done,
}

/// A task's state: whether it is done, who closed it, and with what verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskState {
    pub status: TaskStatus,
    pub closer: Option<String>,
    pub verdict: Option<String>,
}

/// Open or closed, as read from a need.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NeedStatus {
    Open,
    Done,
}

/// The write side of the port.
pub trait MailWriter {
    /// Send a step task; returns its handle.
    fn send_step(
        &mut self,
        from: &str,
        to: &str,
        subject: &str,
        body: &str,
        depends_on: &[String],
    ) -> Result<String, VivariumError>;
    /// Bind the units that implement a need.
    fn bind(&mut self, need: &str, units: &[String]) -> Result<(), VivariumError>;
    /// Reply on a task.
    fn reply(&mut self, handle: &str, body: &str) -> Result<(), VivariumError>;
    /// Post a note on a need, addressed to the need's own recipient set (its
    /// `From`, `To` and `Cc`, minus the pump identity), so it reaches every
    /// member that holds the need. `reply` goes to the sender only.
    fn post_to_need_audience(&mut self, need: &str, body: &str) -> Result<(), VivariumError>;
    /// Close a task with a verdict and, for a merge, its branch and commit.
    fn close_task(
        &mut self,
        handle: &str,
        verdict: &str,
        branch: Option<&str>,
        commit: Option<&str>,
    ) -> Result<(), VivariumError>;
    /// Reopen a closed task.
    fn reopen_task(&mut self, handle: &str) -> Result<(), VivariumError>;
    /// Close a need with a note.
    fn close_need(&mut self, need: &str, note: &str) -> Result<(), VivariumError>;
    /// Open the pump record on a need; returns its handle.
    fn open_record(&mut self, need: &str, body: &str) -> Result<String, VivariumError>;
    /// Close the pump record with an outcome.
    fn close_record(&mut self, need: &str, outcome: &str) -> Result<(), VivariumError>;
}

/// The read side of the port.
pub trait MailReader {
    fn task_state(&self, handle: &str) -> Result<TaskState, VivariumError>;
    fn need_state(&self, need: &str) -> Result<NeedStatus, VivariumError>;
    /// The marks (labels) on a need.
    fn need_marks(&self, need: &str) -> Result<Vec<String>, VivariumError>;
    /// The open pump record on a need: its handle and body.
    fn open_record(&self, need: &str) -> Result<Option<(String, String)>, VivariumError>;
    /// The units bound to a need.
    fn bound_units(&self, need: &str) -> Result<Vec<String>, VivariumError>;
    /// How many run sentinels the need has.
    fn sentinel_count(&self, need: &str) -> Result<usize, VivariumError>;
}

impl MailWriter for MailspacePort {
    fn send_step(
        &mut self,
        from: &str,
        to: &str,
        subject: &str,
        body: &str,
        depends_on: &[String],
    ) -> Result<String, VivariumError> {
        MailspacePort::send_step(self, from, to, subject, body, depends_on)
    }

    fn bind(&mut self, need: &str, units: &[String]) -> Result<(), VivariumError> {
        MailspacePort::bind(self, need, units)
    }

    fn reply(&mut self, handle: &str, body: &str) -> Result<(), VivariumError> {
        MailspacePort::reply(self, handle, body)
    }

    fn post_to_need_audience(&mut self, need: &str, body: &str) -> Result<(), VivariumError> {
        MailspacePort::post_to_need_audience(self, need, body)
    }

    fn close_task(
        &mut self,
        handle: &str,
        verdict: &str,
        branch: Option<&str>,
        commit: Option<&str>,
    ) -> Result<(), VivariumError> {
        MailspacePort::close_task(self, handle, verdict, branch, commit)
    }

    fn reopen_task(&mut self, handle: &str) -> Result<(), VivariumError> {
        MailspacePort::reopen_task(self, handle)
    }

    fn close_need(&mut self, need: &str, note: &str) -> Result<(), VivariumError> {
        MailspacePort::close_need(self, need, note)
    }

    fn open_record(&mut self, need: &str, body: &str) -> Result<String, VivariumError> {
        self.open_pump_record(need, body)
    }

    fn close_record(&mut self, need: &str, outcome: &str) -> Result<(), VivariumError> {
        self.close_pump_record(need, outcome)
    }
}
