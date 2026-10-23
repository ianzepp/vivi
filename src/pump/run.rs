//! Pump tick, status, and the run/once entry points.

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
use crate::pump::config::PumpConfig;
use crate::pump::host::{Ctx, RealHost, shell_quote};
use crate::pump::plan;
use crate::pump::state::State;
use crate::pump::store::{DB_FILENAME, PumpStore};
use crate::pump::tick_steps;

/// One pump tick: fold board results, ingest and collect, apply job receipts,
/// dispatch ready work, request the periodic board pass, and write status.
/// Operator actions on pump records (requeue, withdraw, replan) apply after
/// the first ingest, so a requeued unit can dispatch in the same tick. The
/// board pass is bookkeeping; durable execution runs through dispatched pool jobs.
pub fn tick(ctx: &Ctx, st: &mut State, boardpass: &mut BoardPass) -> Result<(), VivariumError> {
    write_status(ctx, st, &[]);
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
            write_status(ctx, st, &[]);
            return Err(error);
        }
    };
    if control.mode == crate::pump::control::PumpMode::Paused {
        periodic(ctx, st, boardpass)?;
        write_status(ctx, st, &[]);
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
    write_status(ctx, st, &[]);
    periodic(ctx, st, boardpass)?;
    write_status(ctx, st, &[]);
    Ok(())
}

fn collect_phase(ctx: &Ctx, st: &mut State) -> Result<(), VivariumError> {
    reconcile_closed_board_needs(ctx, st);
    let t_collect = Instant::now();
    collect::collect(ctx, st);
    ctx.log(&format!("collect took {}s", t_collect.elapsed().as_secs()));
    save(ctx, st)?;
    write_status(ctx, st, &[]);
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
/// any lane lines supplied by a caller. The dispatcher path supplies no
/// process-owned lane lines. An idle board adds no planner lines. A write
/// failure is logged, never fatal.
pub fn write_status(ctx: &Ctx, st: &State, lanes: &[String]) {
    let mut verdicts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
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
    lines.extend(lanes.iter().cloned());
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

/// The pump's status text plus its durable control mode, whether a writer
/// process holds the pump lock, and how old the snapshot is. No pump lock is
/// taken.
///
/// # Errors
/// Returns [`VivariumError::Config`] when a mailspace is found but carries
/// no `[pump]` table.
pub fn status(project: &Path) -> Result<String, VivariumError> {
    let cfg = match Mailspace::discover(Some(project)) {
        Ok(mailspace) => pump_config(&mailspace)?,
        // No mailspace to read: only the default home could be meant, and
        // it cannot hold a status file yet.
        Err(_) => PumpConfig::default(),
    };
    let home = cfg.home_dir(project);
    let path = home.join("status.txt");
    let text = fs::read_to_string(&path).unwrap_or_else(|_| String::from("no status yet\n"));
    let control = crate::pump::control::inspect(&home)?;
    let writer = match lock_is_held(&home) {
        Ok(true) => "running",
        Ok(false) => "not running",
        Err(_) => "unknown",
    };
    Ok(format!(
        "{text}control  {}\nwriter   {writer}\nage      {}\n",
        control.mode,
        status_age(&path)
    ))
}

/// How old `status.txt` is, in whole seconds, or `never` when it is missing
/// or its modification time cannot be read.
fn status_age(path: &Path) -> String {
    let Ok(modified) = fs::metadata(path).and_then(|meta| meta.modified()) else {
        return String::from("never");
    };
    let age = SystemTime::now()
        .duration_since(modified)
        .map_or(0, |elapsed| elapsed.as_secs());
    format!("{age}s")
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
    // Reattach pool jobs before repairing stale merge states so a dispatched
    // merge remains owned by its durable job after a pump restart.
    if !ctx.mailspace.is_empty() {
        crate::pump::jobs::recover_dispatched(ctx, &mut st)?;
    }
    st.recover_after_restart();
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
    if !ctx.cfg.targets.is_empty() {
        for (id, target) in ctx.cfg.target_configs() {
            let source = ctx.cfg.target_path(&ctx.project, &id).ok_or_else(|| {
                VivariumError::Config(format!("repository target {id:?} has no path"))
            })?;
            if !source.is_dir() {
                return Err(VivariumError::Config(format!(
                    "repository target {id:?} path {} does not exist",
                    source.display()
                )));
            }
            let probe = ctx.sh(
                &format!(
                    "git -C {} rev-parse --show-toplevel",
                    shell_quote(&source.to_string_lossy())
                ),
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
                            "git -C {} remote get-url {}",
                            shell_quote(&source.to_string_lossy()),
                            shell_quote(&target.remote)
                        ),
                        None,
                        30,
                    );
                    (out.code == 0 && !out.text.trim().is_empty())
                        .then(|| out.text.trim().to_owned())
                })
                .unwrap_or_else(|| source.to_string_lossy().into_owned());
            let mut target_cfg = ctx.cfg.for_target(&id).ok_or_else(|| {
                VivariumError::Config(format!("repository target {id:?} disappeared"))
            })?;
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
        }
        return Ok(());
    }
    ensure_single_repo(ctx)
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

