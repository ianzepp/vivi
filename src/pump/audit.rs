//! Red-gate auditing state and the auditor's fix-forward brief.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::pump::dispatch::{parse_created_handle, slots};
use crate::pump::gatefix::{fix_need, send_fix_need, take_residuals};
use crate::pump::host::{Ctx, shell_quote};
use crate::pump::state::State;
use crate::routing::RoleRouting;

/// Seconds before an audit that could not start is retried.
const RETRY_SECONDS: u64 = 60;

/// The `state.extra` key holding the live audit run.
pub(crate) const AUDIT_KEY: &str = "audit";

const NO_BLAME: &str = "Do not try to identify which merge or commit caused a \
    failure: no suspect lists, no bisecting, no culprit commit in briefs, no \
    reverts by blame. Nothing goes back to a developer as a rejected commit. \
    Identify what is broken, fix it forward, move on.";

const REPLY_FORMAT: &str = "\
End the reply with a block between the exact markers AUDIT-BEGIN and AUDIT-END \
containing only a JSON array, one object per root cause. Each object must \
contain root_cause (one line), brief (the complete fix-unit brief naming the \
failing tests and concrete fix), tests (the failing test names this cause \
covers), write_scope (files the fix may write), and proof (commands that must \
exit 0, one per failing test when possible). Use this JSON shape:\n\
AUDIT-BEGIN\n\
[\n\
  {\n\
    \"root_cause\": \"<one line>\",\n\
    \"brief\": \"<complete fix-unit brief naming failing tests and concrete fix>\",\n\
    \"tests\": [\"<failing test name>\"],\n\
    \"write_scope\": [\"<file the fix may write>\"],\n\
    \"proof\": [\"<command that exits 0>\"]\n\
  }\n\
]\n\
AUDIT-END\n\
done_when: the AUDIT block is in the reply and every cause names its tests, a \
concrete fix, a write scope and proof\n";

/// One audit run in flight, or the last one still owed a report.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditRun {
    /// Vivi task handle while the audit runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// The fix need id owning the red names.
    #[serde(default)]
    pub need: String,
    /// The integration tip that went red.
    #[serde(default)]
    pub tip: String,
    /// The pool the auditor runs in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool: Option<String>,
    /// Epoch the audit started.
    #[serde(default)]
    pub at: u64,
    /// Consecutive attempts that produced no report.
    #[serde(default)]
    pub fails: u64,
    /// Earliest epoch a retry may go out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_at: Option<u64>,
}

/// Start the red-gate audit for `tip`.
///
/// The failing names are owned by a gate-fix need first, so promotion stays
/// held while the audit runs. The audit task then goes to the mailspace's
/// `auditor` role through the normal role-to-pool routing: role charter,
/// identity, pool and model all come from the role config. A mailspace with
/// no auditor role falls back to `[pump] to`, a gate-style read-only worker
/// dispatched with the gate model.
///
/// Nothing is sent while an audit task is open, and nothing is sent when fix
/// units already exist for this tip.
pub fn start_audit(
    ctx: &Ctx,
    st: &mut State,
    tip: &str,
    output_tail: &str,
    new: &[String],
    residual: &[(String, String)],
    prior: Option<&str>,
) {
    if current(st).is_some_and(|run| run.task.is_some()) {
        return;
    }
    let base = fix_need(ctx, tip);
    if st.units.values().any(|unit| unit.need == base) {
        return;
    }
    let owner = send_fix_need(ctx, tip, new, residual).unwrap_or(base);
    for name in new {
        st.red.insert(name.clone(), owner.clone());
    }
    take_residuals(ctx, st, &owner, residual);
    let mut run = AuditRun {
        need: owner.clone(),
        tip: tip.to_owned(),
        at: ctx.now(),
        retry_at: Some(ctx.now().saturating_add(RETRY_SECONDS)),
        ..AuditRun::default()
    };
    let role = ctx.routing.auditor();
    let free = RoleRouting::first_free(
        role.and_then(|role| role.pool.as_deref()),
        &ctx.cfg.gate.pools,
        &slots(ctx),
    );
    let Some(pool) = free else {
        store(st, Some(run));
        ctx.log(&format!("audit waiting for a free slot for {owner}"));
        return;
    };
    run.pool = Some(pool);
    let brief = audit_brief(ctx, tip, output_tail, new, residual, prior);
    let _ = dispatch_audit(ctx, &mut run, Some(&brief), new.len(), residual.len());
    store(st, Some(run));
}

/// Write the brief (when one is given), send the audit task and dispatch it;
/// `false` means no audit is running and the run is armed for a retry. A
/// retry passes no brief and reuses the one already on disk.
fn dispatch_audit(
    ctx: &Ctx,
    run: &mut AuditRun,
    brief: Option<&str>,
    new: usize,
    residual: usize,
) -> bool {
    let role = ctx.routing.auditor();
    let to = role.map_or_else(|| ctx.cfg.to.clone(), |role| role.name.clone());
    let Some(pool) = run.pool.clone() else {
        return retry_later(ctx, run);
    };
    let Some(path) = audit_brief_file(ctx, brief) else {
        return false;
    };
    let failures = new + residual;
    let subject = format!(
        "[pump audit] {failures} failures at {}",
        tip_short(&run.tip)
    );
    let out = ctx.vivi(
        &format!(
            "task send --from {} --to {} --subject {} --body-file {}",
            shell_quote(&ctx.cfg.from),
            shell_quote(&to),
            shell_quote(&subject),
            shell_quote(&path.to_string_lossy()),
        ),
        120,
    );
    let Some(task) = parse_created_handle(&out.text) else {
        ctx.log(&format!("audit task send failed: {}", last_line(&out.text)));
        return retry_later(ctx, run);
    };
    let args = audit_dispatch_args(ctx, run, &pool, &task, role.is_none());
    let out = ctx.vivi(&args, 180);
    if out.code != 0 || !out.text.contains("dispatched") {
        log_reply_tail(ctx, "audit dispatch failed", &out.text);
        drop(ctx.vivi(
            &format!(
                "task done {task} --for {} --note 'dispatch failed'",
                ctx.cfg.from
            ),
            120,
        ));
        return retry_later(ctx, run);
    }
    run.task = Some(task.clone());
    run.at = ctx.now();
    run.fails = 0;
    run.retry_at = None;
    ctx.log(&format!(
        "audit started for {} ({new} new, {residual} residual) task={task}",
        run.need
    ));
    true
}

