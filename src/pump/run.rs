//! Pump tick, status, and the run/once entry points.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::ErrorKind;
use std::io::Write;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use crate::VivariumError;
use crate::mailspace::Mailspace;
use crate::pump::boardpass::BoardPass;
use crate::pump::collect;
use crate::pump::config::{PumpConfig, PumpTarget};
use crate::pump::host::{Ctx, RealHost, shell_quote};
use crate::pump::plan;
use crate::pump::state::State;
use crate::pump::store::{DB_FILENAME, PumpStore};
use crate::pump::tick_steps;

mod reload;
pub(super) use reload::refresh_config;

/// One pump tick: fold board results, ingest and collect, apply job receipts,
/// dispatch ready work, request the periodic board pass, and write status.
/// Operator actions on pump records (requeue, withdraw, replan) apply after
/// the first ingest, so a requeued unit can dispatch in the same tick. The
/// board pass is bookkeeping; durable execution runs through dispatched pool jobs.
pub fn tick(ctx: &Ctx, st: &mut State, boardpass: &mut BoardPass) -> Result<(), VivariumError> {
    write_status(ctx, st);
    boardpass.apply(ctx, st);
    plan::ingest(ctx, st);
    if !ctx.mailspace.is_empty() {
        crate::pump::action::apply_pending(ctx, st);
    }
    collect_phase(ctx, st)?;
    tick_steps::soft_step(ctx, "flow", crate::pump::flow::tick::tick(ctx, st));
    save(ctx, st)?;
    let control = match crate::pump::control::read(&ctx.home) {
        Ok(control) => control,
        Err(error) => {
            ctx.log(&format!(
                "control state read failed; skipping dispatch: {error}"
            ));
            write_status(ctx, st);
            return Err(error);
        }
    };
    if control.mode == crate::pump::control::PumpMode::Paused {
        periodic(ctx, st, boardpass)?;
        write_status(ctx, st);
        return Ok(());
    }
    if !ctx.mailspace.is_empty() {
        crate::pump::cadence::prepare_due(ctx, st);
        save(ctx, st)?;
        tick_steps::soft_step(
            ctx,
            "dispatch-jobs",
            crate::pump::jobs::dispatch_pending(ctx, st),
        );
        save(ctx, st)?;
    }
    write_status(ctx, st);
    periodic(ctx, st, boardpass)?;
    write_status(ctx, st);
    Ok(())
}

fn collect_phase(ctx: &Ctx, st: &mut State) -> Result<(), VivariumError> {
    reconcile_closed_board_needs(ctx, st);
    let t_collect = Instant::now();
    collect::collect(ctx, st);
    ctx.log(&format!("collect took {}s", t_collect.elapsed().as_secs()));
    save(ctx, st)?;
    write_status(ctx, st);
    Ok(())
}

/// Withdraw unfinished units of needs closed on the board outside the pump.
fn reconcile_closed_board_needs(ctx: &Ctx, st: &mut State) {
    if ctx.mailspace.is_empty() {
        return;
    }
    if crate::pump::board::reconcile_done_needs(ctx, st).is_none() {
        ctx.log("closed-need reconcile skipped: done-need list unreadable");
    }
}

/// The periodic board pass, shared by the paused branch and the normal path:
/// request a board pass on the worker thread, ingest plans, raise the stall
/// and blocking alarms, then run the blocking-stuck automatic response before
/// the state is saved. The pass's results land on the tick that follows it.
fn periodic(ctx: &Ctx, st: &mut State, boardpass: &mut BoardPass) -> Result<(), VivariumError> {
    boardpass.request(ctx, st);
    plan::ingest(ctx, st);
    crate::pump::alarm::check(ctx, st);
    crate::pump::blockers::check(ctx, st);
    save(ctx, st)
}