/// `git clone -o <remote> --branch <branch> <url> <repo>`, quoted.
fn clone_command(ctx: &Ctx, url: &str) -> String {
    format!(
        "git clone -q -o {} --branch {} {} {}",
        shell_quote(&ctx.cfg.remote),
        shell_quote(&ctx.cfg.branch),
        shell_quote(url),
        shell_quote(&ctx.repo.to_string_lossy())
    )
}

/// The run/once loop: tick, then return when control state is stopped, save
/// and return, else wait for the next tick in one-second slices. A finished
/// board pass always lands before a return. A panic inside a tick is caught,
/// logged as `TICK ERROR`, and the state saved.
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
        if exit_requested(ctx) {
            boardpass.apply(ctx, st);
            save(ctx, st)?;
            return Ok(());
        }
        refresh_config(ctx, &mut config_fingerprint, &mut config_error);
        run_recorded_tick(ctx, st, boardpass, &mut cycle_sequence)?;
        if once {
            boardpass.apply(ctx, st);
            save(ctx, st)?;
            return Ok(());
        }
        if wait_for_next_tick(ctx) {
            ctx.log("pump control state requests exit");
            boardpass.apply(ctx, st);
            save(ctx, st)?;
            return Ok(());
        }
    }
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

/// Refresh the live mailspace config between ticks. The pump home and target
/// checkout paths stay fixed for the life of the lock; valid pool, routing,
/// dispatch, merge, gate, budget, and check changes apply to the next tick.
pub(super) fn refresh_config(
    ctx: &mut Ctx,
    fingerprint: &mut Option<Vec<u8>>,
    last_error: &mut Option<String>,
) -> bool {
    let loaded = match load_runtime_config(&ctx.project) {
        Ok(loaded) => loaded,
        Err(error) => return report_config_error(ctx, last_error, error.to_string()),
    };
    if fingerprint.as_ref() == Some(&loaded.fingerprint) {
        *last_error = None;
        return false;
    }
    if !runtime_paths_match(ctx, &loaded.cfg) {
        return report_config_error(
            ctx,
            last_error,
            String::from("pump home or repository target paths changed; restart required"),
        );
    }
    let changed = fingerprint.is_some();
    ctx.shell_prefix.clone_from(&loaded.cfg.shell_prefix);
    ctx.cfg = Arc::new(loaded.cfg);
    ctx.routing = Arc::new(loaded.routing);
    ctx.pool_sources = Arc::new(loaded.pool_sources);
    ctx.actor = loaded.actor;
    ctx.mailspace = loaded.mailspace;
    *fingerprint = Some(loaded.fingerprint);
    *last_error = None;
    if changed {
        ctx.log("pump config reloaded");
    }
    changed
}

/// Read and validate one complete runtime config snapshot.
fn load_runtime_config(project: &Path) -> Result<LoadedRuntimeConfig, VivariumError> {
    let mailspace = Mailspace::discover(Some(project))?;
    let cfg = pump_config(&mailspace)?;
    let issues = crate::pump::readiness::assess(&mailspace);
    crate::routing::require_ok(&issues)?;
    if let Some(summary) = crate::routing::not_ready_summary(&issues) {
        return Err(VivariumError::Config(summary));
    }
    let fingerprint = serde_json::to_vec(&mailspace.config)
        .map_err(|error| VivariumError::Message(error.to_string()))?;
    Ok(LoadedRuntimeConfig {
        pool_sources: cfg.pool_sources(&mailspace.config, project),
        routing: crate::routing::from_mailspace(&mailspace.config),
        actor: actor_string(&mailspace),
        mailspace: mailspace.mailspace_id().to_string(),
        cfg,
        fingerprint,
    })
}

