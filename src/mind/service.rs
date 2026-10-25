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
use super::pi::{PiSession, ProcessPi};
use super::probe::{HealthProbe, Probe, ProjectSignals};
use super::prompt;
use super::socket::{self, BusyGuard, Shared};
use super::store::{self, MindState};
use super::wake::{self, WakeDecision, WakeInputs, WakeRecord};

/// How often the loop re-evaluates wakes. Heartbeats are coarser than this.
pub const TICK: Duration = Duration::from_secs(60);

/// What one tick did.
#[derive(Debug)]
pub enum TickOutcome {
    Slept(&'static str),
    Woke { planned: Vec<Planned> },
    Failed(String),
}

/// Everything one tick reads besides the session.
pub struct TickContext<'a, Q: Probe> {
    pub config: &'a MindConfig,
    pub registry: &'a Path,
    pub probe: &'a Q,
    pub now: u64,
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
    let state_path = &cx.config.paths.state;
    let state = store::load_at(state_path)?;
    let watched = super::projects::watched_at(cx.registry, &state.ignored)?;
    let signals: Vec<ProjectSignals> = watched.iter().map(|root| cx.probe.probe(root)).collect();
    let decision = wake::decide(&WakeInputs {
        now: cx.now,
        heartbeat: &cx.config.heartbeat,
        last: state.last_wake.as_ref(),
        recent: &state.recent_wakes,
        max_wakes_per_hour: cx.config.budgets.max_wakes_per_hour,
        paused: state.paused,
        signals: &signals,
    });
    let (reasons, attempt, seen) = match decision {
        WakeDecision::Sleep(why) => return Ok(TickOutcome::Slept(why)),
        WakeDecision::Wake {
            reasons,
            attempt,
            seen,
        } => (reasons, attempt, seen),
    };
    let record = begin_wake(state_path, cx.now, attempt, seen)?;
    let mut message = prompt::wake_prompt(cx.config.authority, &reasons, &signals);
    if let Some(report) = &state.pending_report {
        message = format!("{report}\n{message}");
    }
    let turn = {
        let mut pi = shared.pi.lock().await;
        let _busy = BusyGuard::hold(&shared.busy);
        pi.prompt(message, None).await
    };
    finish_wake(cx, record, turn, &watched, actuator)
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

fn finish_wake<Q: Probe>(
    cx: &TickContext<'_, Q>,
    mut record: WakeRecord,
    turn: Result<super::pi::TurnSummary, VivariumError>,
    watched: &[PathBuf],
    actuator: &mut dyn Actuator,
) -> Result<TickOutcome, VivariumError> {
    let config = cx.config;
    let summary = match turn {
        Ok(summary) => summary,
        Err(error) => {
            wake::finish(&mut record, false, cx.now);
            store::update_at(&config.paths.state, |state| {
                state.last_wake = Some(record);
                Ok(())
            })?;
            log_line(&config.paths.log, &format!("wake failed: {error}"));
            return Ok(TickOutcome::Failed(error.to_string()));
        }
    };
    log_line(&config.paths.log, &format!("wake turn:\n{}", summary.text));
    let planned = store::update_at(&config.paths.state, |state| {
        let planned = apply_envelope(config, state, &summary.text, watched, cx.now, actuator);
        wake::finish(&mut record, true, cx.now);
        state.last_wake = Some(record);
        state.pending_report = (!planned.is_empty()).then(|| prompt::plan_report(&planned));
        Ok(planned)
    })?;
    Ok(TickOutcome::Woke { planned })
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
    let ledger = state.ledger.clone();
    let gate = Gate {
        stage: config.authority,
        paused: state.paused,
        budgets: &config.budgets,
        ledger: &ledger,
        now,
    };
    let planned = actions::plan(requests, &gate, watched, &mut state.ledger);
    for item in &planned {
        let line = match &item.verdict {
            Ok(()) => actuator
                .apply(&item.request)
                .unwrap_or_else(|error| format!("failed: {error}")),
            Err(refusal) => format!("refused {:?}: {refusal}", item.request.kind),
        };
        log_line(&config.paths.log, &line);
    }
    planned
}

/// Append one timestamped line to the mind log. Logging never fails the
/// service.
pub fn log_line(path: &Path, text: &str) {
    let stamp = chrono::Utc::now().to_rfc3339();
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
    let shared = Arc::new(Shared::new(
        start_pi(&config).await?,
        config.authority.as_str(),
        config.paths.state.clone(),
    ));
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
    std::fs::remove_file(&config.paths.socket)?;
    log_line(&config.paths.log, "mind stopped");
    Ok(())
}

async fn start_pi(config: &MindConfig) -> Result<ProcessPi, VivariumError> {
    let state = store::load_at(&config.paths.state)?;
    let mut pi = ProcessPi::spawn(config, state.session_id.as_deref())?;
    if state.session_id.is_none()
        && let Some(id) = pi.session_id().await?
    {
        store::update_at(&config.paths.state, |state| {
            state.session_id = Some(id);
            Ok(())
        })?;
    }
    Ok(pi)
}

async fn wake_loop(
    config: &MindConfig,
    shared: &Arc<Shared<ProcessPi>>,
) -> Result<(), VivariumError> {
    let registry = crate::registry::path()?;
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
            now: now(),
        };
        match tick(shared, &cx, &mut DryRun::default()).await {
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
