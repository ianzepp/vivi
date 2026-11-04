//! The simulated host: the pump's only door to the outside world.
//!
//! Time is virtual: [`SimHost::now`] reads a counter the driver advances, so a
//! simulated hour costs nothing. Every command the pump runs is answered by
//! the world's rules and recorded. A command no rule answers is still
//! answered (exit 0, no output) but counted as unscripted, so a scenario shows
//! which parts of the pump's traffic the world does not model yet.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use vivi_core::pump::host::{CmdOutput, Host};

/// The virtual clock's start: 2026-01-01T00:00:00Z.
pub const T0: u64 = 1_767_225_600;

/// What the host saw during a run.
#[derive(Debug, Default)]
pub struct Record {
    pub commands: usize,
    pub spawned: usize,
    /// Commands no world rule answered, keyed by their first words.
    pub unscripted: BTreeMap<String, usize>,
    /// `kind unit` for each exception the pump logged.
    pub exceptions: Vec<String>,
    /// Every log line, in order.
    pub logs: Vec<String>,
}

pub struct SimHost {
    clock: AtomicU64,
    next_pid: AtomicU64,
    record: Mutex<Record>,
}

impl SimHost {
    pub fn new() -> SimHost {
        SimHost {
            clock: AtomicU64::new(T0),
            next_pid: AtomicU64::new(1000),
            record: Mutex::new(Record::default()),
        }
    }

    /// Move the virtual clock forward.
    pub fn advance(&self, secs: u64) {
        self.clock.fetch_add(secs, Ordering::SeqCst);
    }

    /// Take everything recorded so far.
    pub fn take_record(&self) -> Record {
        std::mem::take(&mut *self.lock())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Record> {
        self.record
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Host for SimHost {
    fn now(&self) -> u64 {
        self.clock.load(Ordering::SeqCst)
    }

    fn run(&self, cmd: &str, _cwd: Option<&Path>, _timeout_secs: u64) -> CmdOutput {
        let answer = world_answer(cmd);
        let mut record = self.lock();
        record.commands += 1;
        if answer.is_none() {
            *record.unscripted.entry(shape(cmd)).or_default() += 1;
        }
        answer.unwrap_or(CmdOutput {
            code: 0,
            text: String::new(),
        })
    }

    fn spawn(
        &self,
        _cmd: &str,
        _cwd: Option<&Path>,
        _stdin: Option<&Path>,
        _out: &Path,
    ) -> Option<u32> {
        self.lock().spawned += 1;
        u32::try_from(self.next_pid.fetch_add(1, Ordering::SeqCst)).ok()
    }

    fn log(&self, line: &str) {
        let mut record = self.lock();
        if let Some(rest) = line.strip_prefix("EXCEPTION ") {
            record.exceptions.push(rest.to_string());
        }
        record.logs.push(line.to_string());
    }
}

/// The world's rules. An idle world: no branches exist, no records are open,
/// nothing is done.
fn world_answer(cmd: &str) -> Option<CmdOutput> {
    let reply = |code: i32, text: &str| {
        Some(CmdOutput {
            code,
            text: text.to_string(),
        })
    };
    if cmd.contains("show-ref --verify --quiet refs/heads/") {
        return reply(1, "");
    }
    if cmd.contains("pump list --status open --json") || cmd.contains("need list --status done") {
        return reply(0, "[]");
    }
    None
}

/// A command's first three words, with the leading `cd <dir> &&` dropped.
fn shape(cmd: &str) -> String {
    let body = cmd.rsplit("&& ").next().unwrap_or(cmd);
    body.split_whitespace()
        .take(3)
        .collect::<Vec<_>>()
        .join(" ")
}