/// Write the status lines to `<home>/status.txt`: a UTC timestamp, unit
/// counts, plan verdict counts, the merge counter, the stall lines from
/// [`crate::pump::alarm::status_lines`], the blocker lines from
/// [`crate::pump::blockers::status_lines`], the in-flight planner lines, then
/// the cadence lines. An idle board adds no planner lines. A write failure is
/// logged, never fatal.
pub fn write_status(ctx: &Ctx, st: &State) {
    let mut verdicts: BTreeMap<String, usize> = BTreeMap::new();
    for need in st.needs.values() {
        *verdicts.entry(need.verdict.clone()).or_default() += 1;
    }
    let mut lines = vec![
        utc_stamp(ctx.now()),
        counts_line("units", &st.counts()),
        counts_line("plans", &verdicts),
        format!(
            "merges {}  needs-ready {}",
            st.merges,
            st.closed_needs.len()
        ),
    ];
    lines.extend(crate::pump::alarm::status_lines(ctx, st));
    lines.extend(crate::pump::blockers::status_lines(ctx, st));
    lines.extend(planner_lines(ctx, st));
    lines.extend(crate::pump::cadence::status_lines(ctx, st));
    let path = ctx.home.join("status.txt");
    if let Err(err) = fs::write(&path, lines.join("\n") + "\n") {
        ctx.log(&format!("status write failed at {}: {err}", path.display()));
    }
}

/// The lines `status.txt` carries for durable planner jobs, their failure
/// counts. Empty when the board has nothing to report.
fn planner_lines(ctx: &Ctx, st: &State) -> Vec<String> {
    let mut lines = crate::pump::planner_jobs::status_lines(ctx, st);
    if !st.board.planner_fails.is_empty() {
        let counts: Vec<String> = st
            .board
            .planner_fails
            .iter()
            .map(|(handle, count)| format!("{handle}={count}"))
            .collect();
        lines.push(format!("planner-failures {}", counts.join(" ")));
    }
    lines
}

/// Run the pump over `project`: load the mailspace's `[pump]` table, take
/// the exclusive home lock, and loop [`tick`] until `once` or durable control
/// state stops it.
///
/// # Errors
/// Returns [`VivariumError::Config`] when the mailspace has no `[pump]`
/// table, the table fails `validate`, or a pump identity names no roster
/// role, an error when the lock is already
/// held, and whatever loading the state fails with.
pub fn run(project: &Path, once: bool) -> Result<(), VivariumError> {
    run_inner(project, once, false)
}

/// Run as a service-owned child and announce readiness after taking the pump lock.
pub fn run_service_child(project: &Path) -> Result<(), VivariumError> {
    run_inner(project, false, true)
}

/// The pump's actor identity: `<address-prefix>/<sync-name>`, the prefix
/// alone without a sync name, `unknown` without a mailspace id.
#[must_use]
pub fn actor_string(mailspace: &Mailspace) -> String {
    let prefix = mailspace.address_prefix();
    if prefix.is_empty() {
        return String::from("unknown");
    }
    match mailspace
        .config
        .sync
        .as_ref()
        .and_then(|s| s.name.as_deref())
    {
        Some(name) => format!("{prefix}/{name}"),
        None => prefix.to_owned(),
    }
}

