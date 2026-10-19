//! Pump tick, status, and the run/once entry points.

use std::fs::{self, File, OpenOptions};
use std::io::ErrorKind;
use std::io::Write;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::VivariumError;
use crate::mailspace::Mailspace;
use crate::pump::board;
use crate::pump::collect;
use crate::pump::config::PumpConfig;
use crate::pump::dispatch;
use crate::pump::gate;
use crate::pump::host::{Ctx, RealHost, shell_quote};
use crate::pump::merge::Merger;
use crate::pump::plan;
use crate::pump::state::State;
use crate::pump::store::{DB_FILENAME, PumpStore};

/// One pump tick: ingest and collect, feed merges, dispatch ready work, run
/// gates, then perform the periodic board pass and write status.
pub fn tick(ctx: &Ctx, st: &mut State, merger: &Merger) -> Result<(), VivariumError> {
    merger.apply(ctx, st);
    plan::ingest(ctx, st);
    let gate_target = st
        .active_gate_target()
        .unwrap_or_else(|| String::from("default"));
    let gate_ctx = ctx.for_target(&gate_target).unwrap_or_else(|| ctx.clone());
    crate::pump::state::with_target_state(st, &gate_target, |st| {
        if let Some(text) = collect::collect(&gate_ctx, st) {
            gate::finish_gate(&gate_ctx, st, &text);
        }
    });
    merger.apply(ctx, st);
    merger.feed(ctx, st);
    save(ctx, st)?;
    let control = match crate::pump::control::read(&ctx.home) {
        Ok(control) => control,
        Err(error) => {
            ctx.log(&format!(
                "control state read failed; skipping dispatch: {error}"
            ));
            write_status(ctx, st, &merger.lane_lines(ctx.now()));
            return Err(error);
        }
    };
    if control.mode == crate::pump::control::PumpMode::Paused {
        board::board(ctx, st);
        plan::ingest(ctx, st);
        crate::pump::alarm::check(ctx, st);
        save(ctx, st)?;
        write_status(ctx, st, &merger.lane_lines(ctx.now()));
        return Ok(());
    }
    dispatch::dispatch(ctx, st);
    save(ctx, st)?;
    for target in ctx.cfg.target_configs().keys() {
        let Some(target_ctx) = ctx.for_target(target) else {
            ctx.exception(
                "unknown-target",
                target,
                "configured repository target could not be resolved",
            );
            continue;
        };
        crate::pump::state::with_target_state(st, target, |st| {
            gate::hold_cycle(&target_ctx, st);
            gate::external_gates(&target_ctx, st);
            gate::start_gate(&target_ctx, st);
        });
    }
    merger.apply(ctx, st);
    merger.feed(ctx, st);
    board::board(ctx, st);
    plan::ingest(ctx, st);
    crate::pump::alarm::check(ctx, st);
    save(ctx, st)?;
    write_status(ctx, st, &merger.lane_lines(ctx.now()));
    Ok(())
}

/// Write the status lines to `<home>/status.txt`: a UTC timestamp, unit
/// counts, plan verdict counts, the merge/gate counters, the stall lines
/// from [`crate::pump::alarm::status_lines`], the in-flight planner lines,
/// then one line per merge lane (`lanes`, from [`Merger::lane_lines`]). An
/// idle board adds no planner lines. A write failure is logged, never fatal.
pub fn write_status(ctx: &Ctx, st: &State, lanes: &[String]) {
    let mut verdicts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for need in st.needs.values() {
        *verdicts.entry(need.verdict.clone()).or_default() += 1;
    }
    let baseline_count = st.baseline.as_ref().map_or(0, Vec::len)
        + st.target_states
            .values()
            .filter_map(|target| target.baseline.as_ref())
            .map(Vec::len)
            .sum::<usize>();
    let baseline = if baseline_count == 0 {
        String::from("none")
    } else {
        baseline_count.to_string()
    };
    let gates = st.gates.len()
        + st.target_states
            .values()
            .map(|target| target.gates.len())
            .sum::<usize>();
    let mut lines = vec![
        utc_stamp(ctx.now()),
        counts_line("units", &st.counts()),
        counts_line("plans", &verdicts),
        format!(
            "merges {}  needs-ready {}  gates {}  baseline {}",
            st.merges,
            st.closed_needs.len(),
            gates,
            baseline
        ),
    ];
    lines.extend(crate::pump::alarm::status_lines(ctx, st));
    lines.extend(planner_lines(ctx, st));
    lines.extend(lanes.iter().cloned());
    let path = ctx.home.join("status.txt");
    if let Err(err) = fs::write(&path, lines.join("\n") + "\n") {
        ctx.log(&format!("status write failed at {}: {err}", path.display()));
    }
}

