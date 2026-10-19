//! Gate start, finish, audit handoff, the hold cycle and promotion.
//!
//! The gate: `start_gate`, `finish_gate`, the auditor handoff, `promote` and
//! the hold cycle.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::pump::brief::gate_brief;
use crate::pump::gatefix::{replans_left, residual_failures};
use crate::pump::host::Ctx;
use crate::pump::jobs::{JobSpec, enqueue};
use crate::pump::state::{GateRecord, GateRun, State, Status, Unit};

pub(crate) mod green;
mod jobs;
mod parse;

pub use jobs::collect_completed;
use parse::{BEGIN, BUILD_FAILED, END, last_usable_block};
pub use parse::{env_failed, parse_failing, target_deaths};

/// Flag file that makes the next tick start a gate whatever the due rules;
/// the default target uses this name, other targets suffix `-<target>`.
const FORCE_FLAG: &str = "FORCE-GATE";
/// The `state.json` key remembering the tip whose failing set was last
/// reported unknown, so the line is logged once per tip.
const UNKNOWN_KEY: &str = "promote_unknown_logged";
/// Flag file that holds the merge queue until a clean gate.
const HOLD_FLAG: &str = "HOLD-MERGE";
/// The pool name an external gate run is recorded under.
const EXTERNAL_POOL: &str = "external";
/// The `state.json` key holding the epoch seconds a due gate began waiting
/// for an external result.
const WAIT_KEY: &str = "external_wait_since";
/// The `state.json` key holding the epoch seconds the last external result was
/// consumed; `results_idle_seconds` measures the gap from it.
const RESULT_KEY: &str = "last_external_result_at";
/// The file an external gate runner touches in `results_dir` to prove it is
/// alive; its age decides whether a due gate waits at all.
const HEARTBEAT: &str = "heartbeat";
/// The payload the hold cycle writes into `FORCE-GATE`.
const FORCE_TEXT: &str = "after fixes\n";
/// The payload that marks an automatic `HOLD-MERGE` owner.
const HOLD_AUTO_TEXT: &str = "gate red\n";
/// The automatic hold marker written by older pump versions.
const HOLD_LEGACY_AUTO_TEXT: &str = "red\n";
/// The characters `ctx.exception` keeps of a text.
const EXCEPTION_LIMIT: usize = 600;

/// Take the gate results an external runner wrote, oldest
/// first, as ordinary gates.
///
/// Does nothing unless `[pump.gate] results_dir` is set. Each `<tip>.json` in
/// it is moved to `done/` and finished like a worker's reply: `last_gate`
/// becomes an `external` run at that tip, the wait marker is cleared, and the
/// names (or a `BUILD-FAILED` line plus the tail) go through [`finish_gate`].
/// A file that does not parse is left where it is. A result with neither
/// names nor `build_failed` is finished with an empty reply, which raises
/// `gate-unreadable`.
pub fn external_gates(ctx: &Ctx, st: &mut State) {
    if st.has_active_gate() {
        return;
    }
    let Some(dir) = ctx.cfg.results_dir(&ctx.project) else {
        return;
    };
    for (path, result) in pending_results(ctx, &dir) {
        let done = dir.join("done");
        let moved = fs::create_dir_all(&done)
            .and_then(|()| fs::rename(&path, done.join(path.file_name().unwrap_or_default())));
        if let Err(err) = moved {
            ctx.log(&format!(
                "external gate result move failed at {}: {err}",
                path.display()
            ));
            continue;
        }
        let short = tip_short(&result.tip);
        if st.gates.iter().any(|gate| gate.tip == result.tip) {
            ctx.log(&format!(
                "external gate {short} already has a collected receipt; duplicate ignored"
            ));
            continue;
        }
        st.last_gate = GateRun {
            at: ctx.now(),
            merges: st.merges,
            task: Some(String::from(EXTERNAL_POOL)),
            job_id: None,
            source_jobs: Vec::new(),
            tip: Some(result.tip.clone()),
            pool: Some(String::from(EXTERNAL_POOL)),
        };
        st.extra.remove(WAIT_KEY);
        st.extra
            .insert(String::from(RESULT_KEY), serde_json::json!(ctx.now()));
        let host = result
            .host
            .as_deref()
            .map_or_else(String::new, |host| format!(" host={host}"));
        ctx.log(&format!(
            "external gate at {short} ({}s){host}",
            result.seconds
        ));
        finish_gate(ctx, st, &result.reply());
    }
}

