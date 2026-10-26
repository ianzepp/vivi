//! The foreground mind service (`vivi mind service run`) and its wake tick.
//!
//! The service owns the one pi session, the attach socket, and the wake
//! loop. It is a process of its own under the OS service manager, never a
//! pump cadence job, so no pump can take it down. It reads pumps only
//! through [`super::probe`] and acts only through [`super::actions`].

use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::VivariumError;

use super::actions::{self, Actuator, DryRun, Planned};
use super::authority::Gate;
use super::config::{self, MindConfig};
use super::edge::{self, EdgeActuator};
use super::mcp_actuator::{HttpCaller, McpActuator};
use super::pi::{PiSession, ProcessPi};
use super::probe::{HealthProbe, Probe, ProjectSignals};
use super::prompt;
use super::socket::{self, BusyGuard, Shared};
use super::store::{self, MindState};
use super::wake::{self, WakeDecision, WakeInputs, WakeReason, WakeRecord};

/// How often the loop re-evaluates wakes. Heartbeats are coarser than this.
pub const TICK: Duration = Duration::from_secs(60);

/// What one tick did.
#[derive(Debug)]
pub enum TickOutcome {
    Slept(&'static str),
    Woke { planned: Vec<Planned> },
    Failed(String),
}

/// Where a tick reads the time. Read once before the turn and again
/// after it returns: a turn can run for minutes, and its wake must not
/// end when it began.
pub trait Clock: Send + Sync {
    /// Seconds since the epoch.
    fn now(&self) -> u64;
}

/// The wall clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> u64 {
        now()
    }
}

/// Everything one tick reads besides the session.
pub struct TickContext<'a, Q: Probe> {
    pub config: &'a MindConfig,
    pub registry: &'a Path,
    pub probe: &'a Q,
    pub clock: &'a dyn Clock,
}

/// Run one wake evaluation and, when due, one autonomous turn.
///
/// # Errors
/// Fails on registry or state I/O; a failed turn is [`TickOutcome::Failed`].
pub async fn tick<P: PiSession, Q: Probe>(
    shared: &Shared<P>,
    cx: &TickContext<'_, Q>,
    actuator: &mut dyn Actuator,
) -> Result<TickOutcome, VivariumError> {
    let started = cx.clock.now();
    let state_path = &cx.config.paths.state;
    let state = store::load_at(state_path)?;
    let watched = super::projects::watched_at(cx.registry, &state.ignored)?;
    let signals: Vec<ProjectSignals> = watched.iter().map(|root| cx.probe.probe(root)).collect();
    let edge = cx
        .config
        .edge
        .as_ref()
        .map(|edge| edge::read(&edge.status).unwrap_or_default());
    let hub = edge
        .as_ref()
        .map_or_else(Vec::new, |status| status.fingerprints(started));
    let decision = wake::decide(&WakeInputs {
        now: started,
        heartbeat: &cx.config.heartbeat,
        last: state.last_wake.as_ref(),
        recent: &state.recent_wakes,
        max_wakes_per_hour: cx.config.budgets.max_wakes_per_hour,
        paused: state.paused,
        signals: &signals,
        hub: &hub,
    });
    let (reasons, attempt, seen) = match decision {
        WakeDecision::Sleep(why) => return Ok(TickOutcome::Slept(why)),
        WakeDecision::Wake {
            reasons,
            attempt,
            seen,
        } => (reasons, attempt, seen),
    };
    let record = begin_wake(state_path, started, attempt, seen)?;
    let why: Vec<String> = reasons.iter().map(WakeReason::log_label).collect();
    log_line(
        &cx.config.paths.log,
        &format!("wake begins: {}", why.join("; ")),
    );
    let mut message = prompt::wake_prompt(
        cx.config.authority,
        &reasons,
        &signals,
        edge.as_ref().map(|status| (status, started)),
    );
    if let Some(report) = &state.pending_report {
        message = format!("{report}\n{message}");
    }
    let turn = prompt_once(shared, message).await;
    settle_wake(shared, cx, record, turn, &watched, actuator).await
}

