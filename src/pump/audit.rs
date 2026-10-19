//! Red-gate auditing state and the auditor's fix-forward brief.

use serde::{Deserialize, Serialize};

use crate::pump::state::State;

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