/// A gate result file an external runner writes.
#[derive(Debug, serde::Deserialize)]
struct ExternalResult {
    /// The integration tip that was gated.
    tip: String,
    /// Wall seconds the run took.
    #[serde(default)]
    seconds: u64,
    /// Free text naming the machine that ran it; only logged.
    #[serde(default)]
    host: Option<String>,
    /// Failing test names; null when the build failed.
    #[serde(default)]
    names: Option<Vec<String>>,
    /// The build itself broke.
    #[serde(default)]
    build_failed: bool,
    /// Output tail kept for a failed build.
    #[serde(default)]
    tail: String,
}

impl ExternalResult {
    /// The result as the reply text [`finish_gate`] reads.
    fn reply(&self) -> String {
        if self.build_failed {
            return format!("{BEGIN}\n{BUILD_FAILED}\n{END}\n{}", self.tail);
        }
        match &self.names {
            Some(names) => format!("{BEGIN}\n{}\n{END}", names.join("\n")),
            None => String::new(),
        }
    }
}

/// Parsed `*.json` results in `dir`, oldest modification time first. A file
/// that cannot be read or parsed is logged once and moved to `<dir>/bad/`, so
/// an operator sees it and it is not re-read every tick.
fn pending_results(ctx: &Ctx, dir: &Path) -> Vec<(PathBuf, ExternalResult)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<(u64, PathBuf, ExternalResult)> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json") && path.is_file())
        .filter_map(|path| {
            let parsed = fs::read_to_string(&path)
                .map_err(|err| err.to_string())
                .and_then(|text| serde_json::from_str(&text).map_err(|err| err.to_string()));
            match parsed {
                Ok(result) => Some((file_mtime(&path), path, result)),
                Err(err) => {
                    set_aside(ctx, dir, &path, &err);
                    None
                }
            }
        })
        .collect();
    found.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    found
        .into_iter()
        .map(|(_, path, result)| (path, result))
        .collect()
}

/// Moves an unusable external result to `<dir>/bad/` and logs why.
fn set_aside(ctx: &Ctx, dir: &Path, path: &Path, why: &str) {
    let bad = dir.join("bad");
    let moved = fs::create_dir_all(&bad)
        .and_then(|()| fs::rename(path, bad.join(path.file_name().unwrap_or_default())));
    match moved {
        Ok(()) => ctx.log(&format!(
            "external gate result {} unreadable ({why}); moved to bad/",
            path.display()
        )),
        Err(err) => ctx.log(&format!(
            "external gate result {} unreadable ({why}); move to bad/ failed: {err}",
            path.display()
        )),
    }
}

/// `true` while a due gate should keep waiting for an external result:
/// `results_dir` is set, it has not been idle for `results_idle_seconds`, its
/// `heartbeat` file is younger than twice `results_wait_seconds`, and the wait,
/// started the first time a gate was due, is younger than
/// `results_wait_seconds`. Starts the wait when it is not running yet; an
/// elapsed wait is cleared so the pool gate goes out. A missing or stale
/// heartbeat, or an idle runner, means no external runner is alive: the marker
/// is cleared and the pool gate goes out at once, because a dead runner must
/// never delay a gate. `results_idle_seconds` (when > 0) counts from the last
/// consumed result; with no result consumed yet the clock starts at the epoch,
/// so an enabled idle rule skips the wait until the first result arrives.
fn waiting_for_external(ctx: &Ctx, st: &mut State) -> bool {
    let Some(dir) = ctx.cfg.results_dir(&ctx.project) else {
        return false;
    };
    let now = ctx.now();
    if idle_external(ctx, st, now) {
        st.extra.remove(WAIT_KEY);
        ctx.log(&format!(
            "external gate runner idle over {}s; not waiting for results_dir",
            ctx.cfg.gate.results_idle_seconds
        ));
        return false;
    }
    if let Some(reason) = stalled_runner(ctx, &dir, now) {
        st.extra.remove(WAIT_KEY);
        ctx.log(&format!(
            "external gate runner heartbeat {reason}; not waiting for results_dir"
        ));
        return false;
    }
    let since = st
        .extra
        .get(WAIT_KEY)
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(now);
    if now.saturating_sub(since) < ctx.cfg.gate.results_wait_seconds {
        st.extra
            .insert(String::from(WAIT_KEY), serde_json::json!(since));
        return true;
    }
    st.extra.remove(WAIT_KEY);
    false
}

