//! Pool operations over a [`PoolProvider`]: create, status, destroy.

use std::time::{Duration, Instant};

use super::bootscript;
use super::config::PoolConfig;
use super::github::Secret;
use super::pools::PoolSet;
use super::progress::{Beat, HEARTBEAT, Issue, Progress};
use super::provider::{CreateSpec, MachineRecord, PoolProvider};
use super::record::OpenDispatch;
use super::requires;
use super::slot::{self, META_STATE, PoolId, STATE_READY, SlotStatus};

/// Environment names the seed needs once; they never stay on a ready slot.
pub const ONCE_ENV: [&str; 1] = ["GH_TOKEN"];
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
    /// How often the wait for seeds says it is still working.
    pub heartbeat: Duration,
}

impl Default for CreateOptions {
    fn default() -> Self {
        Self {
            slots: None,
            spacing: Duration::from_secs(1),
            poll: Duration::from_secs(5),
            timeout: Duration::from_mins(15),
            confirm_timeout: Duration::from_secs(30),
            heartbeat: HEARTBEAT,
        }
    }
}

/// How one new slot ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotOutcome {
    /// The pool the slot was made for.
    pub pool: String,
    pub name: String,
    pub id: String,
    /// Provider state word at the last look.
    pub state: String,
    pub exit_code: Option<i64>,
    pub seconds: u64,
    pub timed_out: bool,
    /// Why the slot could not be marked ready after a clean seed, or the
    /// reason the seed failed when the slot named one (a `requires` command
    /// that did not pass).
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
    /// Problems met while waiting for the seeds (a listing that failed).
    pub issues: Vec<Issue>,
}

/// Create the missing slots up to the target and wait for each to seed.
///
/// `seed_inputs` supplies the fetch token and the resolved base commit; it
/// runs only when there is something to create. `progress` gets a line as each
/// phase starts and a heartbeat while the seeds run.
///
/// # Errors
/// Returns an error if the target exceeds the configured maximum, or the
/// provider fails before any slot is created.
pub fn create_slots(
    provider: &dyn PoolProvider,
    config: &PoolConfig,
    id: &PoolId,
    options: &CreateOptions,
    progress: &Progress,
    seed_inputs: impl FnOnce() -> Result<(Secret, String), VivariumError>,
) -> Result<CreateReport, VivariumError> {
    let target = options.slots.unwrap_or(config.slots);
    if target > config.slots {
        return Err(VivariumError::Message(format!(
            "--slots {target} exceeds the configured maximum of {}",
            config.slots
        )));
    }
    progress.say("checking the pool's existing slots");
    let mut machines = provider.list()?;
    if adopt_finished_seeds(provider, id, &machines) {
        machines = provider.list()?;
    }
    let existing = slot::pool_slots(id, &machines);
    let missing = slot::missing_indices(&existing, target);
    let mut report = CreateReport {
        existing,
        ..CreateReport::default()
    };
    if missing.is_empty() {
        return Ok(report);
    }
    let (token, base) = seed_inputs()?;
    let seed = (&token, base.as_str());
    let started = launch_all(provider, config, id, &missing, seed, options, progress)?;
    progress.say(&format!("seeding {} slot(s)", started.len()));
    (report.created, report.issues) = wait_for_seeds(provider, id, &started, options, progress);
    for outcome in &mut report.created {
        if outcome.seeded() {
            progress.say(&format!("marking {} ready", outcome.name));
            outcome.error = finalize_slot(provider, &outcome.id)
                .and_then(|()| confirm_free(provider, &outcome.id, options))
                .err()
                .map(|e| e.to_string());
        } else if !outcome.timed_out && outcome.state == "stopped" {
            outcome.error = seed_failure(provider, &outcome.id);
        }
    }
    Ok(report)
}

/// Why a stopped, unseeded slot's seed failed, when the boot script named a
/// reason (a requirement that did not pass) and the host can read the stopped
/// disk. `None` otherwise: the exit code is then all there is.
fn seed_failure(provider: &dyn PoolProvider, id: &str) -> Option<String> {
    let Ok(Some(files)) = provider.read_stopped(id, &[requires::FAILURE_PATH]) else {
        return None;
    };
    let text = String::from_utf8_lossy(files.get(requires::FAILURE_PATH)?);
    let line = text.lines().next()?.trim();
    (!line.is_empty()).then(|| line.to_string())
}

