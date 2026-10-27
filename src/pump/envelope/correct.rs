//! The envelope correction loop: when a reply does not parse, a small helper
//! job rewrites only the envelope. A correction never counts as an attempt.

use super::EnvelopeError;
use super::roles::RoleKind;
use crate::error::VivariumError;
use crate::pump::flow::index::StepJobRow;
use crate::pump::flow::recovery;
use crate::pump::jobs::{self, JobSpec, JobStore, PoolJob};
use crate::pump::store::PumpStore;

/// Job kind of a correction job.
pub const KIND: &str = "envelope-fix";
/// The original final message is cut to this many bytes in the body.
const ORIGINAL_CAP: usize = 16 * 1024;

/// Whether a job's harness session can be resumed to correct its reply.
///
/// Always false: the pool records no harness session (the slice file's C18,
/// F8). This is the seam for same-session correction later.
#[must_use]
pub fn resumable(_job: &PoolJob) -> bool {
    false
}

/// The correction suffix of the job-id grammar shared with
/// `flow::recovery`: `<base>[-tr<n>][-c<n>]`. A transient retry advances
/// the `-tr` ordinal of the step input; a correction of that retry appends
/// its own `-c` ordinal. Suffixes never stack.
const CORRECTION_SUFFIX: &str = "-c";

/// How many corrections lie behind a job id: `0` for an original job or a
/// plain transient retry (`<id>-tr<n>`), `n` for a trailing `-c<n>`.
#[must_use]
pub fn correction_n(job_id: &str) -> u32 {
    split_id(job_id).1
}

/// The id of the job a correction chain corrects: the chain base with any
/// transient-retry segment retained. A correction of a retried role
/// request (`j-tr1-c1`) resolves to that retried request (`j-tr1`) — the
/// job whose broken reply and machine facts the chain carries — never to
/// an older execution (`j`). Recovery reads the same id from the step
/// input's side (`flow::recovery::base_id` strips the retry too).
#[must_use]
pub fn base_id(job_id: &str) -> &str {
    split_id(job_id).0
}

/// Split a job id into its correction-chain base and correction ordinal:
/// the correction-coordinate mirror of `flow::recovery::split_id`. Only a
/// trailing well-formed `-c<n>` counts, so a retry segment stays in the
/// base here while recovery sees through it to the step input; a malformed
/// or stacked tail (`j-c`, `j-tr1-tr2`, `j-c1-tr1`) leaves an ordinary
/// job.
fn split_id(job_id: &str) -> (&str, u32) {
    match job_id.rsplit_once(CORRECTION_SUFFIX) {
        Some((base, tail)) => tail.parse().map_or((job_id, 0), |n| (base, n)),
        None => (job_id, 0),
    }
}

/// The correction job number `n` for the broken output of `original`.
///
/// `need` and the step's run and node are named in the body's first prose
/// line. The body is a pool job body, not a task letter, so it carries no
/// `[step]` table; the job's own front matter (W3-B3) is where one can go.
///
/// `budget` is the resolved envelope-fix budget in seconds, from
/// configuration — never the failed role's own budget.
#[must_use]
pub fn spec(
    need: &str,
    row: &StepJobRow,
    role: &str,
    kind: RoleKind,
    original: &PoolJob,
    error: &EnvelopeError,
    n: u32,
    budget: u64,
) -> JobSpec {
    let id = format!("{}-c{n}", base_id(&original.id));
    let body = format!(
        "Correction for pump step {need} r{} {}.\n\n\
         Your last reply did not parse as a reply envelope.\n\
         Parse error: {error}\n\n\
         Rewrite only the envelope; do not redo the work; keep every fact. \
         Answer with the corrected reply and nothing else.\n\n\
         The envelope rules and this role's fields:\n{}\n\n\
         The reply to correct:\n{}",
        row.run,
        row.node,
        role_row(kind),
        capped(&final_text(original)),
    );
    let mut spec = JobSpec::new(id, KIND, role, format!("envelope fix: {}", row.node), body);
    spec.target.clone_from(&original.target);
    spec.pool_candidates.clone_from(&original.pool_candidates);
    spec.local_only = original.local_only;
    spec.from_ref.clone_from(&original.from_ref);
    spec.extra_targets.clone_from(&original.extra_targets);
    spec.with.clone_from(&original.with);
    spec.builtin_role.clone_from(&original.builtin_role);
    spec.model.clone_from(&original.model);
    spec.read_only = true;
    spec.budget = budget;
    carry_step_input_provenance(&mut spec.payload, original);
    spec
}