/// `true` when `results_idle_seconds` is on and no external result has been
/// consumed for longer than that: the runner is not running, so the wait must
/// not hold the gate. Off (`0`) is always `false`.
fn idle_external(ctx: &Ctx, st: &State, now: u64) -> bool {
    let idle = ctx.cfg.gate.results_idle_seconds;
    if idle == 0 {
        return false;
    }
    let last = st
        .extra
        .get(RESULT_KEY)
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    now.saturating_sub(last) > idle
}

/// Why `results_dir` holds no proof of a live external runner at `now`:
/// `Some("missing")` when there is no `heartbeat` file, `Some("stale")` when
/// its modification time is older than twice `results_wait_seconds`.
fn stalled_runner(ctx: &Ctx, dir: &Path, now: u64) -> Option<&'static str> {
    let path = dir.join(HEARTBEAT);
    if !path.is_file() {
        return Some("missing");
    }
    let window = ctx.cfg.gate.results_wait_seconds.saturating_mul(2);
    if now.saturating_sub(file_mtime(&path)) > window {
        return Some("stale");
    }
    None
}

/// Start a gate when one is due by enqueuing a durable read-only job. Does
/// nothing while a gate task is open, when no pool is free, when the remote
/// tip is not a 40-hex commit, or while a due gate is still inside its
/// `results_wait_seconds` wait for an external result (a pool gate is only
/// the fallback). The wait only happens while `results_dir` holds a fresh
/// `heartbeat` file; a missing or stale one dispatches the pool gate at once.
/// The generic dispatcher owns task creation, pool selection and execution;
/// this function stores its job ID in `last_gate` and returns.
pub fn start_gate(ctx: &Ctx, st: &mut State) {
    if st.has_active_gate() {
        return;
    }
    force_after_fix(ctx, st);
    let force = ctx.flag(&force_flag(ctx));
    if !gate_due(st, force) || waiting_for_external(ctx, st) {
        return;
    }
    let Some(tip) = remote_tip(ctx) else {
        return;
    };
    let Some(path) = write_gate_brief(ctx, &tip) else {
        return;
    };
    let Ok(brief) = fs::read_to_string(&path) else {
        ctx.log(&format!("gate brief read failed: {}", path.display()));
        return;
    };
    enqueue_gate(ctx, st, tip, brief, force);
}

/// Queue the read-only gate job and retain its tip and merge sources.
fn enqueue_gate(ctx: &Ctx, st: &mut State, tip: String, brief: String, force: bool) {
    let role = ctx.routing.gate();
    let fallback = role.is_none();
    let recipient = role.map_or_else(|| ctx.cfg.to.clone(), |role| role.name.clone());
    let job_id = gate_job_id(ctx, st, &tip);
    let source_jobs = crate::pump::jobs::sources_waiting_for_gate(st, &ctx.target_id);
    let subject = format!(
        "[pump gate] {} at {}",
        ctx.cfg.gate.command,
        tip_short(&tip)
    );
    let mut spec = JobSpec::new(job_id.clone(), "gate", recipient, subject, brief);
    spec.pool_candidates = ctx.cfg.gate.pools.clone();
    spec.target = Some(ctx.target_id.clone());
    spec.from_ref = Some(tip.clone());
    spec.budget = ctx.cfg.gate.budget;
    spec.read_only = true;
    spec.model = fallback.then(|| ctx.cfg.gate.model.clone());
    spec.source_jobs.clone_from(&source_jobs);
    spec.payload = serde_json::json!({
        "tip": tip,
        "merges": st.merges,
        "command": ctx.cfg.gate.command,
        "source_jobs": source_jobs,
    });
    if let Err(err) = enqueue(st, spec) {
        ctx.log(&format!("gate enqueue failed: {err}"));
        return;
    }
    if force {
        remove_flag(ctx, &force_flag(ctx));
    }
    st.last_gate = GateRun {
        at: ctx.now(),
        merges: st.merges,
        task: None,
        job_id: Some(job_id.clone()),
        source_jobs,
        tip: Some(tip.clone()),
        pool: None,
    };
    let short = tip_short(&tip);
    ctx.log(&format!("gate queued at {short} job={job_id}"));
}

