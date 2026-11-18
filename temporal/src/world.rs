//! The stand-in world the activities act on: scripted needs, per-step call
//! counters, branches with fenced pushes, `main`, and an event log the
//! tests read. Nothing here talks to git, a model, or the vivi server.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::error::SpikeError;
use crate::fault::{FaultKind, NeedScript, SliceScript};

/// Speeds of the stand-in world.
#[derive(Debug, Clone)]
pub struct WorldTiming {
    /// Wall time of one implement step (one pushed commit).
    pub step: Duration,
    /// Commits per implement round.
    pub steps_per_round: u32,
    /// Retry-After the scripted provider returns on a 429.
    pub retry_after: Duration,
    /// How long a dark host's activity lingers before it gives up.
    pub dark_hold: Duration,
}

/// Something that happened in the world, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Planned {
        need: String,
    },
    Throttled {
        step: String,
        slice: String,
        at: Duration,
    },
    ImplementStarted(ImplementStart),
    Pushed {
        slice: String,
        branch: String,
        commit: String,
    },
    WentDark {
        slice: String,
        kind: FaultKind,
    },
    Checked {
        slice: String,
        round: u32,
        green: bool,
    },
    Reviewed {
        slice: String,
        round: u32,
        approved: bool,
    },
    Gate {
        batch: Vec<String>,
        pass: bool,
    },
    Merged {
        batch: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImplementStart {
    pub slice: String,
    pub round: u32,
    /// Temporal's attempt number for this activity execution.
    pub attempt: u32,
    pub branch: String,
    pub resumed_from: Option<String>,
    pub feedback: Option<String>,
    pub at: Duration,
}

#[derive(Debug, Default)]
struct Active {
    now: HashMap<String, u32>,
    max: HashMap<String, u32>,
}

pub struct World {
    pub timing: WorldTiming,
    needs: HashMap<String, NeedScript>,
    started: Instant,
    calls: Mutex<HashMap<(String, String), u32>>,
    branches: Mutex<HashMap<String, Vec<String>>>,
    main: Mutex<Vec<String>>,
    events: Mutex<Vec<Event>>,
    active: Mutex<Active>,
}

impl World {
    pub fn new(timing: WorldTiming, needs: Vec<(&str, NeedScript)>) -> World {
        World {
            timing,
            needs: needs.into_iter().map(|(h, s)| (h.to_string(), s)).collect(),
            started: Instant::now(),
            calls: Mutex::default(),
            branches: Mutex::default(),
            main: Mutex::default(),
            events: Mutex::default(),
            active: Mutex::default(),
        }
    }

    pub fn need(&self, need: &str) -> Result<&NeedScript, SpikeError> {
        self.needs
            .get(need)
            .ok_or_else(|| SpikeError::UnknownNeed(need.to_string()))
    }

    pub fn slice(&self, need: &str, slice: &str) -> Result<&SliceScript, SpikeError> {
        self.need(need)?
            .slices
            .iter()
            .find(|s| s.id == slice)
            .ok_or_else(|| SpikeError::UnknownSlice {
                need: need.to_string(),
                slice: slice.to_string(),
            })
    }

    /// Count one call of `step` for `key` and return its 1-based number.
    pub fn next_call(&self, step: &str, key: &str) -> Result<u32, SpikeError> {
        let mut calls = self.calls.lock()?;
        let n = calls
            .entry((step.to_string(), key.to_string()))
            .or_insert(0);
        *n += 1;
        Ok(*n)
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    pub fn log(&self, event: Event) -> Result<(), SpikeError> {
        self.events.lock()?.push(event);
        Ok(())
    }

    pub fn events(&self) -> Result<Vec<Event>, SpikeError> {
        Ok(self.events.lock()?.clone())
    }

    /// Push `commit` onto `branch` if its head is still `expected`. This is
    /// the fence that stops a host declared lost from writing after its
    /// work was handed to another attempt.
    pub fn push(
        &self,
        branch: &str,
        expected: Option<&str>,
        commit: &str,
    ) -> Result<(), SpikeError> {
        let mut branches = self.branches.lock()?;
        let commits = branches.entry(branch.to_string()).or_default();
        let found = commits.last().cloned();
        if found.as_deref() != expected {
            return Err(SpikeError::PushRejected {
                branch: branch.to_string(),
                expected: expected.map(str::to_string),
                found,
            });
        }
        commits.push(commit.to_string());
        Ok(())
    }

    pub fn branch(&self, branch: &str) -> Result<Vec<String>, SpikeError> {
        Ok(self
            .branches
            .lock()?
            .get(branch)
            .cloned()
            .unwrap_or_default())
    }

    pub fn merge(&self, slices: &[String]) -> Result<(), SpikeError> {
        self.main.lock()?.extend(slices.iter().cloned());
        self.log(Event::Merged {
            batch: slices.to_vec(),
        })
    }

    pub fn main(&self) -> Result<Vec<String>, SpikeError> {
        Ok(self.main.lock()?.clone())
    }

    /// Mark one unit of work on `slice` as running until the guard drops.
    pub fn enter(&self, slice: &str) -> Result<ActiveGuard<'_>, SpikeError> {
        let mut active = self.active.lock()?;
        let now = active.now.entry(slice.to_string()).or_insert(0);
        *now += 1;
        let current = *now;
        let max = active.max.entry(slice.to_string()).or_insert(0);
        *max = (*max).max(current);
        Ok(ActiveGuard {
            world: self,
            slice: slice.to_string(),
        })
    }

    /// The most units of work that ever ran at once, per slice.
    pub fn max_concurrency(&self) -> Result<HashMap<String, u32>, SpikeError> {
        Ok(self.active.lock()?.max.clone())
    }

    fn leave(&self, slice: &str) {
        if let Ok(mut active) = self.active.lock()
            && let Some(n) = active.now.get_mut(slice)
        {
            *n = n.saturating_sub(1);
        }
    }
}

pub struct ActiveGuard<'a> {
    world: &'a World,
    slice: String,
}

impl Drop for ActiveGuard<'_> {
    fn drop(&mut self) {
        self.world.leave(&self.slice);
    }
}