/// Mark a cleanly seeded slot ready: remove the fetch token from its config
/// (where the host kept it there) and set the seed mark in one update (the
/// machine stays stopped), so a ready slot never still carries the token.
fn finalize_slot(provider: &dyn PoolProvider, id: &str) -> Result<(), VivariumError> {
    provider.seal(id, &ONCE_ENV, META_STATE, STATE_READY)
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
    id: &PoolId,
    machines: &[MachineRecord],
) -> bool {
    let mut touched = false;
    for machine in machines {
        let ours = id.owns(machine);
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
    id: &PoolId,
    indices: &[u32],
    seed: (&Secret, &str),
    options: &CreateOptions,
    progress: &Progress,
) -> Result<Vec<Launched>, VivariumError> {
    let mut launched = Vec::new();
    for (n, index) in indices.iter().enumerate() {
        if n > 0 {
            std::thread::sleep(options.spacing);
        }
        let spec = slot_spec(config, id, *index, seed, provider);
        progress.say(&format!("creating {}", spec.name));
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

/// What the slot of one dispatch is made from, for a host that makes it per
/// dispatch: as `create` would make slot `index`, seeded at `base` with the
/// dispatch's `token` (the host chooses the index).
#[must_use]
pub fn dispatch_spec(
    config: &PoolConfig,
    id: &PoolId,
    seed: (&Secret, &str),
    provider: &dyn PoolProvider,
) -> CreateSpec {
    slot_spec(config, id, 0, seed, provider)
}

fn slot_spec(
    config: &PoolConfig,
    id: &PoolId,
    index: u32,
    (token, base): (&Secret, &str),
    provider: &dyn PoolProvider,
) -> CreateSpec {
    let mode = bootscript::SlotMode {
        mounted: provider.supplies_build_mount(),
        persists: provider.build_persists(),
        unprivileged: provider.runs_unprivileged(),
    };
    let mut env = bootscript::slot_env(config, token, base, mode);
    let once_env = ONCE_ENV
        .iter()
        .filter_map(|name| Some((name.to_string(), env.remove(*name)?)))
        .collect();
    CreateSpec {
        name: id.slot_name(index),
        image: config.image.clone(),
        metadata: slot::slot_metadata(id, index, base),
        env,
        once_env,
        passthrough_env: config
            .harness
            .as_ref()
            .map(|h| h.env.clone())
            .unwrap_or_default(),
        files: bootscript::slot_files(config),
        command: bootscript::slot_command(),
        build_dir: config.build_dir.clone(),
        tmpfs: config.tmpfs_size().to_string(),
    }
}

fn wait_for_seeds(
    provider: &dyn PoolProvider,
    id: &PoolId,
    launched: &[Launched],
    options: &CreateOptions,
    progress: &Progress,
) -> (Vec<SlotOutcome>, Vec<Issue>) {
    let mut outcomes: Vec<Option<SlotOutcome>> = vec![None; launched.len()];
    let mut seen = ListingErrors::default();
    let started = Instant::now();
    let mut beat = Beat::new(options.heartbeat);
    loop {
        // A failed listing is a warning, not the end of a long create: the
        // slots keep seeding, and the per-slot timeout still ends the wait.
        let listing = provider.list().map_err(|e| e.to_string());
        seen.observe(&id.pool, listing.as_ref().err(), progress);
        for (slot, done) in launched.iter().zip(outcomes.iter_mut()) {
            if done.is_none() {
                *done = match &listing {
                    Ok(machines) => settled(
                        (id, slot),
                        machines.iter().find(|m| m.id == slot.id),
                        options.timeout,
                    ),
                    Err(_) => unseen((id, slot), options.timeout),
                };
            }
        }
        if outcomes.iter().all(Option::is_some) {
            break;
        }
        if beat.due(started.elapsed()) {
            progress.say(&seeding_text(launched, &outcomes, options.timeout));
        }
        std::thread::sleep(options.poll);
    }
    (outcomes.into_iter().flatten().collect(), seen.issues)
}

/// The heartbeat of the wait for seeds: which slots still seed, and for how
/// long, against the time they are given.
fn seeding_text(
    launched: &[Launched],
    outcomes: &[Option<SlotOutcome>],
    timeout: Duration,
) -> String {
    let seeding: Vec<String> = launched
        .iter()
        .zip(outcomes)
        .filter(|(_, done)| done.is_none())
        .map(|(slot, _)| format!("{} {}s", slot.name, slot.at.elapsed().as_secs()))
        .collect();
    format!(
        "waiting  seeding {} (gives up at {}s)",
        seeding.join(", "),
        timeout.as_secs()
    )
}

/// Remembers the listing failure in progress, so a long outage warns once and
/// is one issue with a start and an end.
#[derive(Default)]
struct ListingErrors {
    open: Option<usize>,
    issues: Vec<Issue>,
}

impl ListingErrors {
    fn observe(&mut self, pool: &str, error: Option<&String>, progress: &Progress) {
        let now = super::work::epoch_secs();
        match (error, self.open) {
            (Some(message), open) => {
                let same = open.is_some_and(|i| &self.issues[i].message == message);
                if same {
                    return;
                }
                self.close(now);
                progress.say(&format!("warning: cannot list machines yet: {message}"));
                self.open = Some(self.issues.len());
                self.issues.push(Issue {
                    pool: Some(pool.to_string()),
                    kind: "list-error",
                    subject: None,
                    message: message.clone(),
                    first_seen: now,
                    cleared: None,
                });
            }
            (None, Some(_)) => self.close(now),
            (None, None) => {}
        }
    }

    fn close(&mut self, now: u64) {
        if let Some(i) = self.open.take() {
            self.issues[i].cleared = Some(now);
        }
    }
}

/// The outcome for a slot that could not be looked at for the whole timeout.
fn unseen((id, slot): (&PoolId, &Launched), timeout: Duration) -> Option<SlotOutcome> {
    let elapsed = slot.at.elapsed();
    (elapsed >= timeout).then(|| SlotOutcome {
        pool: id.pool.clone(),
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
    (id, slot): (&PoolId, &Launched),
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
        pool: id.pool.clone(),
        name: slot.name.clone(),
        id: slot.id.clone(),
        state: state.to_string(),
        exit_code: record.and_then(|m| m.exit_code),
        seconds: elapsed.as_secs(),
        timed_out,
        error: None,
    })
}

/// What making the pool's home ready found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomeReport {
    /// What this call created, when it created anything.
    pub created: Option<String>,
    /// Harness environment names the host has no value for.
    pub missing_env: Vec<String>,
}

/// Make sure the pool's home exists, then look for the harness environment
/// the host lacks. No value is set or read here.
///
/// # Errors
/// Returns an error if the home cannot be listed or created.
pub fn prepare_home(
    provider: &dyn PoolProvider,
    config: &PoolConfig,
) -> Result<HomeReport, VivariumError> {
    Ok(HomeReport {
        created: provider.ensure_home()?,
        missing_env: missing_env(provider, config)?,
    })
}

/// The harness environment names that the host has no value for.
///
/// # Errors
/// Returns an error if the host cannot be asked.
pub fn missing_env(
    provider: &dyn PoolProvider,
    config: &PoolConfig,
) -> Result<Vec<String>, VivariumError> {
    let Some(harness) = &config.harness else {
        return Ok(Vec::new());
    };
    if harness.env.is_empty() {
        return Ok(Vec::new());
    }
    provider.harness_env_gaps(&harness.env)
}

/// The harness environment names a dispatch to existing slots would lack.
///
/// # Errors
/// Returns an error if the host cannot be asked.
pub fn missing_env_at_dispatch(
    provider: &dyn PoolProvider,
    config: &PoolConfig,
) -> Result<Vec<String>, VivariumError> {
    match config.harness.as_ref().filter(|h| !h.env.is_empty()) {
        Some(harness) => provider.env_gaps_at_dispatch(&harness.env),
        None => Ok(Vec::new()),
    }
}

/// The warning for missing harness environment: names and how to supply them
/// on this host, no values.
#[must_use]
pub fn env_warning(provider: &dyn PoolProvider, missing: &[String]) -> Option<String> {
    (!missing.is_empty()).then(|| provider.env_gap_hint(missing))
}

/// The pool's slots as the provider reports them.
///
/// # Errors
/// Returns an error if the provider cannot list machines.
pub fn pool_status(
    provider: &dyn PoolProvider,
    id: &PoolId,
) -> Result<Vec<SlotStatus>, VivariumError> {
    Ok(slot::pool_slots(id, &provider.list()?))
}

/// The slots of every pool in a set, each with its open dispatch's running
/// time, and the pools that could not be listed.
#[derive(Debug, Clone, Default)]
pub struct StatusReport {
    /// Slots of the pools that listed, in pool order.
    pub slots: Vec<SlotStatus>,
    /// `(pool, slots)` for each pool that listed, so an empty pool still shows.
    pub pools: Vec<(String, usize)>,
    /// `(pool, error)` for each pool whose host could not be listed.
    pub failures: Vec<(String, String)>,
}

/// The status of every pool in `set`. One pool's host failing does not hide
/// the others.
#[must_use]
pub fn status_of(set: &PoolSet, open: &[OpenDispatch], now: u64) -> StatusReport {
    let mut report = StatusReport::default();
    for member in &set.members {
        match pool_status(member.provider, member.id) {
            Ok(mut slots) => {
                annotate_running(&mut slots, open, now);
                report.pools.push((member.id.pool.clone(), slots.len()));
                report.slots.extend(slots);
            }
            Err(e) => report
                .failures
                .push((member.id.pool.clone(), e.to_string())),
        }
    }
    report
}

/// Fill in how long each slot's open dispatch has run and its budget.
pub fn annotate_running(slots: &mut [SlotStatus], open: &[OpenDispatch], now: u64) {
    for slot in slots {
        if let Some(d) = open.iter().find(|d| d.record.machine == slot.id) {
            slot.running_secs = Some(now.saturating_sub(d.record.at));
            slot.budget_secs = Some(d.record.budget);
            slot.read_only = Some(d.record.read_only);
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

/// Destroy pool slots; machines outside the pool (another pool, or another
/// mailspace's pool of the same name) are never touched.
///
/// # Errors
/// Returns an error if a named slot is not in the pool, or a destroy fails.
pub fn destroy_slots(
    provider: &dyn PoolProvider,
    id: &PoolId,
    target: &DestroyTarget,
) -> Result<Vec<SlotStatus>, VivariumError> {
    let slots = slot::pool_slots(id, &provider.list()?);
    let doomed: Vec<SlotStatus> = match target {
        DestroyTarget::All => slots,
        DestroyTarget::Slot(selector) => {
            let Some(found) = slot::find_slot(&slots, selector) else {
                return Err(unknown_slot(&id.pool, selector, &slots));
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