pub(crate) fn run_inner(
    project: &Path,
    once: bool,
    announce_ready: bool,
) -> Result<(), VivariumError> {
    crate::mailspace::inherit::refuse_base(project)?;
    let mailspace = Mailspace::discover(Some(project))?;
    let cfg = pump_config(&mailspace)?;
    let issues = crate::pump::readiness::assess(&mailspace);
    for issue in &issues {
        if issue.severity == crate::routing::Severity::Warning {
            eprintln!("vivi: warning: {}", issue.message);
        }
    }
    crate::routing::require_ok(&issues)?;
    let home = cfg.home_dir(project);
    fs::create_dir_all(&home)?;
    let lock = take_lock(&home)?;
    let mut control = crate::pump::control::initialize_for_run(&home)?;
    if control.shutdown_requested {
        crate::pump::control::request_shutdown(&home, false)?;
        control.shutdown_requested = false;
    }
    if !crate::pump::readiness::is_ready(&issues) {
        let mode = crate::pump::hold::HoldMode {
            once,
            announce_ready,
        };
        return crate::pump::hold::hold(project, &home, cfg.tick, issues, control, mode, lock);
    }
    let host = Arc::new(RealHost::new(&home));
    let vivi = std::env::current_exe()
        .map_or_else(|_| String::from("vivi"), |exe| exe.display().to_string());
    let sources = cfg.pool_sources(&mailspace.config, project);
    let ctx = Ctx::new(cfg, host, project.to_path_buf(), vivi)
        .with_routing(crate::routing::from_mailspace(&mailspace.config))
        .with_actor(actor_string(&mailspace))
        .with_mailspace(mailspace.mailspace_id().to_string())
        .with_pool_sources(sources);
    crate::pump::priority::import_legacy(&ctx);
    if announce_ready {
        println!("VIVI_PUMP_SERVICE_READY");
        std::io::stdout().flush()?;
    }
    if control.mode == crate::pump::control::PumpMode::Stopped {
        ctx.log("pump is stopped; run vivi pump start to enable it");
        return Ok(());
    }
    for line in crate::pump::apfssweep::sweep_apfs(&ctx, &crate::pool::ramdisk::RealRunner) {
        ctx.log(&line);
    }
    for line in crate::pump::ramsweep::sweep_ramdisks(&ctx, &crate::pool::ramdisk::RealRunner) {
        ctx.log(&line);
    }
    run_with_locked(&ctx, once)?;
    drop(lock);
    Ok(())
}

/// Load state and drive the tick loop over an already-built context, the
/// testable half of [`run`].
///
/// # Errors
/// Returns whatever loading the state fails with.
#[cfg(test)]
pub(crate) fn run_with(ctx: &Ctx, once: bool) -> Result<(), VivariumError> {
    run_with_lock_policy(ctx, once, false)
}

fn run_with_locked(ctx: &Ctx, once: bool) -> Result<(), VivariumError> {
    run_with_lock_policy(ctx, once, true)
}

fn run_with_lock_policy(
    ctx: &Ctx,
    once: bool,
    caller_holds_pump_lock: bool,
) -> Result<(), VivariumError> {
    ensure_repo(ctx)?;
    let mut run_ctx = ctx.clone();
    let mut store = if caller_holds_pump_lock {
        PumpStore::open_for_run(&ctx.home.join(DB_FILENAME))?
    } else {
        PumpStore::open(&ctx.home.join(DB_FILENAME))?
    };
    let json_path = ctx.home.join("state.json");
    let mut st = if store.is_empty()? && json_path.is_file() {
        let mut imported = State::load(&json_path)?;
        imported.normalize_legacy();
        store.save(&imported)?;
        let archived = ctx.home.join("state.json.imported");
        fs::rename(&json_path, &archived)?;
        ctx.log(&format!(
            "imported state.json into pump.db; archived at {}",
            archived.display()
        ));
        imported
    } else {
        store.load()?
    };
    // Reattach pool jobs so a dispatched job stays owned by its durable record
    // after a pump restart.
    if !ctx.mailspace.is_empty() {
        crate::pump::jobs::recover_dispatched(ctx, &mut st)?;
    }
    reconcile_closed_board_needs(ctx, &mut st);
    plan::ingest(ctx, &mut st);
    store.save(&st)?;
    let mut boardpass = BoardPass::start(&run_ctx);
    drive(&mut run_ctx, &mut st, &mut boardpass, once)
}

/// Startup check: the integration clone must exist. A missing `repo` is
/// cloned from `remote_url` (the clone's remote is named `remote`, on the
/// integration branch); with no `remote_url` the pump refuses to start.
///
/// # Errors
/// Returns [`VivariumError::Config`] when the clone is missing and no
/// `remote_url` is set, and [`VivariumError::Message`] when the clone fails.
pub(crate) fn ensure_repo(ctx: &Ctx) -> Result<(), VivariumError> {
    if ctx.cfg.targets.is_empty() {
        return ensure_single_repo(ctx);
    }
    for (id, target) in ctx.cfg.target_configs() {
        ensure_target_repo(ctx, &id, target)?;
    }
    Ok(())
}

