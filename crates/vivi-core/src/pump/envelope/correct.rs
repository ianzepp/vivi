//! The envelope correction loop: when a reply does not parse, a small helper
//! job rewrites only the envelope. A correction never counts as an attempt.

use super::EnvelopeError;
use crate::pump::flow::index::StepJobRow;
use crate::pump::jobs::{self, JobSpec, JobStore, PoolJob};
use crate::pump::store::PumpStore;
use vivi_error::VivariumError;
use vivi_mail::letter::roles::RoleKind;

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

/// How many corrections lie behind a job id: `0` for an original job, `n`
/// for `<id>-c<n>`.
#[must_use]
pub fn correction_n(job_id: &str) -> u32 {
    split_id(job_id).1
}

/// The id of the job a correction chain started from.
#[must_use]
pub fn base_id(job_id: &str) -> &str {
    split_id(job_id).0
}

fn split_id(job_id: &str) -> (&str, u32) {
    if let Some((base, tail)) = job_id.rsplit_once("-c")
        && let Ok(n) = tail.parse::<u32>()
    {
        return (base, n);
    }
    (job_id, 0)
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
    let diagnosis = super::refusal::describe(original, error);
    let body = format!(
        "Correction for pump step {need} r{} {}.\n\n\
         Your last reply did not parse as a reply envelope.\n\
         Diagnosis for job {}: {diagnosis}\n\n\
         Rewrite only the envelope; do not redo the work; keep every fact. \
         Answer with the corrected reply and nothing else.\n\n\
         The envelope rules and this role's fields:\n{}\n\n\
         The reply to correct:\n{}",
        row.run,
        row.node,
        original.id,
        super::format::role(kind),
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
    spec
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

#[cfg(test)]
#[path = "correct_test.rs"]
mod tests;
