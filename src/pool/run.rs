//! Pool operations over a [`PoolProvider`]: create, status, destroy.

use std::time::{Duration, Instant};

use super::bootscript;
use super::config::PoolConfig;
use super::github::Secret;
use super::provider::{CreateSpec, MachineRecord, PoolProvider};
use super::slot::{self, SlotStatus};
use crate::error::VivariumError;

const MISSING_GRACE: Duration = Duration::from_secs(30);

/// How `create` paces itself.
#[derive(Debug, Clone)]
pub struct CreateOptions {
    /// Pool size to reach; `None` means the configured maximum.
    pub slots: Option<u32>,
    /// Pause between create calls (flyctl wants about a second).
    pub spacing: Duration,
    /// Pause between state polls while seeding.
    pub poll: Duration,
    /// Give up waiting on a slot after this long.
    pub timeout: Duration,
}

impl Default for CreateOptions {
    fn default() -> Self {
        Self {
            slots: None,
            spacing: Duration::from_secs(1),
            poll: Duration::from_secs(5),
            timeout: Duration::from_mins(15),
        }
    }
}

/// How one new slot ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotOutcome {
    pub name: String,
    pub id: String,
    /// Provider state word at the last look.
    pub state: String,
    pub exit_code: Option<i64>,
    pub seconds: u64,
    pub timed_out: bool,
}

impl SlotOutcome {
    /// Seeded cleanly: stopped with exit code 0 (or none reported).
    #[must_use]
    pub fn seeded(&self) -> bool {
        !self.timed_out && self.state == "stopped" && self.exit_code.unwrap_or(0) == 0
    }
}

/// What `create` did.
#[derive(Debug, Clone, Default)]
pub struct CreateReport {
    /// Slots the pool already held and left alone.
    pub existing: Vec<SlotStatus>,
    pub created: Vec<SlotOutcome>,
}

/// Create the missing slots up to the target and wait for each to seed.
///
/// `seed_inputs` supplies the fetch token and the resolved base commit; it
/// runs only when there is something to create.
///
/// # Errors
/// Returns an error if the target exceeds the configured maximum, or the
/// provider fails before any slot is created.
pub fn create_slots(
    provider: &dyn PoolProvider,
    config: &PoolConfig,
    pool: &str,
    options: &CreateOptions,
    seed_inputs: impl FnOnce() -> Result<(Secret, String), VivariumError>,
) -> Result<CreateReport, VivariumError> {
    let target = options.slots.unwrap_or(config.slots);
    if target > config.slots {
        return Err(VivariumError::Message(format!(
            "--slots {target} exceeds the configured maximum of {}",
            config.slots
        )));
    }
    let existing = slot::pool_slots(pool, &provider.list()?);
    let missing = slot::missing_indices(&existing, target);
    let mut report = CreateReport {
        existing,
        created: Vec::new(),
    };
    if missing.is_empty() {
        return Ok(report);
    }
    let (token, base) = seed_inputs()?;
    let started = launch_all(provider, config, pool, &missing, &token, &base, options)?;
    report.created = wait_for_seeds(provider, &started, options)?;
    Ok(report)
}

struct Launched {
    name: String,
    id: String,
    at: Instant,
}

fn launch_all(
    provider: &dyn PoolProvider,
    config: &PoolConfig,
    pool: &str,
    indices: &[u32],
    token: &Secret,
    base: &str,
    options: &CreateOptions,
) -> Result<Vec<Launched>, VivariumError> {
    let mut launched = Vec::new();
    for (n, index) in indices.iter().enumerate() {
        if n > 0 {
            std::thread::sleep(options.spacing);
        }
        let spec = slot_spec(config, pool, *index, token, base);
        let at = Instant::now();
        match provider.create(&spec) {
            Ok(id) => launched.push(Launched {
                name: spec.name,
                id,
                at,
            }),
            Err(e) if launched.is_empty() => return Err(e),
            Err(e) => eprintln!("warning: could not create {}: {e}", spec.name),
        }
    }
    Ok(launched)
}