/// Ensure the integration clone of one named repository target exists.
///
/// # Errors
/// Returns [`VivariumError::Config`] when the target path is missing or not a
/// Git repository, and [`VivariumError::Message`] when the clone fails.
pub(in crate::pump) fn ensure_target_repo(
    ctx: &Ctx,
    id: &str,
    target: PumpTarget,
) -> Result<(), VivariumError> {
    let source = ctx
        .cfg
        .target_path(&ctx.project, &id)
        .ok_or_else(|| VivariumError::Config(format!("repository target {id:?} has no path")))?;
    if !source.is_dir() {
        return Err(VivariumError::Config(format!(
            "repository target {id:?} path {} does not exist",
            source.display()
        )));
    }
    let quoted_source = shell_quote(&source.to_string_lossy());
    let probe = ctx.sh(
        &format!("git -C {quoted_source} rev-parse --show-toplevel"),
        None,
        30,
    );
    if probe.code != 0 {
        return Err(VivariumError::Config(format!(
            "repository target {id:?} path {} is not a Git repository",
            source.display()
        )));
    }
    let remote_url = target
        .remote_url
        .or_else(|| {
            let out = ctx.sh(
                &format!(
                    "git -C {quoted_source} remote get-url {}",
                    shell_quote(&target.remote)
                ),
                None,
                30,
            );
            (out.code == 0 && !out.text.trim().is_empty()).then(|| out.text.trim().to_owned())
        })
        .unwrap_or_else(|| source.to_string_lossy().into_owned());
    let mut target_cfg = ctx
        .cfg
        .for_target(&id)
        .ok_or_else(|| VivariumError::Config(format!("repository target {id:?} disappeared")))?;
    target_cfg.remote_url = Some(remote_url);
    let repo = ctx.cfg.target_repo_dir(&ctx.project, &id).ok_or_else(|| {
        VivariumError::Config(format!("repository target {id:?} has no integration path"))
    })?;
    let target_ctx = Ctx::with_parts(
        target_cfg,
        ctx.host.clone(),
        ctx.project.clone(),
        ctx.vivi.clone(),
        ctx.shell_prefix.clone(),
        ctx.home.clone(),
        repo,
    );
    ensure_single_repo(&target_ctx)?;
    Ok(())
}

fn ensure_single_repo(ctx: &Ctx) -> Result<(), VivariumError> {
    if ctx.repo.exists() {
        return Ok(());
    }
    let Some(url) = ctx.cfg.remote_url.as_deref().filter(|u| !u.is_empty()) else {
        return Err(VivariumError::Config(format!(
            "integration clone {} does not exist and [pump] remote_url is not set; \
             create the clone or set remote_url",
            ctx.repo.display()
        )));
    };
    if let Some(parent) = ctx.repo.parent() {
        fs::create_dir_all(parent)?;
    }
    let cmd = clone_command(ctx, url);
    ctx.log(&format!("cloning {url} into {}", ctx.repo.display()));
    let out = ctx.sh(&cmd, None, 900);
    if out.code != 0 {
        return Err(VivariumError::Message(format!(
            "cloning {url} into {} failed (exit {}): {}",
            ctx.repo.display(),
            out.code,
            out.text.trim()
        )));
    }
    Ok(())
}

/// `git clone -o <remote> --branch <main> <url> <repo>`, quoted.
fn clone_command(ctx: &Ctx, url: &str) -> String {
    format!(
        "git clone -q -o {} --branch {} {} {}",
        shell_quote(&ctx.cfg.remote),
        shell_quote(&ctx.cfg.main),
        shell_quote(url),
        shell_quote(&ctx.repo.to_string_lossy())
    )
}