/// A stable job identity for a gate start at this target/tip/sequence.
fn gate_job_id(ctx: &Ctx, st: &State, tip: &str) -> String {
    format!(
        "gate-{}-{}-{}",
        safe_job_id(&ctx.target_id),
        safe_job_id(&tip[..tip.len().min(12)]),
        st.gates.len().saturating_add(1)
    )
}

/// Keep only characters accepted by the durable job ID validator.
fn safe_job_id(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

/// Force the next gate when a fix unit of this target merged after the last
/// gate started: a merged fix is the path back to green and gates on the next
/// tick. Writes `FORCE-GATE` once per merged fix (starting the gate moves
/// `last_gate.at` past the merge); does nothing while a gate runs or the flag
/// is already set.
fn force_after_fix(ctx: &Ctx, st: &State) {
    if st.has_active_gate() || ctx.flag(&force_flag(ctx)) {
        return;
    }
    let prefix = ctx.cfg.fix_prefix.as_str();
    let merged_at = |unit: &Unit| unit.merged_at.unwrap_or(unit.since);
    let newest = st
        .units
        .iter()
        .filter(|(uid, unit)| {
            uid.starts_with(prefix)
                && unit.status == Status::Merged
                && unit.target_id() == ctx.target_id
                && merged_at(unit) > st.last_gate.at
        })
        .max_by_key(|(_, unit)| merged_at(unit));
    if let Some((uid, _)) = newest {
        write_force_flag(ctx);
        ctx.log(&format!("gate forced: fix {uid} merged"));
    }
}

/// The due rules: a `FORCE-GATE` flag, no baseline yet, or any merge that
/// landed since the last gate started. The `every_merges` / `every_seconds`
/// thresholds are retired.
fn gate_due(st: &State, force: bool) -> bool {
    force || st.baseline.is_none() || st.merges > st.last_gate.merges
}

/// The remote integration tip: first token of
/// `git ls-remote <remote> refs/heads/<branch>`, when it is 40 hex characters.
fn remote_tip(ctx: &Ctx) -> Option<String> {
    let out = ctx.git(
        &format!("ls-remote {} refs/heads/{}", ctx.cfg.remote, ctx.cfg.branch),
        60,
    );
    let tip = out.text.split_whitespace().next().unwrap_or_default();
    if tip.len() == 40 && tip.bytes().all(|b| b.is_ascii_hexdigit()) {
        Some(tip.to_owned())
    } else {
        None
    }
}

/// Write the rendered gate brief to `<home>/briefs/gate.txt`.
fn write_gate_brief(ctx: &Ctx, tip: &str) -> Option<PathBuf> {
    let path = ctx.home.join("briefs").join("gate.txt");
    if let Some(dir) = path.parent()
        && let Err(err) = fs::create_dir_all(dir)
    {
        ctx.log(&format!("gate brief dir failed: {err}"));
        return None;
    }
    if let Err(err) = fs::write(&path, gate_brief(ctx, tip)) {
        ctx.log(&format!("gate brief write failed: {err}"));
        return None;
    }
    Some(path)
}

/// The forced-gate flag file name for this context's target: `FORCE-GATE`
/// for `default`, `FORCE-GATE-<target>` otherwise.
fn force_flag(ctx: &Ctx) -> String {
    if ctx.target_id == "default" {
        FORCE_FLAG.to_owned()
    } else {
        format!("{FORCE_FLAG}-{}", ctx.target_id)
    }
}

/// The first ten characters of a tip, for log and subject lines.
fn tip_short(tip: &str) -> String {
    tip.chars().take(10).collect()
}

/// Finish the open gate from its receipt: classify the reply,
/// keep the owned-red map honest, then set the baseline, audit red gates, or
/// promote a clean gate. The gate task is cleared
/// whatever the reply said.
pub fn finish_gate(ctx: &Ctx, st: &mut State, text: &str) {
    let names = parse_failing(text);
    let deaths = target_deaths(text);
    log_markerless(ctx, text, names.as_deref());
    record_red_after_fix(ctx, st, names.as_deref());
    let run = st.last_gate.clone();
    let tip = run.tip.clone().unwrap_or_default();
    let env = env_failed(text);
    if !env.is_empty() {
        ctx.exception(
            "gate-environment",
            &tip_short(&tip),
            &environment_text(&env),
        );
    }
    let mut rec = GateRecord {
        tip: tip.clone(),
        at: ctx.now(),
        merges: run.merges,
        names: names.clone(),
        new: None,
    };
    st.last_gate.task = None;
    if let Some(kind) = unreadable_kind(names.as_deref()) {
        ctx.exception(kind, &tip_short(&tip), &tail_chars(text, 500));
        write_force_flag(ctx);
        ctx.log("gate retry forced after unreadable receipt");
        st.gates.push(rec);
        return;
    }
    let ignored = ignored_names(ctx);
    let names: Vec<String> = names
        .unwrap_or_default()
        .into_iter()
        .filter(|name| !ignored.contains(name))
        .collect();
    rec.names = Some(names.clone());
    let build_failed = names.as_slice() == [BUILD_FAILED];
    if build_failed {
        ctx.exception(
            "gate-build-failed",
            &tip_short(&tip),
            &tail_chars(text, 500),
        );
    }
    if !establish_baseline(ctx, st, &tip, &names, build_failed) {
        st.gates.push(rec);
        return;
    }
    finish_known_gate(ctx, st, &tail_chars(text, 4000), &mut rec, &names, &deaths);
    st.gates.push(rec);
}

/// Establish the first readable baseline; a build failure starts from empty
/// so it is sent through the auditor and remains held.
fn establish_baseline(
    ctx: &Ctx,
    st: &mut State,
    tip: &str,
    names: &[String],
    build_failed: bool,
) -> bool {
    if st.baseline.is_some() {
        return true;
    }
    if build_failed {
        st.baseline = Some(Vec::new());
        ctx.log(&format!(
            "gate build failed at {}; auditor will establish a passing baseline",
            tip_short(tip)
        ));
        return true;
    }
    st.baseline = Some(names.to_vec());
    ctx.log(&format!(
        "gate baseline at {}: {} failing names",
        tip_short(tip),
        names.len()
    ));
    false
}

/// One line naming the names that failed only on missing tools: they are
/// already out of the failing set, so they are never owned, never new, and no
/// audit runs for them.
fn environment_text(env: &[(String, String)]) -> String {
    let listed = env
        .iter()
        .map(|(name, evidence)| format!("{name} ({evidence})"))
        .collect::<Vec<_>>()
        .join("; ");
    cut_chars(
        &format!(
            "{} names failed on missing tools, not owned: {listed}",
            env.len()
        ),
        EXCEPTION_LIMIT,
    )
}

/// `Some("gate-unreadable")` when the reply contains no failing-name list.
fn unreadable_kind(names: Option<&[String]>) -> Option<&'static str> {
    match names {
        None => Some("gate-unreadable"),
        Some(_) => None,
    }
}