/// One autonomous turn: take the turn lock, send `message`, return the
/// reply. The lock is held for this turn only, so an attached operator can
/// speak between a wake's turns.
async fn prompt_once<P: PiSession>(
    shared: &Shared<P>,
    message: String,
) -> Result<super::pi::TurnSummary, VivariumError> {
    let mut pi = shared.pi.lock().await;
    let _busy = BusyGuard::hold(&shared.busy);
    pi.prompt(message, None).await
}

/// What an attached operator prompt is sent as: a fresh probe of every
/// watched project, then the operator's text. When the projects cannot be
/// read the text goes as typed, with a line saying so, so an attach never
/// fails on the probe.
pub fn attach_text<Q: Probe>(registry: &Path, state_path: &Path, probe: &Q, text: &str) -> String {
    let watched = store::load_at(state_path)
        .and_then(|state| super::projects::watched_at(registry, &state.ignored));
    match watched {
        Ok(roots) => {
            let signals: Vec<ProjectSignals> = roots.iter().map(|root| probe.probe(root)).collect();
            prompt::attach_prompt(text, &signals)
        }
        Err(error) => format!("(a fresh pump status could not be read: {error})\n{text}"),
    }
}

fn begin_wake(
    state_path: &Path,
    now: u64,
    attempt: u32,
    seen: Vec<String>,
) -> Result<WakeRecord, VivariumError> {
    let record = WakeRecord {
        started_at: now,
        attempt,
        seen,
        ..WakeRecord::default()
    };
    store::update_at(state_path, |state| {
        state.note_wake(now);
        state.last_wake = Some(record.clone());
        Ok(())
    })?;
    Ok(record)
}

/// Turn the first reply into actions, run them, and while the actuator
/// returns real answers show them to the session so it can check its work:
/// each follow-up turn may name more actions, up to `follow_ups` times. The
/// wake ends after the last turn returns.
async fn settle_wake<P: PiSession, Q: Probe>(
    shared: &Shared<P>,
    cx: &TickContext<'_, Q>,
    mut record: WakeRecord,
    first: Result<super::pi::TurnSummary, VivariumError>,
    watched: &[PathBuf],
    actuator: &mut dyn Actuator,
) -> Result<TickOutcome, VivariumError> {
    let config = cx.config;
    let max_follow_ups = config.mcp.as_ref().map_or(0, |mcp| mcp.follow_ups);
    let mut turn = first;
    let mut all = Vec::new();
    let mut round = 0;
    let mut unreported = Vec::new();
    loop {
        let summary = match turn {
            Ok(summary) => summary,
            Err(error) if round == 0 => return fail_wake(cx, record, &error),
            Err(error) => {
                log_line(
                    &config.paths.log,
                    &format!("follow-up turn failed: {error}"),
                );
                break;
            }
        };
        let label = if round == 0 {
            "wake turn"
        } else {
            "follow-up turn"
        };
        log_line(&config.paths.log, &format!("{label}:\n{}", summary.text));
        let planned = run_envelope(cx, &summary.text, watched, actuator)?;
        let ran = planned.iter().any(|item| item.executed.is_some());
        if ran && actuator.feeds_back() && round < max_follow_ups {
            round += 1;
            turn = prompt_once(shared, prompt::plan_report(&planned)).await;
            all.extend(planned);
            unreported.clear();
        } else {
            unreported = planned.clone();
            all.extend(planned);
            break;
        }
    }
    let ended = cx.clock.now();
    store::update_at(&config.paths.state, |state| {
        wake::finish(&mut record, true, ended);
        state.last_wake = Some(record);
        state.pending_report = (!unreported.is_empty()).then(|| prompt::plan_report(&unreported));
        Ok(())
    })?;
    Ok(TickOutcome::Woke { planned: all })
}

