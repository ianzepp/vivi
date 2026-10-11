//! Pool operations over a [`PoolProvider`]: create, status, destroy.

use std::time::{Duration, Instant};

use super::bootscript;
use super::config::PoolConfig;
use super::github::Secret;
use super::provider::{CreateSpec, MachineRecord, PoolProvider};
use super::record::OpenDispatch;
use super::slot::{self, META_STATE, STATE_READY, SlotStatus};
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
    /// How long to wait for a sealed slot to read `free` in a fresh listing.
    pub confirm_timeout: Duration,
}

impl Default for CreateOptions {
    fn default() -> Self {
        Self {
            slots: None,
            spacing: Duration::from_secs(1),
            poll: Duration::from_secs(5),
            timeout: Duration::from_mins(15),
            confirm_timeout: Duration::from_secs(30),
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
    /// Why the slot could not be marked ready after a clean seed.
    pub error: Option<String>,
}

impl SlotOutcome {
    /// Seeded cleanly and marked ready: stopped with exit code 0 (or none
    /// reported), seed token removed, seed mark set.
    #[must_use]
    pub fn seeded(&self) -> bool {
        !self.timed_out
            && self.state == "stopped"
            && self.exit_code.unwrap_or(0) == 0
            && self.error.is_none()
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
    let mut machines = provider.list()?;
    if adopt_finished_seeds(provider, pool, &machines) {
        machines = provider.list()?;
    }
    let existing = slot::pool_slots(pool, &machines);
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
    report.created = wait_for_seeds(provider, &started, options);
    for outcome in &mut report.created {
        if outcome.seeded() {
            outcome.error = finalize_slot(provider, &outcome.id)
                .and_then(|()| confirm_free(provider, &outcome.id, options))
                .err()
                .map(|e| e.to_string());
        }
    }
    Ok(report)
}

/// Mark a cleanly seeded slot ready: remove the fetch token from its config
/// and set the seed mark in one update (the machine stays stopped), so a ready
/// slot never still carries the token.
fn finalize_slot(provider: &dyn PoolProvider, id: &str) -> Result<(), VivariumError> {
    provider.seal(id, &["GH_TOKEN"], META_STATE, STATE_READY)
}

/// Read the listing until the sealed slot shows `free`: `status` reads that
/// listing, and it can trail the seal by a few seconds. `create` does not
/// return before it has seen the slot free.
fn confirm_free(
    provider: &dyn PoolProvider,
    id: &str,
    options: &CreateOptions,
) -> Result<(), VivariumError> {
    let deadline = Instant::now() + options.confirm_timeout;
    let pause = options.poll.min(Duration::from_secs(2));
    loop {
        let machines = provider.list()?;
        let state = machines.iter().find(|m| m.id == id).map(slot::slot_state);
        if state.as_deref() == Some("free") {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(VivariumError::Message(format!(
                "slot {id} was sealed but still reads {} in a fresh listing",
                state.unwrap_or_else(|| "missing".to_string())
            )));
        }
        std::thread::sleep(pause);
    }
}

/// Finish slots a previous, interrupted `create` seeded cleanly but never
/// marked: stopped, exit code 0, no seed mark. A failure here only leaves the
/// slot unmarked, so it shows as `unseeded`. True when any slot was touched.
fn adopt_finished_seeds(
    provider: &dyn PoolProvider,
    pool: &str,
    machines: &[MachineRecord],
) -> bool {
    let mut touched = false;
    for machine in machines {
        let ours = machine
            .metadata
            .get(slot::META_POOL)
            .is_some_and(|p| p == pool);
        let marked = machine.metadata.get(META_STATE).map(String::as_str) == Some(STATE_READY);
        if ours && !marked && machine.state == "stopped" && machine.exit_code == Some(0) {
            if let Err(e) = finalize_slot(provider, &machine.id) {
                eprintln!("warning: could not finish seeding {}: {e}", machine.name);
            }
            touched = true;
        }
    }
    touched
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
        region: config.fly.region.clone(),
        image: config.image.clone(),
        vm_size: config.fly.size.clone(),
        memory_mb: config.fly.memory,
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
) -> Vec<SlotOutcome> {
    let mut outcomes: Vec<Option<SlotOutcome>> = vec![None; launched.len()];
    let mut shown = String::new();
    loop {
        // A failed listing is a warning, not the end of a long create: the
        // slots keep seeding, and the per-slot timeout still ends the wait.
        let listing = provider.list().map_err(|e| e.to_string());
        if let Err(message) = &listing {
            if *message != shown {
                eprintln!("warning: cannot list machines yet: {message}");
            }
            shown.clone_from(message);
        } else {
            shown.clear();
        }
        for (slot, done) in launched.iter().zip(outcomes.iter_mut()) {
            if done.is_none() {
                *done = match &listing {
                    Ok(machines) => settled(
                        slot,
                        machines.iter().find(|m| m.id == slot.id),
                        options.timeout,
                    ),
                    Err(_) => unseen(slot, options.timeout),
                };
            }
        }
        if outcomes.iter().all(Option::is_some) {
            break;
        }
        std::thread::sleep(options.poll);
    }
    outcomes.into_iter().flatten().collect()
}

/// The outcome for a slot that could not be looked at for the whole timeout.
fn unseen(slot: &Launched, timeout: Duration) -> Option<SlotOutcome> {
    let elapsed = slot.at.elapsed();
    (elapsed >= timeout).then(|| SlotOutcome {
        name: slot.name.clone(),
        id: slot.id.clone(),
        state: "unknown".to_string(),
        exit_code: None,
        seconds: elapsed.as_secs(),
        timed_out: true,
        error: None,
    })
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
        error: None,
    })
}

/// What making the application ready found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppReport {
    pub app: String,
    /// True when this call created the application.
    pub created: bool,
    /// Harness environment names the application has no secret for.
    pub missing_secrets: Vec<String>,
}

