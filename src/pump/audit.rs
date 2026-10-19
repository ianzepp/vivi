//! Red-gate auditing: the live audit run and the auditor brief.
//!
//! When the integration branch goes red the pump hands the failure to the
//! auditor instead of blaming a merge. The live run lives in
//! `state.extra["audit"]`, so `store::write_meta` persists it for free and
//! `state::with_target_state` swaps it per target.

use serde::{Deserialize, Serialize};

use crate::pump::host::Ctx;
use crate::pump::state::State;

/// The `state.json` key holding the live audit run.
pub(crate) const AUDIT_KEY: &str = "audit";

/// The operator's no-blame ruling, quoted verbatim in every audit brief.
const NO_BLAME: &str = "Do not try to identify which merge or commit caused a \
    failure: no suspect lists, no bisecting, no culprit commit in briefs, no \
    reverts by blame. Nothing goes back to a developer as a rejected commit. \
    Identify what is broken, fix it forward, move on.";

/// The required reply shape, quoted in every audit brief.
const REPLY_FORMAT: &str = "\
End your reply with a block between the exact markers AUDIT-BEGIN and \
AUDIT-END containing only a JSON array, one object per root cause:
{\"root_cause\": one line, \"brief\": the complete fix-unit brief naming the \
failing tests and the concrete fix, \"tests\": [failing test names this cause \
covers], \"write_scope\": [files the fix may write], \"proof\": [commands that \
must exit 0, one per failing test when possible]}

done_when: the AUDIT block is in the reply and every cause names its tests, a \
concrete fix, a write scope and proof
";

/// One audit run in flight, or the last one still owed a report.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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

/// The live audit run, if one is stored.
pub(crate) fn current(st: &State) -> Option<AuditRun> {
    let value = st.extra.get(AUDIT_KEY)?.clone();
    serde_json::from_value(value).ok()
}

/// Store (or clear) the live audit run in `state.extra`.
pub(crate) fn store(st: &mut State, run: Option<AuditRun>) {
    match run {
        Some(run) => {
            if let Ok(value) = serde_json::to_value(run) {
                st.extra.insert(String::from(AUDIT_KEY), value);
            }
        }
        None => {
            st.extra.remove(AUDIT_KEY);
        }
    }
}

/// The auditor brief for one red tip.
pub(crate) fn audit_brief(
    _ctx: &Ctx,
    tip: &str,
    output_tail: &str,
    new: &[String],
    residual: &[(String, String)],
    prior: Option<&str>,
) -> String {
    let mut brief = header(tip);
    brief.push_str("\nNew failing tests:\n");
    bullets(&mut brief, new.iter().map(String::as_str));
    let residual_names: Vec<&str> = residual.iter().map(|(name, _)| name.as_str()).collect();
    if !residual_names.is_empty() {
        brief.push_str("\nStill failing after earlier fixes:\n");
        bullets(&mut brief, residual_names.iter().copied());
    }
    fenced(&mut brief, "\nGate output (tail):\n", output_tail);
    if let Some(prior) = prior.filter(|text| !text.trim().is_empty()) {
        push_section(
            &mut brief,
            "\nPrior audit report (context, do not repeat it):\n",
            prior,
        );
    }
    brief.push('\n');
    brief.push_str(REPLY_FORMAT);
    brief
}

/// The fixed opening of an audit brief, ending before the failure lists.
fn header(tip: &str) -> String {
    format!(
        "The integration branch went red at {tip}. You are the auditor: read the \
         failing tests and the code they exercise and report what is broken and \
         why, grouped by root cause, with a concrete fix per cause. Fix forward \
         only.\n\n{NO_BLAME}\n\nIt must never weaken, skip or delete tests.\n"
    )
}

/// One bullet per name.
fn bullets<'a>(brief: &mut String, names: impl Iterator<Item = &'a str>) {
    for name in names {
        brief.push_str("- ");
        brief.push_str(name);
        brief.push('\n');
    }
}

/// A heading followed by `body` in a fenced block.
fn fenced(brief: &mut String, heading: &str, body: &str) {
    brief.push_str(heading);
    brief.push_str("```\n");
    brief.push_str(body);
    if !body.ends_with('\n') {
        brief.push('\n');
    }
    brief.push_str("```\n");
}

/// A heading followed by `body`, newline-terminated.
fn push_section(brief: &mut String, heading: &str, body: &str) {
    brief.push_str(heading);
    brief.push_str(body);
    if !body.ends_with('\n') {
        brief.push('\n');
    }
}