fn fail_wake<Q: Probe>(
    cx: &TickContext<'_, Q>,
    mut record: WakeRecord,
    error: &VivariumError,
) -> Result<TickOutcome, VivariumError> {
    let ended = cx.clock.now();
    store::update_at(&cx.config.paths.state, |state| {
        wake::finish(&mut record, false, ended);
        state.last_wake = Some(record);
        Ok(())
    })?;
    log_line(&cx.config.paths.log, &format!("wake failed: {error}"));
    Ok(TickOutcome::Failed(error.to_string()))
}

/// Parse a reply's envelope, gate it, run what is permitted, and log each
/// line.
fn run_envelope<Q: Probe>(
    cx: &TickContext<'_, Q>,
    reply: &str,
    watched: &[PathBuf],
    actuator: &mut dyn Actuator,
) -> Result<Vec<Planned>, VivariumError> {
    let config = cx.config;
    let now = cx.clock.now();
    store::update_at(&config.paths.state, |state| {
        Ok(apply_envelope(config, state, reply, watched, now, actuator))
    })
}

fn apply_envelope(
    config: &MindConfig,
    state: &mut MindState,
    reply: &str,
    watched: &[PathBuf],
    now: u64,
    actuator: &mut dyn Actuator,
) -> Vec<Planned> {
    let requests = match actions::parse_envelope(reply) {
        Ok(requests) => requests,
        Err(error) => {
            log_line(&config.paths.log, &format!("envelope refused: {error}"));
            return Vec::new();
        }
    };
    let gate = Gate {
        stage: config.authority,
        paused: state.paused,
        budgets: &config.budgets,
        now,
    };
    let mut planned = actions::plan(requests, &gate, watched, &mut state.ledger);
    for item in &mut planned {
        let line = match &item.verdict {
            Ok(()) => {
                let result = actuator.apply(&item.request);
                let line = match &result {
                    Ok(receipt) => receipt.clone(),
                    Err(error) => format!("failed: {error}"),
                };
                item.executed = Some(result);
                line
            }
            Err(refusal) => format!("refused {:?}: {refusal}", item.request.kind),
        };
        log_line(&config.paths.log, &line);
    }
    planned
}

/// Append one timestamped line to the mind log. Logging never fails the
/// service.
pub fn log_line(path: &Path, text: &str) {
    let stamp = chrono::Local::now().to_rfc3339();
    let written = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| writeln!(file, "{stamp} {text}"));
    if let Err(error) = written {
        tracing::warn!(%error, "could not write mind log");
    }
}

/// Seconds since the epoch.
#[must_use]
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// `vivi mind service run`: the foreground service for a service manager.
///
/// # Errors
/// Fails when another mind holds the lock, pi cannot start, or the socket
/// cannot be bound.
pub async fn serve_forever() -> Result<(), VivariumError> {
    let config = config::load()?;
    std::fs::create_dir_all(&config.paths.home)?;
    let _lock =
        crate::registry::lock_file(config.paths.lock.clone(), "another vivi mind is running")?;
    if close_interrupted_wake(&config.paths.state, now())? {
        log_line(
            &config.paths.log,
            "previous wake was interrupted; closed as failed",
        );
    }
    let registry = crate::registry::path()?;
    let state_path = config.paths.state.clone();
    let shared = Arc::new(
        Shared::new(
            start_pi(&config).await?,
            config.authority.as_str(),
            config.paths.state.clone(),
        )
        .with_attach_context(Box::new(move |text| {
            attach_text(&registry, &state_path, &HealthProbe, text)
        })),
    );
    let listener = socket::bind(&config.paths.socket)?;
    tokio::spawn(socket::serve(
        listener,
        config.paths.socket.clone(),
        Arc::clone(&shared),
    ));
    log_line(
        &config.paths.log,
        &format!("mind started; authority {}", config.authority.as_str()),
    );
    wake_loop(&config, &shared).await?;
    match std::fs::remove_file(&config.paths.socket) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
        _ => {}
    }
    log_line(&config.paths.log, "mind stopped");
    Ok(())
}