fn slot_spec(
    config: &PoolConfig,
    pool: &str,
    index: u32,
    token: &Secret,
    base: &str,
) -> CreateSpec {
    CreateSpec {
        name: slot::slot_name(pool, index),
        region: config.region.clone(),
        image: config.image.clone(),
        vm_size: config.size.clone(),
        memory_mb: config.memory,
        metadata: slot::slot_metadata(pool, index, base),
        env: bootscript::slot_env(config, token, base),
        files: bootscript::slot_files(config),
        command: bootscript::slot_command(),
    }
}

fn wait_for_seeds(
    provider: &dyn PoolProvider,
    launched: &[Launched],
    options: &CreateOptions,
) -> Result<Vec<SlotOutcome>, VivariumError> {
    let mut outcomes: Vec<Option<SlotOutcome>> = vec![None; launched.len()];
    loop {
        let machines = provider.list()?;
        for (slot, done) in launched.iter().zip(outcomes.iter_mut()) {
            if done.is_some() {
                continue;
            }
            let record = machines.iter().find(|m| m.id == slot.id);
            *done = settled(slot, record, options.timeout);
        }
        if outcomes.iter().all(Option::is_some) {
            break;
        }
        std::thread::sleep(options.poll);
    }
    Ok(outcomes.into_iter().flatten().collect())
}

/// The outcome for one slot once it has stopped, vanished, or timed out.
fn settled(
    slot: &Launched,
    record: Option<&MachineRecord>,
    timeout: Duration,
) -> Option<SlotOutcome> {
    let elapsed = slot.at.elapsed();
    let state = record.map_or("missing", |m| m.state.as_str());
    // A machine absent from the list may just not be listed yet; give it time.
    let gone = state == "missing" && elapsed >= MISSING_GRACE;
    let finished = matches!(state, "stopped" | "destroyed" | "failed") || gone;
    let timed_out = !finished && elapsed >= timeout;
    (finished || timed_out).then(|| SlotOutcome {
        name: slot.name.clone(),
        id: slot.id.clone(),
        state: state.to_string(),
        exit_code: record.and_then(|m| m.exit_code),
        seconds: elapsed.as_secs(),
        timed_out,
    })
}

/// The pool's slots as the provider reports them.
///
/// # Errors
/// Returns an error if the provider cannot list machines.
pub fn pool_status(
    provider: &dyn PoolProvider,
    pool: &str,
) -> Result<Vec<SlotStatus>, VivariumError> {
    Ok(slot::pool_slots(pool, &provider.list()?))
}

/// Which slots `destroy` removes.
#[derive(Debug, Clone)]
pub enum DestroyTarget {
    /// One slot, by name or machine id.
    Slot(String),
    All,
}

/// Destroy pool slots; machines outside the pool are never touched.
///
/// # Errors
/// Returns an error if a named slot is not in the pool, or a destroy fails.
pub fn destroy_slots(
    provider: &dyn PoolProvider,
    pool: &str,
    target: &DestroyTarget,
) -> Result<Vec<SlotStatus>, VivariumError> {
    let slots = slot::pool_slots(pool, &provider.list()?);
    let doomed: Vec<SlotStatus> = match target {
        DestroyTarget::All => slots,
        DestroyTarget::Slot(selector) => {
            let Some(found) = slot::find_slot(&slots, selector) else {
                return Err(unknown_slot(pool, selector, &slots));
            };
            vec![found.clone()]
        }
    };
    for slot in &doomed {
        provider.destroy(&slot.id)?;
    }
    Ok(doomed)
}

fn unknown_slot(pool: &str, selector: &str, slots: &[SlotStatus]) -> VivariumError {
    let names: Vec<&str> = slots.iter().map(|s| s.name.as_str()).collect();
    let known = if names.is_empty() {
        "none".to_string()
    } else {
        names.join(", ")
    };
    VivariumError::Message(format!(
        "no slot \"{selector}\" in pool {pool} (slots: {known})"
    ))
}

#[cfg(test)]
#[path = "run_test.rs"]
mod tests;
