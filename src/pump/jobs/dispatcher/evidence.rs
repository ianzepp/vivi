//! Durable dispatch evidence, written when a dispatch starts and when a pool
//! accepts it.
//!
//! Two writes book one dispatch in the pump store's recovery tables: the
//! start evidence captured before `pool dispatch` runs (the task, the pump
//! job and brief identity, the candidate pool, and the dispatch identities
//! already observed on the task), and the association the pool accepted
//! afterwards (task, pool, slot, the pool's own job id). Both outlive the
//! in-memory job map, so a crash between the pool taking the job and the
//! next state save still leaves the ownership decision to
//! [`execution_receipt`](crate::pump::jobs::execution_receipt), not to
//! guessing from whatever answered last on the task.

use crate::error::VivariumError;
use crate::letter::records::{MailFront, NoteWord};
use crate::letter::work::JobFront;
use crate::mailspace::PoolEventRow;
use crate::pump::host::{Ctx, shell_quote};
use crate::pump::jobs::{JobStore, PoolJob, execution_receipt, tail};
use crate::pump::store::{DB_FILENAME, PumpStore};

/// How a pool answered one `pool dispatch` call.
pub(super) enum Answer {
    /// A `dispatched TASK POOL SLOT JOB ...` line for `task`: the pool took
    /// the job, on `slot`, under its own job id.
    Dispatched { slot: String, pool_job: String },
    /// A `refused TASK REASON` line for `task`: the dispatch can never work
    /// as asked.
    Refused,
    /// Anything else: the response reads as neither acceptance nor refusal.
    Uncertain,
}

/// Read how the pool answered one `pool dispatch` call. Only the porcelain
/// result lines count: a `dispatched` line for this task accepts it, a
/// `refused` line for this task refuses it, and every other response — a
/// truncated line, a provider error, a result line for another task — is
/// uncertain and left to the saved evidence to resolve.
pub(super) fn classify(task: &str, text: &str) -> Answer {
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("refused ")
            && let Some((refused, _)) = token(rest)
        {
            if refused == task {
                return Answer::Refused;
            }
        }
        if let Some(rest) = line.strip_prefix("dispatched ")
            && let Some(fields) = dispatched_fields(rest)
        {
            return if fields.task == task {
                Answer::Dispatched {
                    slot: fields.slot,
                    pool_job: fields.pool_job,
                }
            } else {
                Answer::Uncertain
            };
        }
    }
    Answer::Uncertain
}

/// The identity fields of a `dispatched` line that concern the pump: task,
/// slot and the pool's own job id. The pool field is read and dropped: the
/// pool asked for is the pool that answers.
struct DispatchedFields {
    task: String,
    slot: String,
    pool_job: String,
}

fn dispatched_fields(rest: &str) -> Option<DispatchedFields> {
    let (task, rest) = token(rest)?;
    let (_, rest) = token(rest)?;
    let (slot, rest) = token(rest)?;
    let (pool_job, _) = token(rest)?;
    Some(DispatchedFields {
        task,
        slot,
        pool_job,
    })
}

/// One porcelain value: a bare word, or a double-quoted string carrying the
/// porcelain escapes. Decodes exactly what
/// [`crate::pool::porcelain::quote`] writes; shell-aware, so a value with
/// whitespace, quotes or backslashes survives the round trip.
fn token(text: &str) -> Option<(String, &str)> {
    let text = text.trim_start();
    let Some(body) = text.strip_prefix('"') else {
        let end = text.find(char::is_whitespace).unwrap_or(text.len());
        return (end > 0).then(|| (text[..end].to_owned(), &text[end..]));
    };
    let mut out = String::new();
    let mut chars = body.char_indices();
    while let Some((at, c)) = chars.next() {
        match c {
            '"' => return Some((out, &body[at + 1..])),
            '\\' => match chars.next()?.1 {
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                'u' => {
                    let (_, open) = chars.next()?;
                    if open != '{' {
                        return None;
                    }
                    let hex: String = chars
                        .by_ref()
                        .map(|(_, c)| c)
                        .take_while(|c| *c != '}')
                        .collect();
                    let code = u32::from_str_radix(&hex, 16).ok()?;
                    out.push(char::from_u32(code)?);
                }
                other => out.push(other),
            },
            c => out.push(c),
        }
    }
    None
}

/// Record the dispatch-start evidence for `job` about to be offered to
/// `pool` on `task`: the pump job and brief identity, the candidate pool,
/// and the dispatch identities already accepted on the task. Called before
/// `pool dispatch` runs; the first record for a job wins, so a later pool
/// never moves the anchor.
///
/// # Errors
/// Returns [`VivariumError`] when the store cannot be opened or written; a
/// dispatch without its start evidence would read unattributable later, so
/// the caller refuses to dispatch.
pub(super) fn record_start(
    ctx: &Ctx,
    job: &PoolJob,
    pool: &str,
    task: &str,
) -> Result<(), VivariumError> {
    let store = open(ctx)?;
    let prior = store.dispatch_pool_jobs_on_task(task)?;
    let evidence = serde_json::json!({
        "job": JobFront {
            id: job.id.clone(),
            kind: job.kind.clone(),
            builtin_role: job.builtin_role.clone(),
        },
        "task": task,
        "pool": pool,
        "prior_pool_jobs": prior,
    })
    .to_string();
    store.save_dispatch_start(&job.id, ctx.now(), &evidence)
}