/// A wake left open by a mind that died mid-turn would read as "in flight"
/// forever. Under the mind lock no other wake can be live, so close it as
/// failed (which also starts the backoff). Returns whether one was closed.
///
/// # Errors
/// Fails when the state file cannot be read or written.
pub fn close_interrupted_wake(state: &Path, now: u64) -> Result<bool, VivariumError> {
    store::update_at(state, |state| {
        let Some(record) = state
            .last_wake
            .as_mut()
            .filter(|record| record.ended_at.is_none())
        else {
            return Ok(false);
        };
        wake::finish(record, false, now);
        Ok(true)
    })
}

async fn start_pi(config: &MindConfig) -> Result<ProcessPi, VivariumError> {
    let state = store::load_at(&config.paths.state)?;
    let mut pi = ProcessPi::spawn(config, state.session_id.as_deref())?;
    // pi starts a fresh session when the recorded one has no file yet, so
    // record whatever session it is actually in.
    if let Some(id) = pi.session_id().await?
        && state.session_id.as_deref() != Some(id.as_str())
    {
        store::update_at(&config.paths.state, |state| {
            state.session_id = Some(id);
            Ok(())
        })?;
    }
    Ok(pi)
}

/// The actuator the config asks for: MCP calls when `[mcp]` names a server,
/// else a dry run. With `[edge]` as well, `rollback-edge` writes its request
/// for the root unit; a dry run never does.
///
/// # Errors
/// Fails when the key file cannot be read.
fn actuator_for(config: &MindConfig) -> Result<Box<dyn Actuator>, VivariumError> {
    let Some(mcp) = &config.mcp else {
        return Ok(Box::new(DryRun::default()));
    };
    let caller =
        HttpCaller::from_key_file(&mcp.server, &mcp.key_file).map_err(VivariumError::Config)?;
    log_line(
        &config.paths.log,
        &format!("acting through the server at {}", mcp.server),
    );
    let actuator: Box<dyn Actuator> = Box::new(McpActuator::new(caller));
    let Some(edge) = &config.edge else {
        return Ok(actuator);
    };
    log_line(
        &config.paths.log,
        &format!("watching edge builds through {}", edge.status.display()),
    );
    Ok(Box::new(EdgeActuator {
        inner: actuator,
        edge: edge.clone(),
    }))
}

async fn wake_loop(
    config: &MindConfig,
    shared: &Arc<Shared<ProcessPi>>,
) -> Result<(), VivariumError> {
    let registry = crate::registry::path()?;
    let clock = SystemClock;
    let mut actuator = actuator_for(config)?;
    let mut interval = tokio::time::interval(TICK);
    let mut stop = shared.shutdown.subscribe();
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    loop {
        tokio::select! {
            _ = interval.tick() => {}
            _ = stop.changed() => return Ok(()),
            _ = terminate.recv() => return Ok(()),
            _ = tokio::signal::ctrl_c() => return Ok(()),
        }
        respawn_if_exited(config, shared).await;
        let cx = TickContext {
            config,
            registry: &registry,
            probe: &HealthProbe,
            clock: &clock,
        };
        match tick(shared, &cx, &mut *actuator).await {
            Ok(outcome) => tracing::debug!(?outcome, "mind tick"),
            Err(error) => log_line(&config.paths.log, &format!("tick failed: {error}")),
        }
    }
}

/// A crashed pi is restarted on the same session; the mind itself stays up.
async fn respawn_if_exited(config: &MindConfig, shared: &Shared<ProcessPi>) {
    let mut pi = shared.pi.lock().await;
    if !pi.exited() {
        return;
    }
    log_line(
        &config.paths.log,
        "pi exited; restarting on the same session",
    );
    match start_pi(config).await {
        Ok(fresh) => *pi = fresh,
        Err(error) => log_line(&config.paths.log, &format!("pi restart failed: {error}")),
    }
}

#[cfg(test)]
#[path = "service_test.rs"]
mod tests;
