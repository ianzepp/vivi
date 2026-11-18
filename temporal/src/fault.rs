//! The scripted fault model, in the style of `lab/scenarios/*/plan.toml`.
//!
//! Each step of each slice gets a list of `kind@call` entries. `call` counts
//! every invocation of that step for that slice, Temporal retries included,
//! starting at 1. A slice can also be marked `bad`: its change passes its own
//! check but breaks the Lander's gate.

use crate::error::SpikeError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultKind {
    /// Provider 429 or spent quota before any work is done.
    Quota,
    /// The host goes dark after one pushed step: no more heartbeats.
    Lost,
    /// The agent keeps heartbeating without progress until the cap.
    Timeout,
    /// The check comes back red.
    Red,
    /// The reviewer asks for one change round.
    Changes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fault {
    pub kind: FaultKind,
    pub call: u32,
}

impl Fault {
    /// Parse one `kind@call` entry, for example `quota@1` or `lost@2`.
    pub fn parse(entry: &str) -> Result<Fault, SpikeError> {
        let bad = |reason: &str| SpikeError::FaultScript {
            entry: entry.to_string(),
            reason: reason.to_string(),
        };
        let (kind, call) = entry
            .split_once('@')
            .ok_or_else(|| bad("expected kind@call"))?;
        let kind = match kind {
            "quota" => FaultKind::Quota,
            "lost" => FaultKind::Lost,
            "timeout" => FaultKind::Timeout,
            "red" => FaultKind::Red,
            "changes" => FaultKind::Changes,
            _ => return Err(bad("unknown kind")),
        };
        let call: u32 = call.parse().map_err(|_| bad("call is not a number"))?;
        if call == 0 {
            return Err(bad("calls count from 1"));
        }
        Ok(Fault { kind, call })
    }

    pub fn parse_all(entries: &[&str]) -> Result<Vec<Fault>, SpikeError> {
        entries.iter().map(|e| Fault::parse(e)).collect()
    }
}

/// The fault to inject on a given call, if any.
pub fn fault_at(script: &[Fault], call: u32) -> Option<FaultKind> {
    script.iter().find(|f| f.call == call).map(|f| f.kind)
}

/// One slice of a scripted need.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SliceScript {
    pub id: String,
    pub write_scope: Vec<String>,
    pub implement: Vec<Fault>,
    pub check: Vec<Fault>,
    pub review: Vec<Fault>,
    /// Passes its own check, breaks the Lander's gate.
    pub bad: bool,
}

impl SliceScript {
    pub fn new(id: &str) -> SliceScript {
        SliceScript {
            id: id.to_string(),
            write_scope: vec![format!("src/{id}.rs")],
            ..SliceScript::default()
        }
    }

    pub fn implement(mut self, entries: &[&str]) -> Result<SliceScript, SpikeError> {
        self.implement = Fault::parse_all(entries)?;
        Ok(self)
    }

    pub fn check(mut self, entries: &[&str]) -> Result<SliceScript, SpikeError> {
        self.check = Fault::parse_all(entries)?;
        Ok(self)
    }

    pub fn review(mut self, entries: &[&str]) -> Result<SliceScript, SpikeError> {
        self.review = Fault::parse_all(entries)?;
        Ok(self)
    }

    pub fn scope(mut self, paths: &[&str]) -> SliceScript {
        self.write_scope = paths.iter().map(|p| p.to_string()).collect();
        self
    }

    pub fn bad(mut self) -> SliceScript {
        self.bad = true;
        self
    }
}

/// A scripted need: faults for its plan step and its slices.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NeedScript {
    pub plan: Vec<Fault>,
    pub slices: Vec<SliceScript>,
}

impl NeedScript {
    pub fn new(slices: Vec<SliceScript>) -> NeedScript {
        NeedScript {
            plan: Vec::new(),
            slices,
        }
    }

    pub fn plan(mut self, entries: &[&str]) -> Result<NeedScript, SpikeError> {
        self.plan = Fault::parse_all(entries)?;
        Ok(self)
    }
}

#[cfg(test)]
#[path = "fault_test.rs"]
mod tests;