/// The run/once loop: reload, tick, then return when control state is
/// stopped, else wait for the next tick in one-second slices. A finished
/// board pass always lands before a return. A panic inside a tick is caught,
/// logged as `TICK ERROR`, and the state saved.
///
/// Every tick starts from a fresh load of `<home>/pump.db` (`reload`), and
/// the exit paths reload before they apply the board pass and save, so no
/// tick acts on, and no save writes back, a `State` older than the store. A
/// change made to pump.db between two ticks (a stop and start, a job
/// removed or enqueued by another handle) is seen by the next tick. What the
/// process still keeps across ticks: the board-pass worker thread with its
/// in-flight snapshot, the config fingerprint and last config error, the
/// cycle counter, and the process-local alarm dedupe maps
/// (`tick_steps::should_raise`; the flow raise-once and step counters belong
/// to the flow rework). The `State` parameter is only a slot the reload
/// overwrites.
fn drive(
    ctx: &mut Ctx,
    st: &mut State,
    boardpass: &mut BoardPass,
    once: bool,
) -> Result<(), VivariumError> {
    let mut config_fingerprint = None;
    let mut config_error = None;
    let mut cycle_sequence = 0;
    loop {
        reload(ctx, st)?;
        if exit_requested(ctx) {
            boardpass.apply(ctx, st);
            save(ctx, st)?;
            return Ok(());
        }
        refresh_config(ctx, &mut config_fingerprint, &mut config_error);
        run_recorded_tick(ctx, st, boardpass, &mut cycle_sequence)?;
        if once {
            return land_board_pass(ctx, st, boardpass);
        }
        if wait_for_next_tick(ctx) {
            ctx.log("pump control state requests exit");
            return land_board_pass(ctx, st, boardpass);
        }
    }
}

/// Reload the state from pump.db, then fold in a finished board pass and save.
/// The reload keeps the final save from overwriting changes made to the store
/// since the last tick.
fn land_board_pass(
    ctx: &Ctx,
    st: &mut State,
    boardpass: &mut BoardPass,
) -> Result<(), VivariumError> {
    reload(ctx, st)?;
    boardpass.apply(ctx, st);
    save(ctx, st)
}

fn run_recorded_tick(
    ctx: &Ctx,
    st: &mut State,
    boardpass: &mut BoardPass,
    sequence: &mut u64,
) -> Result<(), VivariumError> {
    let cycle_id = crate::pump::cycle::new_id(*sequence);
    *sequence = (*sequence).saturating_add(1);
    let started_at = epoch_seconds();
    let recorded = record_cycle_start(ctx, &cycle_id, started_at);
    let started = Instant::now();
    let outcome = catch_unwind(AssertUnwindSafe(|| tick(ctx, st, boardpass)));
    let duration = started.elapsed().as_secs();
    let ended_at = epoch_seconds();
    match outcome {
        Ok(Ok(())) => finish_cycle(ctx, recorded, &cycle_id, ended_at, duration, "ok"),
        Ok(Err(error)) => {
            finish_cycle(ctx, recorded, &cycle_id, ended_at, duration, "error");
            return Err(error);
        }
        Err(error) => {
            finish_cycle(ctx, recorded, &cycle_id, ended_at, duration, "panic");
            ctx.log(&format!("TICK ERROR {}", panic_note(&error)));
            save(ctx, st)?;
        }
    }
    Ok(())
}

fn record_cycle_start(ctx: &Ctx, id: &str, started_at: u64) -> bool {
    match crate::pump::cycle::begin(&ctx.home, id.to_owned(), started_at) {
        Ok(()) => true,
        Err(error) => {
            ctx.log(&format!("pump monitor cycle start receipt failed: {error}"));
            false
        }
    }
}

fn finish_cycle(
    ctx: &Ctx,
    recorded: bool,
    id: &str,
    ended_at: u64,
    duration_secs: u64,
    outcome: &str,
) {
    if recorded
        && let Err(error) =
            crate::pump::cycle::finish(&ctx.home, id, ended_at, duration_secs, outcome)
    {
        ctx.log(&format!(
            "pump monitor cycle finish receipt failed: {error}"
        ));
    }
}

fn epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