/// The `pool dispatch` line for the audit task. A real auditor role supplies
/// its own charter, read-only default and band model, so the gate model and
/// `--read-only` are only sent for the built-in fallback worker.
fn audit_dispatch_args(
    ctx: &Ctx,
    run: &AuditRun,
    pool: &str,
    task: &str,
    fallback: bool,
) -> String {
    let mut args = format!(
        "pool dispatch --pool {pool} {task} --from {} --budget {} --porcelain",
        run.tip, ctx.cfg.gate.budget,
    );
    if fallback {
        args.push_str(&format!(" --read-only --model {}", ctx.cfg.gate.model));
    }
    args
}

/// Write the rendered brief to `<home>/briefs/audit.txt`; no brief means a
/// retry, which reuses the file the first attempt wrote.
fn audit_brief_file(ctx: &Ctx, brief: Option<&str>) -> Option<PathBuf> {
    let path = ctx.home.join("briefs").join("audit.txt");
    let text = brief?;
    if let Some(dir) = path.parent()
        && let Err(err) = fs::create_dir_all(dir)
    {
        ctx.log(&format!("audit brief dir failed: {err}"));
        return None;
    }
    if let Err(err) = fs::write(&path, text) {
        ctx.log(&format!("audit brief write failed: {err}"));
        return None;
    }
    Some(path)
}

/// Count one attempt that produced no running audit and arm the retry.
fn retry_later(ctx: &Ctx, run: &mut AuditRun) -> bool {
    run.task = None;
    run.fails = run.fails.saturating_add(1);
    run.retry_at = Some(ctx.now().saturating_add(RETRY_SECONDS));
    ctx.log(&format!(
        "audit retry {}/{}s for {}",
        run.fails, RETRY_SECONDS, run.need
    ));
    false
}

/// Log the last 200 characters of a command reply as one line.
fn log_reply_tail(ctx: &Ctx, what: &str, text: &str) {
    let count = text.chars().count();
    let tail: String = text.chars().skip(count.saturating_sub(200)).collect();
    ctx.log(&format!("{what}: {}", tail.replace('\n', " | ")));
}

/// The last non-blank line of `text`, cut to 80 chars.
fn last_line(text: &str) -> String {
    let line = text.trim().lines().last().unwrap_or("");
    line.chars().take(80).collect()
}

/// The first ten characters of a tip, for log and subject lines.
fn tip_short(tip: &str) -> String {
    tip.chars().take(10).collect()
}

/// Read the live audit run, if one is stored.
pub(crate) fn current(st: &State) -> Option<AuditRun> {
    let value = st.extra.get(AUDIT_KEY)?.clone();
    match serde_json::from_value(value) {
        Ok(run) => Some(run),
        Err(_) => None,
    }
}

/// Store (or clear) the live audit run in `state.extra`.
pub(crate) fn store(st: &mut State, run: Option<AuditRun>) {
    match run {
        Some(run) => match serde_json::to_value(run) {
            Ok(value) => {
                st.extra.insert(String::from(AUDIT_KEY), value);
            }
            Err(_) => {
                st.extra.remove(AUDIT_KEY);
            }
        },
        None => {
            st.extra.remove(AUDIT_KEY);
        }
    }
}

/// Build the auditor brief for one red integration tip.
pub(crate) fn audit_brief(
    _ctx: &crate::pump::host::Ctx,
    tip: &str,
    output_tail: &str,
    new: &[String],
    residual: &[(String, String)],
    prior: Option<&str>,
) -> String {
    let mut brief = format!(
        "The integration branch went red at {tip}. You are the auditor: read the \
         failing tests and the code they exercise and report what is broken and \
         why, grouped by root cause, with a concrete fix per cause. Fix forward \
         only.\n\n{NO_BLAME}\n\nIt must never weaken, skip or delete tests.\n"
    );
    brief.push_str("\nNew failing tests:\n");
    append_bullets(&mut brief, new.iter().map(String::as_str));

    let residual_names = residual.iter().map(|(name, _)| name.as_str());
    if !residual.is_empty() {
        brief.push_str("\nStill failing after earlier fixes:\n");
        append_bullets(&mut brief, residual_names);
    }

    brief.push_str("\nGate output (tail):\n```\n");
    brief.push_str(output_tail);
    if !output_tail.ends_with('\n') {
        brief.push('\n');
    }
    brief.push_str("```\n");

    if let Some(prior) = prior.filter(|text| !text.trim().is_empty()) {
        brief.push_str("\nPrior audit report (context, do not repeat it):\n");
        brief.push_str(prior);
        if !prior.ends_with('\n') {
            brief.push('\n');
        }
    }
    brief.push('\n');
    brief.push_str(REPLY_FORMAT);
    brief
}

fn append_bullets<'a>(brief: &mut String, names: impl Iterator<Item = &'a str>) {
    for name in names {
        brief.push_str("- ");
        brief.push_str(name);
        brief.push('\n');
    }
}