/// The lines `status.txt` carries for the planner board: one line per
/// in-flight planner with its pid and age, the consecutive-failure counts,
/// and the parked handles. Empty when the board has nothing to report.
fn planner_lines(ctx: &Ctx, st: &State) -> Vec<String> {
    let now = ctx.now();
    let mut lines: Vec<String> = st
        .board
        .planning
        .iter()
        .map(|(handle, at)| {
            let pid = st
                .board
                .planner_pids
                .get(handle)
                .map_or_else(|| String::from("-"), u32::to_string);
            let age = now.saturating_sub(*at);
            format!("planner {handle} pid {pid} age {age}s")
        })
        .collect();
    if !st.board.planner_fails.is_empty() {
        let counts: Vec<String> = st
            .board
            .planner_fails
            .iter()
            .map(|(handle, count)| format!("{handle}={count}"))
            .collect();
        lines.push(format!("planner-failures {}", counts.join(" ")));
    }
    if !st.board.planner_failed.is_empty() {
        lines.push(format!(
            "planner-parked {}",
            st.board.planner_failed.join(" ")
        ));
    }
    lines
}

/// Run the pump over `project`: load the mailspace's `[pump]` table, take
/// the exclusive home lock, and loop [`tick`] until `once` or durable control
/// state stops it.
///
/// # Errors
/// Returns [`VivariumError::Config`] when the mailspace has no `[pump]`
/// table or the table fails `validate`, an error when the lock is already
/// held, and whatever loading the state fails with.
pub fn run(project: &Path, once: bool) -> Result<(), VivariumError> {
    run_inner(project, once, false)
}

/// Run as a service-owned child and announce readiness after taking the pump lock.
pub fn run_service_child(project: &Path) -> Result<(), VivariumError> {
    run_inner(project, false, true)
}

fn run_inner(project: &Path, once: bool, announce_ready: bool) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(Some(project))?;
    let cfg = pump_config(&mailspace)?;
    let pools: Vec<String> = mailspace.config.pools.keys().cloned().collect();
    let role_pool = mailspace
        .config
        .identities
        .iter()
        .any(|role| role.pool.is_some());
    cfg.validate_for(project, &pools, role_pool)?;
    let issues = crate::routing::validate(&mailspace.config);
    for issue in &issues {
        eprintln!(
            "vivi: {}: {}",
            match issue.severity {
                crate::routing::Severity::Warning => "warning",
                crate::routing::Severity::Error => "error",
            },
            issue.message
        );
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
    let host = Arc::new(RealHost::new(&home));
    let vivi = std::env::current_exe()
        .map_or_else(|_| String::from("vivi"), |exe| exe.display().to_string());
    let ctx = Ctx::new(cfg, host, project.to_path_buf(), vivi)
        .with_routing(crate::routing::from_mailspace(&mailspace.config));
    crate::pump::priority::import_legacy(&ctx);
    if announce_ready {
        println!("VIVI_PUMP_SERVICE_READY");
        std::io::stdout().flush()?;
    }
    if control.mode == crate::pump::control::PumpMode::Stopped {
        ctx.log("pump is stopped; run vivi pump start to enable it");
        return Ok(());
    }
    run_with_locked(&ctx, once)?;
    drop(lock);
    Ok(())
}

/// The pump's status text plus its durable control mode. No pump lock is taken.
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
    Ok(format!("{text}control  {}\n", control.mode))
}

/// Load the state, start the merge thread, and drive the tick loop over an
/// already-built context (the testable half of [`run`]).
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
    // The previous controller's in-memory merge queue no longer exists.
    // Repair both persisted formats before the new worker can receive jobs.
    st.recover_after_restart();
    plan::ingest(ctx, &mut st);
    collect::recover_pushed_pending(ctx, &mut st)?;
    store.save(&st)?;
    let merger = Merger::start(ctx);
    drive(ctx, &mut st, &merger, once)
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

/// The run/once loop: tick, then return when control state is stopped,
/// save and return, else sleep `tick` seconds. A panic
/// inside a tick is caught, logged as `TICK ERROR`, and the state saved.
fn drive(ctx: &Ctx, st: &mut State, merger: &Merger, once: bool) -> Result<(), VivariumError> {
    loop {
        if exit_requested(ctx) {
            save(ctx, st)?;
            return Ok(());
        }
        let tick_once = AssertUnwindSafe(|| tick(ctx, st, merger));
        match catch_unwind(tick_once) {
            Ok(result) => result?,
            Err(err) => {
                ctx.log(&format!("TICK ERROR {}", panic_note(&err)));
                save(ctx, st)?;
            }
        }
        if once {
            return Ok(());
        }
        if exit_requested(ctx) {
            ctx.log("pump control state requests exit");
            save(ctx, st)?;
            return Ok(());
        }
        thread::sleep(Duration::from_secs(ctx.cfg.tick));
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
    let snapshot = match crate::pump::report::read(&home)? {
        crate::pump::report::PumpRead::Ready(snapshot) => snapshot,
        crate::pump::report::PumpRead::Missing => {
            return Err(VivariumError::Message(format!(
                "no pump.db at {}",
                home.join(DB_FILENAME).display()
            )));
        }
        crate::pump::report::PumpRead::Unreadable(why) => {
            return Err(VivariumError::Message(format!("pump.db unreadable: {why}")));
        }
    };
    let rows = crate::pump::closeout::complete_open(&mailspace, &snapshot)?;
    if let Some(handle) = close {
        let row = rows
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
fn utc_stamp(secs: u64) -> String {
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