/// The log line python emits when the names came from the reply text rather
/// than the markers.
fn log_markerless(ctx: &Ctx, text: &str, names: Option<&[String]>) {
    if let Some(names) = names
        && last_usable_block(text).is_none()
        && !names.is_empty()
    {
        ctx.log(&format!(
            "gate markers missing; read {} names from the reply text",
            names.len()
        ));
    }
}

/// The `red-after-fix` screen: an owned name that still
/// fails while every unit of its owner need is settled is reported once.
fn record_red_after_fix(ctx: &Ctx, st: &mut State, names: Option<&[String]>) {
    let Some(names) = names else {
        return;
    };
    let owned: Vec<(String, String)> = st
        .red
        .iter()
        .map(|(name, owner)| (name.clone(), owner.clone()))
        .collect();
    for (name, owner) in owned {
        let statuses: Vec<Status> = st
            .units
            .values()
            .filter(|unit| unit.need == owner)
            .map(|unit| unit.status)
            .collect();
        let mark = format!("{owner}:{name}");
        let failing = names.iter().any(|n| n == &name);
        if statuses.is_empty()
            || !failing
            || !statuses.iter().all(|&s| is_settled(s))
            || st.red_reported.contains(&mark)
            || replans_left(ctx, st, &name)
        {
            continue;
        }
        st.red_reported.push(mark);
        let attempts = st
            .extra
            .get("replans")
            .and_then(|counts| counts.get(&name))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        ctx.exception(
            "red-after-fix",
            &owner,
            &format!(
                "{name}; audits for this name: {attempts}; last report: {}/audit/{owner}.report",
                ctx.home.display()
            ),
        );
    }
}