struct LoadedRuntimeConfig {
    cfg: PumpConfig,
    routing: crate::routing::RoleRouting,
    pool_sources: std::collections::BTreeMap<String, std::path::PathBuf>,
    actor: String,
    mailspace: String,
    fingerprint: Vec<u8>,
}

/// Reject live changes that would move the lock or an integration checkout.
fn runtime_paths_match(ctx: &Ctx, cfg: &PumpConfig) -> bool {
    if cfg.home_dir(&ctx.project) != ctx.home {
        return false;
    }
    runtime_repo_paths(&ctx.cfg, &ctx.project) == runtime_repo_paths(cfg, &ctx.project)
}

fn runtime_repo_paths(
    cfg: &PumpConfig,
    project: &Path,
) -> std::collections::BTreeMap<String, std::path::PathBuf> {
    cfg.target_configs()
        .keys()
        .filter_map(|id| {
            cfg.target_repo_dir(project, id)
                .map(|path| (id.clone(), path))
        })
        .collect()
}

fn report_config_error(ctx: &Ctx, last_error: &mut Option<String>, error: String) -> bool {
    if last_error.as_deref() != Some(error.as_str()) {
        ctx.log(&format!("pump config reload skipped: {error}"));
    }
    *last_error = Some(error);
    false
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

/// `vivi pump closeout`: list complete-but-open needs, or close one.
///
/// # Errors
/// Returns [`VivariumError`] when the pump state is missing or unreadable,
/// `--close` names a need that is not complete and open, `--for` is missing
/// with `--close`, or the lifecycle move fails.
pub fn closeout(
    project: &Path,
    json: bool,
    close: Option<&str>,
    for_identity: Option<&str>,
) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(Some(project))?;
    let cfg = pump_config(&mailspace)?;
    let home = cfg.home_dir(project);
    let snapshot = read_closeout_snapshot(&home)?;
    let rows = crate::pump::closeout::complete_open(&mailspace, &snapshot)?;
    if let Some(handle) = close {
        let current_snapshot = read_closeout_snapshot(&home)?;
        let current_rows = crate::pump::closeout::complete_open(&mailspace, &current_snapshot)?;
        let row = current_rows
            .iter()
            .find(|row| row.handle == handle || row.handle.starts_with(handle))
            .ok_or_else(|| {
                VivariumError::Message(format!("{handle}: not a complete-but-open need"))
            })?;
        let receipt = crate::pump::closeout::close(&mailspace, row, for_identity)?;
        println!("{receipt}");
        return Ok(());
    }
    if json {
        let text = serde_json::to_string_pretty(&rows)
            .map_err(|err| VivariumError::Message(err.to_string()))?;
        println!("{text}");
        return Ok(());
    }
    println!("complete but open: {}", rows.len());
    println!("  handle  units  tip  from  completed  subject");
    for row in &rows {
        let tip: String = row.tip.as_deref().unwrap_or("-").chars().take(10).collect();
        let completed = row
            .completed_at
            .map_or_else(|| String::from("-"), utc_stamp);
        println!(
            "  {}  {}/{}  {}  {}  {}  {}",
            row.handle, row.merged, row.units, tip, row.from, completed, row.subject
        );
    }
    Ok(())
}

fn read_closeout_snapshot(home: &Path) -> Result<crate::pump::report::Snapshot, VivariumError> {
    match crate::pump::report::read(home)? {
        crate::pump::report::PumpRead::Ready(snapshot) => Ok(snapshot),
        crate::pump::report::PumpRead::Missing => Err(VivariumError::Message(format!(
            "no pump.db at {}",
            home.join(DB_FILENAME).display()
        ))),
        crate::pump::report::PumpRead::Unreadable(why) => {
            Err(VivariumError::Message(format!("pump.db unreadable: {why}")))
        }
    }
}

/// Write the live `pump.db` as the `state.json` shape at `out`.
///
/// # Errors
/// Returns [`VivariumError`] when the mailspace or pump config is missing,
/// `pump.db` does not exist, or the export cannot be written.
pub fn export(project: &Path, out: &Path) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(Some(project))?;
    let cfg = pump_config(&mailspace)?;
    let path = cfg.home_dir(project).join(DB_FILENAME);
    let store = PumpStore::open_read_only(&path)?
        .ok_or_else(|| VivariumError::Message(format!("no pump.db at {}", path.display())))?;
    store.export_json(out)
}

/// `"label k=v k=v"` from sorted counts, joined with single spaces
/// (an empty map leaves the trailing space).
fn counts_line(label: &str, counts: &std::collections::BTreeMap<String, usize>) -> String {
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