/// Record the association a pool accepted for `job`: task, pool, slot and
/// the pool's own job id, plus the job snapshot for later readers.
///
/// # Errors
/// Returns [`VivariumError`] when the store cannot be opened or written.
pub(super) fn record_accept(
    ctx: &Ctx,
    job: &PoolJob,
    task: &str,
    pool: &str,
    slot: &str,
    pool_job: &str,
) -> Result<(), VivariumError> {
    let store = open(ctx)?;
    let snapshot = serde_json::to_string(job)
        .map_err(|err| VivariumError::Message(format!("job {} snapshot: {err}", job.id)))?;
    store.save_dispatch_accept(
        &job.id,
        Some(task),
        Some(pool),
        Some(slot),
        Some(pool_job),
        Some(&snapshot),
    )
}

/// Select what the saved evidence and the task's typed pool notes say about
/// `job`'s execution on `task`: the reader the collectors will share, fed
/// from the dispatcher's own side of the wires.
///
/// # Errors
/// Returns [`VivariumError`] when the saved evidence cannot be read.
pub(super) fn select_receipt(
    ctx: &Ctx,
    job: &str,
    task: &str,
) -> Result<execution_receipt::ExecutionReceipt, VivariumError> {
    let store = open(ctx)?;
    let saved = execution_receipt::load(&store, job)?;
    let (events, briefed) = thread_evidence(ctx, job, task);
    let mut prior = store.dispatch_pool_jobs_on_task(task)?;
    prior.retain(|pool_job| pool_job != job);
    Ok(execution_receipt::select(
        &saved,
        execution_receipt::ExecutionQuery {
            job,
            task,
            events: &events,
            briefed,
            prior_pool_jobs: &prior,
        },
    ))
}

/// The typed pool notes on `task`'s thread, oldest first, and whether the
/// thread carries a brief for `job` (a brief note reply, or the task
/// letter's own `[job]` front). An unreadable thread reads as no evidence:
/// selection then stays on the safe side by itself.
fn thread_evidence(ctx: &Ctx, job: &str, task: &str) -> (Vec<PoolEventRow>, bool) {
    let thread = ctx.vivi(
        &format!("mail thread {} --limit 1000 --json", shell_quote(task)),
        60,
    );
    if thread.code != 0 {
        ctx.log(&format!(
            "job dispatch thread read failed for {task}: {}",
            tail(&thread.text, 240)
        ));
        return (Vec::new(), false);
    }
    let mut events = Vec::new();
    let mut briefed = false;
    for body in crate::pump::jobs::recovery::bodies_of(&thread.text) {
        if let Some(note) = pool_note(&body, task) {
            events.push(note);
        }
        briefed |= names_job(&body, job);
    }
    (events, briefed)
}

/// The `[pool]` note of a mail letter as one pool event row. A body that is
/// not a pool note is not typed evidence.
fn pool_note(body: &str, task: &str) -> Option<PoolEventRow> {
    let letter = crate::letter::parse_letter::<MailFront>(body).ok()?;
    let pool = letter.front.pool?;
    Some(PoolEventRow {
        handle: task.to_owned(),
        event_type: pool.event,
        note: pool.record,
    })
}

/// Whether `body` names `job` as its pump brief or task letter.
fn names_job(body: &str, job: &str) -> bool {
    crate::mailspace::pump_note::note_of(body).is_some_and(|note| {
        note.pump.note == NoteWord::Brief && note.job.is_some_and(|front| front.id == job)
    }) || crate::letter::read::task_of("", body)
        .is_ok_and(|task| task.front.job.is_some_and(|front| front.id == job))
}

/// Hold `job` on an unresolved dispatch receipt: no pool may run it again
/// until an operator releases it, because a second dispatch could double-run
/// work the mailspace already attributes elsewhere.
pub(super) fn hold_ambiguous(
    ctx: &Ctx,
    st: &mut impl JobStore,
    snapshot: &PoolJob,
    task: &str,
    reason: &str,
) {
    let release = match snapshot.payload["task"].as_str() {
        Some(step) => format!("vivi pump requeue {step}"),
        None => format!("vivi pump requeue --job {}", snapshot.id),
    };
    if let Some(job) = st.jobs_mut().get_mut(&snapshot.id) {
        job.held = Some(String::from("unresolved dispatch receipt"));
    }
    ctx.log(&format!(
        "job dispatch held {} on task {task}: {reason}",
        snapshot.id
    ));
    ctx.exception(
        "flow-job-receipt-ambiguous",
        &snapshot.id,
        &format!(
            "pool dispatch for job {} on task {task} cannot be attributed: {reason}; \
             nothing was adopted and the job is held; fix the mailspace, then run \
             `{release}` to release it, or `vivi pump withdraw <need>`",
            snapshot.id
        ),
    );
}

/// A short-lived connection to the pump store: the recovery tables live
/// beside the state, and the dispatcher holds no long-lived handle.
fn open(ctx: &Ctx) -> Result<PumpStore, VivariumError> {
    PumpStore::open(&ctx.home.join(DB_FILENAME))
}