/// A readable gate against an existing baseline: release
/// owned names that pass again, compute the new failures and the residual
/// ones (owned names whose owner settled but that still fail), then hand reds
/// to the auditor or promote a clean gate.
fn finish_known_gate(
    ctx: &Ctx,
    st: &mut State,
    output_tail: &str,
    rec: &mut GateRecord,
    names: &[String],
    deaths: &[String],
) {
    drop_fixed_names(ctx, st, names, deaths, &rec.tip);
    let new = new_failures(st, names);
    rec.new = Some(new.clone());
    let residual = residual_failures(ctx, st, names);
    if !new.is_empty() || !residual.is_empty() {
        let prior = crate::pump::audit::prior_reports(ctx, &residual);
        crate::pump::audit::start_audit(
            ctx,
            st,
            &rec.tip,
            output_tail,
            &new,
            &residual,
            prior.as_deref(),
        );
        write_hold_flag(ctx, &rec.tip);
    }
    if new.is_empty() {
        let source_jobs = st.last_gate.source_jobs.clone();
        crate::pump::jobs::mark_gated(st, &source_jobs, &rec.tip);
        ctx.log(&gate_summary(rec, names, 0));
        note_unknown_main_set(ctx, st);
        let off_main = not_on_main(st, names);
        if off_main.is_empty() {
            jobs::promote(ctx, st, &rec.tip, names);
        } else {
            green::promote_last_green(ctx, st, &rec.tip, names);
            ctx.log(&held_message(&off_main, &rec.tip));
        }
        return;
    }
    report_new_failures(ctx, rec, &new);
    green::promote_last_green(ctx, st, &rec.tip, names);
    ctx.log(&gate_summary(rec, names, new.len()));
}

/// Log once per tip when the newest promotion has no recorded failing set
/// (a legacy promotion), so main's set falls back to the baseline visibly.
fn note_unknown_main_set(ctx: &Ctx, st: &mut State) {
    let Some(promotion) = st.promotions.last() else {
        return;
    };
    let tip = promotion.tip.clone();
    let logged = st.extra.get(UNKNOWN_KEY).and_then(|v| v.as_str());
    if green::promoted_set(&st.promotions, &st.gates).is_some() || logged == Some(tip.as_str()) {
        return;
    }
    ctx.log(&format!(
        "promote: main's failing set unknown at {}; using baseline",
        tip_short(&tip)
    ));
    st.extra
        .insert(UNKNOWN_KEY.to_owned(), serde_json::Value::String(tip));
}