/// Carry the source job's step-input recovery provenance onto the
/// correction payload. A correction of a transiently retried job — or of a
/// correction of one — runs on the same step input, so it inherits the
/// source's `recovery` metadata and the transient-retry allowance already
/// spent stays spent: `flow::recovery::retries_spent` is unchanged across
/// the correction chain, and a correction never reads as a fresh input
/// minting a second autonomous allowance. A source without recovery
/// metadata leaves the payload untouched; this writes no classifier input
/// and touches no correction ordinal, so execution and correction budget
/// semantics are the ordinary ones.
fn carry_step_input_provenance(payload: &mut serde_json::Value, source: &PoolJob) {
    if let Some(meta) = recovery::metadata(&source.payload) {
        recovery::stamp_metadata(payload, &meta);
    }
}

/// Queue the correction job and record it as a job of the step. The row
/// keeps the corrected job's attempt, so the attempt count is unchanged.
///
/// # Errors
/// Returns [`VivariumError`] when the job id names different work or the
/// index cannot be written.
pub fn enqueue(
    st: &mut impl JobStore,
    store: &PumpStore,
    need: &str,
    row: &StepJobRow,
    mut spec: JobSpec,
) -> Result<(), VivariumError> {
    let record = row.again(&spec.id);
    record.stamp(need, &mut spec.payload);
    jobs::enqueue(st, spec)?;
    store.insert_step_job(need, &record)
}

fn final_text(job: &PoolJob) -> String {
    job.outcome
        .as_ref()
        .map(|outcome| crate::pump::attempt_record::hand_message(&outcome.receipt))
        .unwrap_or_default()
}

fn capped(text: &str) -> String {
    if text.len() <= ORIGINAL_CAP {
        return text.to_owned();
    }
    let mut end = ORIGINAL_CAP;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[cut at {ORIGINAL_CAP} bytes]", &text[..end])
}

/// The role's section 1a row as text, with the general envelope rules.
fn role_row(kind: RoleKind) -> String {
    format!("{RULES}\n{}", fields(kind))
}

const RULES: &str = "The reply opens with TOML front matter between two lines containing only \
`+++`, then the prose body. Unknown keys are refused. Write free text as literal strings \
'...' (or '''...''' for several lines). Lists default to empty.";

fn fields(kind: RoleKind) -> &'static str {
    match kind {
        RoleKind::Researcher => {
            "researcher: answer = \"READY\" | \"NOT READY\" | \"NEEDS FURTHER REVIEW\"; open_questions (list)"
        }
        RoleKind::Planner => {
            "planner: answer = \"planned\" | \"delivered\" | \"operator\" | \"hold\" | \"design\"; reason (string, required unless planned)"
        }
        RoleKind::Admitter => {
            "admitter: answer = \"admitted\" | \"revise\" | \"escalate\"; required_changes (list, non-empty for revise); findings (list); reason (string, required for escalate)"
        }
        RoleKind::Slicer => {
            "slicer: answer = \"sliced\" | \"escalate\"; mode = \"slice\" | \"rework\" | \"split\"; [[slice]] {id, title, brief, target, write_scope (list), deps (list)}, at least one for sliced and none for escalate; reason (string, required for escalate)"
        }
        RoleKind::Auditor => {
            "auditor: answer = \"clean_pass\" | \"residual\" | \"block_range\" | \"block_ship\"; raise_rung = \"need\" | \"main\" | \"release\" (optional); [[requirement]] {id, text, status = SATISFIED | PARTIAL | MISSING | UNVERIFIED | NOT_APPLICABLE, evidence (list)}; [[finding]] {id, content, paths (list), recheck_trigger}"
        }
        RoleKind::Judge => "judge: answer = \"block\" | \"residual\"; reason (string, required)",
        RoleKind::Cto => {
            "head-cto: answer = \"research\" | \"plan\" | \"slice\" | \"rework\" | \"lock\"; reason (string, required); new_fact (string, required for a non-lock answer from run 3 on)"
        }
        RoleKind::Merge => {
            "merge: answer = \"merged\" | \"conflict\"; conflicts (list of paths); design_change (bool); [[check]] {cmd, result = pass | fail | skipped, seconds}. No sha, ref, commit or exit code."
        }
        RoleKind::Hand => {
            "hand: answer = \"built\" | \"blocked-scope\" | \"failed\"; change (list); [[check]] {cmd, result = pass | fail | skipped, seconds}; deviations (list); [[follow_up]] {paths (list), need}; remaining (list); scope_paths (list, required for blocked-scope); scope_reason (string, required for blocked-scope); scope_design (bool)"
        }
        RoleKind::Repairer => {
            "repairer: answer = \"built\" | \"failed\"; change (list); [[check]] {cmd, result = pass | fail | skipped, seconds}; deviations (list). No sha, ref, commit or exit code."
        }
        RoleKind::HeadCeo => {
            "head-ceo: answer = \"reviewed\" | \"no-op\"; served, research, dropped (lists of handles)"
        }
    }
}

#[cfg(test)]
#[path = "correct_test.rs"]
mod tests;