/// Sleep until the next tick is due in one-second slices, re-reading control
/// state after each slice so stop and pause take effect within about a
/// second. `true` means control asked the pump to exit.
fn wait_for_next_tick(ctx: &Ctx) -> bool {
    let deadline = Instant::now() + Duration::from_secs(ctx.cfg.tick);
    loop {
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        thread::sleep((deadline - now).min(Duration::from_secs(1)));
        if exit_requested(ctx) {
            return true;
        }
    }
}

fn exit_requested(ctx: &Ctx) -> bool {
    match crate::pump::control::read(&ctx.home) {
        Ok(control) => {
            control.mode == crate::pump::control::PumpMode::Stopped || control.shutdown_requested
        }
        Err(error) => {
            ctx.log(&format!(
                "control state read failed; stopping pump: {error}"
            ));
            true
        }
    }
}

/// The `[pump]` table of a mailspace, or the config error naming it.
fn pump_config(mailspace: &Mailspace) -> Result<PumpConfig, VivariumError> {
    mailspace.config.pump.clone().ok_or_else(|| {
        VivariumError::Config(String::from("no [pump] table in .vivi/mailspace.toml"))
    })
}

/// Create `<home>/lock` and take the exclusive non-blocking flock.
///
/// # Errors
/// Returns an error naming the other pump when the lock is already held,
/// and whatever creating the file fails with.
fn take_lock(home: &Path) -> Result<File, VivariumError> {
    let lock = File::create(home.join("lock"))?;
    if lock.try_lock().is_err() {
        return Err(VivariumError::Message(String::from(
            "another vivi pump is running",
        )));
    }
    Ok(lock)
}

/// Whether another pump process currently owns this project's lock.
pub fn lock_is_held(home: &Path) -> Result<bool, VivariumError> {
    let path = home.join("lock");
    let lock = match OpenOptions::new().read(true).write(true).open(path) {
        Ok(lock) => lock,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    Ok(lock.try_lock().is_err())
}

/// Replace the in-memory state with the contents of `<home>/pump.db`; stop the
/// controller if the store cannot be read.
fn reload(ctx: &Ctx, st: &mut State) -> Result<(), VivariumError> {
    let path = ctx.home.join(DB_FILENAME);
    *st = PumpStore::open(&path)
        .and_then(|store| store.load())
        .map_err(|err| {
            let message = format!("state reload failed at {}: {err}", path.display());
            ctx.log(&message);
            VivariumError::Message(message)
        })?;
    Ok(())
}

/// Save the state to `<home>/pump.db`; stop the controller if it cannot be saved.
fn save(ctx: &Ctx, st: &State) -> Result<(), VivariumError> {
    let path = ctx.home.join(DB_FILENAME);
    PumpStore::open(&path)
        .and_then(|mut store| store.save(st))
        .map_err(|err| {
            let message = format!("state save failed at {}: {err}", path.display());
            ctx.log(&message);
            VivariumError::Message(message)
        })
}

/// `"label k=v k=v"` from sorted counts, joined with single spaces
/// (an empty map leaves the trailing space).
fn counts_line(label: &str, counts: &BTreeMap<String, usize>) -> String {
    let joined = counts
        .iter()
        .map(|(name, count)| format!("{name}={count}"))
        .collect::<Vec<String>>()
        .join(" ");
    format!("{label} {joined}")
}

/// `%Y-%m-%dT%H:%M:%SZ` in UTC for epoch seconds.
pub(crate) fn utc_stamp(secs: u64) -> String {
    let stamp = i64::try_from(secs).unwrap_or(0);
    chrono::DateTime::<chrono::Utc>::from_timestamp(stamp, 0).map_or_else(String::new, |time| {
        time.format("%Y-%m-%dT%H:%M:%SZ").to_string()
    })
}

/// The panic payload as one logged line: newlines folded to `" | "`, cut at
/// 900 characters like a traceback tail.
fn panic_note(err: &(dyn std::any::Any + Send)) -> String {
    let text = err
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| err.downcast_ref::<&str>().map(|s| (*s).to_string()))
        .unwrap_or_else(|| String::from("unknown panic"));
    text.replace('\n', " | ").chars().take(900).collect()
}
