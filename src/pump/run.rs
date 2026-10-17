//! Pump tick, status, and the run/once entry points.

use std::fs::{self, File};
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

/// One pump tick: board, ingest, collect, the merge
/// feed, and — unless the `PAUSE` flag exists — dispatch, the hold cycle,
/// the gate start, and a second merge pass, saving at three save
/// points and ending in [`write_status`].
pub fn tick(ctx: &Ctx, st: &mut State, merger: &Merger) {
    merger.apply(ctx, st);
    board::board(ctx, st);
    plan::ingest(ctx, st);
    let gate_receipt = collect::collect(ctx, st);
    if let Some(text) = gate_receipt {
        gate::finish_gate(ctx, st, &text);
    }
    merger.apply(ctx, st);
    merger.feed(ctx, st);
    save(ctx, st);
    if ctx.flag("PAUSE") {
        write_status(ctx, st);
        return;
    }
    dispatch::dispatch(ctx, st);
    save(ctx, st);
    gate::hold_cycle(ctx, st);
    gate::external_gates(ctx, st);
    gate::start_gate(ctx, st);
    merger.apply(ctx, st);
    merger.feed(ctx, st);
    save(ctx, st);
    write_status(ctx, st);
}

/// Write the four status lines to `<home>/status.txt`:
/// a UTC timestamp, unit counts, plan verdict counts, and the merge/gate
/// counters. A write failure is logged, never fatal.
pub fn write_status(ctx: &Ctx, st: &State) {
    let mut verdicts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for need in st.needs.values() {
        *verdicts.entry(need.verdict.clone()).or_default() += 1;
    }
    let baseline = st
        .baseline
        .as_ref()
        .map_or_else(|| String::from("none"), |names| names.len().to_string());
    let lines = [
        utc_stamp(ctx.now()),
        counts_line("units", &st.counts()),
        counts_line("plans", &verdicts),
        format!(
            "merges {}  needs-ready {}  gates {}  baseline {}",
            st.merges,
            st.closed_needs.len(),
            st.gates.len(),
            baseline
        ),
    ];
    let path = ctx.home.join("status.txt");
    if let Err(err) = fs::write(&path, lines.join("\n") + "\n") {
        ctx.log(&format!("status write failed at {}: {err}", path.display()));
    }
}

/// Run the pump over `project`: load the mailspace's `[pump]` table, take
/// the exclusive home lock, and loop [`tick`] until `once` or the `STOP`
/// flag ends it.
///
/// # Errors
/// Returns [`VivariumError::Config`] when the mailspace has no `[pump]`
/// table or the table fails `validate`, an error when the lock is already
/// held, and whatever loading the state fails with.
pub fn run(project: &Path, once: bool) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(Some(project))?;
    let cfg = pump_config(&mailspace)?;
    let pools: Vec<String> = mailspace.config.pools.keys().cloned().collect();
    cfg.validate(project, &pools)?;
    let home = cfg.home_dir(project);
    fs::create_dir_all(&home)?;
    let lock = take_lock(&home)?;
    let host = Arc::new(RealHost::new(&home));
    let vivi = std::env::current_exe()
        .map_or_else(|_| String::from("vivi"), |exe| exe.display().to_string());
    let ctx = Ctx::new(cfg, host, project.to_path_buf(), vivi);
    run_with(&ctx, once)?;
    drop(lock);
    Ok(())
}

/// The pump's status text: `<home>/status.txt`, or `no status yet` when
/// absent. A peek only: no lock, no state load.
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
    let path = cfg.home_dir(project).join("status.txt");
    match fs::read_to_string(&path) {
        Ok(text) => Ok(text),
        Err(_) => Ok(String::from("no status yet")),
    }
}

/// Load the state, start the merge thread, and drive the tick loop over an
/// already-built context (the testable half of [`run`]).
///
/// # Errors
/// Returns whatever loading the state fails with.
pub(crate) fn run_with(ctx: &Ctx, once: bool) -> Result<(), VivariumError> {
    ensure_repo(ctx)?;
    let mut store = PumpStore::open(&ctx.home.join(DB_FILENAME))?;
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
    store.save(&st)?;
    let merger = Merger::start(ctx);
    drive(ctx, &mut st, &merger, once);
    Ok(())
}

/// Startup check: the integration clone must exist. A missing `repo` is
/// cloned from `remote_url` (the clone's remote is named `remote`, on the
/// integration branch); with no `remote_url` the pump refuses to start.
///
/// # Errors
/// Returns [`VivariumError::Config`] when the clone is missing and no
/// `remote_url` is set, and [`VivariumError::Message`] when the clone fails.
pub(crate) fn ensure_repo(ctx: &Ctx) -> Result<(), VivariumError> {
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

/// The run/once loop: tick, then return when `once`,
/// save and return on the `STOP` flag, else sleep `tick` seconds. A panic
/// inside a tick is caught, logged as `TICK ERROR`, and the state saved.
fn drive(ctx: &Ctx, st: &mut State, merger: &Merger, once: bool) {
    loop {
        let tick_once = AssertUnwindSafe(|| tick(ctx, st, merger));
        if let Err(err) = catch_unwind(tick_once) {
            ctx.log(&format!("TICK ERROR {}", panic_note(&err)));
            save(ctx, st);
        }
        if once {
            return;
        }
        if ctx.flag("STOP") {
            ctx.log("STOP file seen; exiting");
            save(ctx, st);
            return;
        }
        thread::sleep(Duration::from_secs(ctx.cfg.tick));
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

/// Save the state to `<home>/pump.db`; a failure is logged, never fatal.
fn save(ctx: &Ctx, st: &State) {
    let path = ctx.home.join(DB_FILENAME);
    if let Err(err) = PumpStore::open(&path).and_then(|mut store| store.save(st)) {
        ctx.log(&format!("state save failed at {}: {err}", path.display()));
    }
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