/// Make sure the application exists, then look for the secrets the harness
/// needs. Secrets are never set here, and no value is read.
///
/// # Errors
/// Returns an error if the application cannot be listed or created.
pub fn prepare_app(
    provider: &dyn PoolProvider,
    config: &PoolConfig,
    app: &str,
) -> Result<AppReport, VivariumError> {
    let created = provider.ensure_app(config.organization_name())?;
    Ok(AppReport {
        app: app.to_string(),
        created,
        missing_secrets: missing_secrets(provider, config)?,
    })
}

/// The harness environment names that the application has no secret for.
///
/// # Errors
/// Returns an error if the secret names cannot be listed.
pub fn missing_secrets(
    provider: &dyn PoolProvider,
    config: &PoolConfig,
) -> Result<Vec<String>, VivariumError> {
    let Some(harness) = &config.harness else {
        return Ok(Vec::new());
    };
    if harness.env.is_empty() {
        return Ok(Vec::new());
    }
    let have = provider.secret_names()?;
    Ok(harness
        .env
        .iter()
        .filter(|name| !have.contains(name))
        .cloned()
        .collect())
}

/// The warning for missing secrets: names and a command shape, no values.
#[must_use]
pub fn secrets_warning(app: &str, missing: &[String]) -> Option<String> {
    if missing.is_empty() {
        return None;
    }
    let assignments = missing
        .iter()
        .map(|name| format!("{name}=<value>"))
        .collect::<Vec<_>>()
        .join(" ");
    Some(format!(
        "warning: app {app} has no secret named {}. The harness gets these from the machine's environment, so a job will run without them unless the image sets them. Set them yourself (vivi never reads or sets secret values):\n  fly secrets set -a {app} {assignments}",
        missing.join(", ")
    ))
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

/// Fill in how long each slot's open dispatch has run and its budget.
pub fn annotate_running(slots: &mut [SlotStatus], open: &[OpenDispatch], now: u64) {
    for slot in slots {
        if let Some(d) = open.iter().find(|d| d.record.machine == slot.id) {
            slot.running_secs = Some(now.saturating_sub(d.record.at));
            slot.budget_secs = Some(d.record.budget);
        }
    }
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
