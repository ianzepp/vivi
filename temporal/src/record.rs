//! The Vivi record, as the workflows see it: status updates, parks with a
//! typed cause, and closes. The real implementation will call the vivi
//! server over HTTP/MCP; the spike keeps it in memory.

use std::sync::Mutex;

use crate::error::SpikeError;
use crate::types::RecordCall;

pub trait NeedRecord: Send + Sync {
    fn apply(&self, call: &RecordCall) -> Result<(), SpikeError>;
}

/// An in-memory record that keeps every call in order.
#[derive(Debug, Default)]
pub struct MemoryRecord {
    calls: Mutex<Vec<RecordCall>>,
}

impl MemoryRecord {
    pub fn calls(&self) -> Result<Vec<RecordCall>, SpikeError> {
        Ok(self.calls.lock()?.clone())
    }

    /// Calls for one need, in order.
    pub fn calls_for(&self, need: &str) -> Result<Vec<RecordCall>, SpikeError> {
        let calls = self.calls()?;
        Ok(calls.into_iter().filter(|c| call_need(c) == need).collect())
    }
}

impl NeedRecord for MemoryRecord {
    fn apply(&self, call: &RecordCall) -> Result<(), SpikeError> {
        self.calls.lock()?.push(call.clone());
        Ok(())
    }
}

fn call_need(call: &RecordCall) -> &str {
    match call {
        RecordCall::Status { need, .. }
        | RecordCall::Park { need, .. }
        | RecordCall::Close { need, .. } => need,
    }
}
