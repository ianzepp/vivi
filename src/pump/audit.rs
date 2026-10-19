//! Red-gate auditing state and the auditor's fix-forward brief.

use std::fs;

use serde::{Deserialize, Serialize};

use crate::pump::host::Ctx;
use crate::pump::state::{NeedRecord, State, Status, Unit};

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

const REPORT_BEGIN: &str = "AUDIT-BEGIN";
const REPORT_END: &str = "AUDIT-END";

/// One root cause from the auditor's reply: the seed of one fix unit.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct Cause {
    /// One line: what is broken and why.
    pub(crate) root_cause: String,
    /// The complete fix-unit brief naming the failing tests and concrete fix.
    pub(crate) brief: String,
    /// The failing test names this cause covers.
    #[serde(default)]
    pub(crate) tests: Vec<String>,
    /// Files the fix may write.
    pub(crate) write_scope: Vec<String>,
    /// Commands that must exit 0, one per failing test when possible.
    #[serde(default)]
    pub(crate) proof: Vec<String>,
}

/// The body of the last complete `AUDIT-BEGIN ... AUDIT-END` block in the
/// reply, or `None` when no complete block exists. Local port of the gate's
/// `marker_blocks` scan: a marker followed by no newline starts no block, and
/// a `BEGIN` with no later `END` ends the scan.
fn last_report_block(text: &str) -> Option<String> {
    let mut last = None;
    let mut rest = text;
    while let Some(at) = rest.find(REPORT_BEGIN) {
        let after = &rest[at + REPORT_BEGIN.len()..];
        let ws = after.len() - after.trim_start().len();
        if !after[..ws].contains('\n') {
            rest = after;
            continue;
        }
        let body = &after[ws..];
        let Some(end) = body.find(REPORT_END) else {
            break;
        };
        last = Some(body[..end].to_owned());
        rest = &body[end + REPORT_END.len()..];
    }
    last
}

/// Parse the auditor reply's last `AUDIT-BEGIN ... AUDIT-END` block into one
/// [`Cause`] per root cause.
///
/// Returns `None` when the reply carries no complete block, the block body is
/// not a JSON array of causes, the array is empty, or any cause has an empty
/// root cause, brief, or write scope.
pub(crate) fn parse_report(text: &str) -> Option<Vec<Cause>> {
    let body = last_report_block(text)?;
    let causes = match serde_json::from_str::<Vec<Cause>>(body.trim()) {
        Ok(causes) => causes,
        Err(_) => return None,
    };
    if causes.is_empty() {
        return None;
    }
    for cause in &causes {
        if cause.root_cause.trim().is_empty()
            || cause.brief.trim().is_empty()
            || cause.write_scope.is_empty()
        {
            return None;
        }
    }
    Some(causes)
}

/// Store the auditor's raw reply and mint one pending fix unit per cause.
///
/// The raw text is written to `<home>/audit/<need>.report`, where later
/// audits read it as context. Cause *k* becomes unit `<need>-<k+1>` bound to
/// the need; the need is recorded as `units` with the minted handles. The
/// record overwrites any earlier one: a need that reaches the audit path
/// never had a plan file. Returns the number of units minted.
pub(crate) fn apply_report(
    ctx: &Ctx,
    st: &mut State,
    need: &str,
    raw: &str,
    causes: &[Cause],
) -> usize {
    let dir = ctx.home.join("audit");
    drop(fs::create_dir_all(&dir));
    drop(fs::write(dir.join(format!("{need}.report")), raw));

    let mut uids = Vec::new();
    for (k, cause) in causes.iter().enumerate() {
        let uid = format!("{need}-{}", k + 1);
        let unit = Unit {
            need: need.to_string(),
            title: cause.root_cause.chars().take(90).collect(),
            brief: cause.brief.clone(),
            write_scope: cause.write_scope.clone(),
            proof: cause.proof.clone(),
            tools: String::new(),
            status: Status::Pending,
            since: ctx.now(),
            ..Unit::default()
        };
        st.units.insert(uid.clone(), unit);
        st.order.push(uid.clone());
        uids.push(uid);
    }
    st.needs.insert(
        need.to_string(),
        NeedRecord {
            verdict: String::from("units"),
            reason: String::from("audit report"),
            units: uids,
            revision: 1,
            ..NeedRecord::default()
        },
    );
    ctx.log(&format!(
        "audit report for {need}: {} fix units",
        causes.len()
    ));
    causes.len()
}