/// The gate's failing names that main does not carry, sorted. Main is
/// promoted only when there are none: main never gets redder.
fn not_on_main(st: &State, names: &[String]) -> Vec<String> {
    let main = green::main_set(&st.promotions, &st.gates, st.baseline.as_deref());
    names
        .iter()
        .filter(|name| !main.contains(*name))
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// The `promote held` log line, naming up to five of the offending names.
fn held_message(off_main: &[String], tip: &str) -> String {
    let shown = off_main.iter().take(5).cloned().collect::<Vec<_>>();
    let more = off_main.len().saturating_sub(shown.len());
    let tail = if more > 0 {
        format!(" and {more} more")
    } else {
        String::new()
    };
    format!(
        "promote held: {} failures not on main at {} (main never gets redder): {}{tail}",
        off_main.len(),
        tip_short(tip),
        shown.join("; ")
    )
}

/// The `gate at <tip10>: <n> failing, <m> new` log line.
fn gate_summary(rec: &GateRecord, names: &[String], new: usize) -> String {
    format!(
        "gate at {}: {} failing, {new} new",
        tip_short(&rec.tip),
        names.len()
    )
}

/// Owned names that no longer fail are released. A name whose crate target
/// died without a libtest summary is not fixed: it stays owned, and the gate
/// reports the death instead.
fn drop_fixed_names(ctx: &Ctx, st: &mut State, names: &[String], deaths: &[String], tip: &str) {
    let (kept, fixed): (Vec<String>, Vec<String>) = st
        .red
        .keys()
        .filter(|name| !names.iter().any(|n| n == *name))
        .cloned()
        .partition(|name| target_died(name, deaths));
    if !kept.is_empty() {
        let targets = deaths.join(", ");
        ctx.log(&format!(
            "gate-target-died: {targets} kept {} owned reds (target died, not fixed)",
            kept.len()
        ));
        ctx.exception(
            "gate-target-died",
            &tip_short(tip),
            &format!(
                "{targets} died without a libtest summary; {} owned names stay red",
                kept.len()
            ),
        );
    }
    for name in fixed {
        if let Some(owner) = st.red.remove(&name) {
            ctx.log(&format!("red fixed: {name} (owner {owner})"));
        }
    }
}

/// `true` when the name's first whitespace token, its crate, is one of the
/// targets that died without a libtest summary.
fn target_died(name: &str, deaths: &[String]) -> bool {
    name.split_whitespace()
        .next()
        .is_some_and(|krate| deaths.iter().any(|death| death == krate))
}

/// The names in neither the baseline nor the owned map,
/// sorted.
fn new_failures(st: &State, names: &[String]) -> Vec<String> {
    names
        .iter()
        .filter(|name| {
            !st.baseline.as_ref().is_some_and(|b| b.contains(*name)) && !st.red.contains_key(*name)
        })
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// The names in `[pump.gate] ignore_file`: one per line, trimmed, blank lines
/// and `#` comments skipped. Empty when unset or unreadable.
fn ignored_names(ctx: &Ctx) -> BTreeSet<String> {
    let Some(path) = ctx.cfg.ignore_file(&ctx.project) else {
        return BTreeSet::new();
    };
    match fs::read_to_string(path) {
        Ok(text) => text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(String::from)
            .collect(),
        Err(_) => BTreeSet::new(),
    }
}

/// Raise the `gate-new-failures` exception without assigning blame.
fn report_new_failures(ctx: &Ctx, rec: &GateRecord, new: &[String]) {
    let joined = cut_chars(&new.join("; "), 350);
    ctx.exception(
        "gate-new-failures",
        &tip_short(&rec.tip),
        &format!("{} new: {joined}", new.len()),
    );
}

/// While merging is held: once every fix unit has settled, a
/// clean gate newer than the `HOLD-MERGE` flag lifts the hold; fixes merged
/// after the last gate (or an unreadable one) force a new gate.
pub fn hold_cycle(ctx: &Ctx, st: &mut State) {
    if ctx.cfg.gate.hold_on_red && !st.red.is_empty() && !ctx.flag(HOLD_FLAG) {
        match fs::write(ctx.home.join(HOLD_FLAG), HOLD_AUTO_TEXT) {
            Ok(()) => ctx.log(&format!(
                "hold on red: {} owned names unfixed; HOLD-MERGE written",
                st.red.len()
            )),
            Err(err) => ctx.log(&format!("hold flag write failed: {err}")),
        }
    }
    if !ctx.flag(HOLD_FLAG)
        || st.last_gate.task.is_some()
        || st.last_gate.job_id.is_some()
        || ctx.flag(&force_flag(ctx))
    {
        return;
    }
    let prefix = ctx.cfg.fix_prefix.as_str();
    if !all_fixes_settled(st, prefix) {
        return;
    }
    let Some(last) = st.gates.last().cloned() else {
        return;
    };
    if clean_gate(&last) && last.at > file_mtime(&ctx.home.join(HOLD_FLAG)) {
        if !automatic_hold(ctx) {
            ctx.log("hold retained: HOLD-MERGE belongs to the operator");
            return;
        }
        remove_flag(ctx, HOLD_FLAG);
        ctx.log(&format!(
            "hold lifted: gate at {} is clean",
            tip_short(&last.tip)
        ));
        return;
    }
    if newest_merged_fix(st, prefix) > last.at || last.names.is_none() {
        write_force_flag(ctx);
        ctx.log("hold: fixes merged, forcing a gate");
    }
}

/// Whether Vivi owns the current hold marker and may clear it after a clean gate.
fn automatic_hold(ctx: &Ctx) -> bool {
    fs::read_to_string(ctx.home.join(HOLD_FLAG))
        .is_ok_and(|text| text == HOLD_AUTO_TEXT || text == HOLD_LEGACY_AUTO_TEXT)
}

/// Write the automatic hold when a readable red gate starts an audit.
fn write_hold_flag(ctx: &Ctx, tip: &str) {
    if ctx.flag(HOLD_FLAG) {
        ctx.log("hold already set; existing HOLD-MERGE preserved");
        return;
    }
    match fs::write(ctx.home.join(HOLD_FLAG), HOLD_AUTO_TEXT) {
        Ok(()) => ctx.log(&format!(
            "hold set automatically: gate red at {}",
            tip_short(tip)
        )),
        Err(err) => ctx.log(&format!("hold flag write failed: {err}")),
    }
}

/// Every `<fix_prefix>`-prefixed unit has a settled status.
fn all_fixes_settled(st: &State, prefix: &str) -> bool {
    st.units
        .iter()
        .filter(|(uid, _)| uid.starts_with(prefix))
        .all(|(_, unit)| is_settled(unit.status))
}

/// The newest `since` among merged fix units, or 0.
fn newest_merged_fix(st: &State, prefix: &str) -> u64 {
    st.units
        .iter()
        .filter(|(uid, unit)| uid.starts_with(prefix) && unit.status == Status::Merged)
        .map(|(_, unit)| unit.since)
        .max()
        .unwrap_or(0)
}

/// A gate record with readable names and no new failure.
fn clean_gate(gate: &GateRecord) -> bool {
    gate.names.is_some() && gate.new.as_ref().is_none_or(Vec::is_empty)
}

/// `true` for the statuses a fix unit ends in without further work: merged,
/// stuck, no-defect or replaced.
pub(crate) fn is_settled(status: Status) -> bool {
    matches!(
        status,
        Status::Merged | Status::Stuck | Status::NoDefect | Status::Replaced
    )
}

/// The flag file's modification time as epoch seconds, 0 when unknown.
fn file_mtime(path: &Path) -> u64 {
    match fs::metadata(path) {
        Ok(meta) => match meta.modified() {
            Ok(when) => when
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |span| span.as_secs()),
            Err(_) => 0,
        },
        Err(_) => 0,
    }
}

/// Write the `FORCE-GATE` flag with its payload, logging a failure.
fn write_force_flag(ctx: &Ctx) {
    if let Err(err) = fs::write(ctx.home.join(force_flag(ctx)), FORCE_TEXT) {
        ctx.log(&format!("gate flag write failed: {err}"));
    }
}

/// Remove a flag file, logging a failure.
fn remove_flag(ctx: &Ctx, name: &str) {
    if let Err(err) = fs::remove_file(ctx.home.join(name)) {
        ctx.log(&format!("gate flag remove failed: {err}"));
    }
}

/// The last `limit` characters of `text`.
fn tail_chars(text: &str, limit: usize) -> String {
    let count = text.chars().count();
    text.chars().skip(count.saturating_sub(limit)).collect()
}

/// The first `limit` characters of `text`.
fn cut_chars(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}
