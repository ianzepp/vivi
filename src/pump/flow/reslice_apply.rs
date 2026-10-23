//! Applying a reslice reply: records the new slices, rewires the dependents
//! of a replaced slice, and files a follow-up need when a follow-up job
//! escalates.

use std::fs;

use crate::error::VivariumError;
use crate::pump::envelope::roles::{Slicer, SlicerAnswer};
use crate::pump::flow::def::VERDICT_REPLACED;
use crate::pump::flow::fanout;
use crate::pump::flow::handlers::FlowMail;
use crate::pump::flow::index::{RunRow, StepRow};
use crate::pump::flow::reslice::Mode;
use crate::pump::host::{Ctx, shell_quote};
use crate::pump::store::PumpStore;

/// Exception kind: a follow-up need was filed.
pub(crate) const FOLLOW_UP_FILED: &str = "flow-follow-up-filed";

/// Exception kind: a follow-up need could not be filed.
pub(crate) const FOLLOW_UP_UNROUTED: &str = "flow-follow-up-unrouted";

/// Act on the slicer's reply (`None` when the job failed or its reply did not
/// parse) for `old_slice`, and return the word to close the reslice task with:
/// `sliced`, `escalate` or `failed`.
///
/// # Errors
/// Returns a [`VivariumError`] when the fanout, the index or the mailspace
/// cannot be written.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply<M: FlowMail>(
    ctx: &Ctx,
    store: &PumpStore,
    mail: &mut M,
    run: &RunRow,
    reslice: &StepRow,
    old_slice: &str,
    mode: Mode,
    reply: Option<&Slicer>,
) -> Result<&'static str, VivariumError> {
    match reply {
        Some(slicer) if slicer.answer == SlicerAnswer::Sliced => {
            let new_tasks = fanout::expand(ctx, store, mail, run, reslice, &slicer.slice)?;
            if mode != Mode::FollowUp {
                rewire(store, mail, run, old_slice, &new_tasks)?;
                fanout::close_slice(store, mail, old_slice, VERDICT_REPLACED)?;
            }
            Ok("sliced")
        }
        other => {
            if mode == Mode::FollowUp {
                file_follow_up(ctx, mail, run, reslice, old_slice, other);
            } else {
                fanout::mark_exhausted(store, old_slice)?;
            }
            Ok(if other.is_some() {
                "escalate"
            } else {
                "failed"
            })
        }
    }
}

/// Point every dependent of `old_slice` at its replacements.
fn rewire<M: FlowMail>(
    store: &PumpStore,
    mail: &mut M,
    run: &RunRow,
    old_slice: &str,
    new_tasks: &[String],
) -> Result<(), VivariumError> {
    for link in store.slice_links_of_run(&run.need, run.run)? {
        if !link.deps.iter().any(|dep| dep == old_slice) || new_tasks.contains(&link.task) {
            continue;
        }
        let mut deps: Vec<String> = link
            .deps
            .iter()
            .filter(|dep| dep.as_str() != old_slice)
            .cloned()
            .collect();
        for task in new_tasks {
            if !deps.contains(task) {
                deps.push(task.clone());
            }
        }
        store.set_slice_deps(&link.task, &deps)?;
        fanout::wait_on(mail, &link.task, new_tasks)?;
    }
    Ok(())
}

/// File the follow-up input as a new need. A failure raises the unrouted
/// alarm and never fails the apply.
fn file_follow_up<M: FlowMail>(
    ctx: &Ctx,
    mail: &M,
    run: &RunRow,
    reslice: &StepRow,
    old_slice: &str,
    reply: Option<&Slicer>,
) {
    let unit = format!("{} r{}", run.need, run.run);
    match send_need(ctx, mail, run, reslice, old_slice, reply) {
        Ok(handle) => ctx.exception(
            FOLLOW_UP_FILED,
            &unit,
            &format!("follow-up of slice {old_slice} filed as need {handle}"),
        ),
        Err(error) => ctx.exception(
            FOLLOW_UP_UNROUTED,
            &unit,
            &format!("follow-up of slice {old_slice} not filed: {error}"),
        ),
    }
}

fn send_need<M: FlowMail>(
    ctx: &Ctx,
    mail: &M,
    run: &RunRow,
    reslice: &StepRow,
    old_slice: &str,
    reply: Option<&Slicer>,
) -> Result<String, VivariumError> {
    let input = mail.task_body(&reslice.task)?;
    let reason = reply
        .and_then(|slicer| slicer.reason.as_deref())
        .unwrap_or("the slicer job failed");
    let body = format!(
        "Follow-up found while running need {} r{}, slice task {old_slice}.\n\n\
         Follow-up input:\n{input}\n\nSlicer reason: {reason}\n",
        run.need, run.run
    );
    let subject = format!("follow-up of {} r{} slice {old_slice}", run.need, run.run);
    let key: String = format!("{}-r{}-{}", run.need, run.run, reslice.task)
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let path = ctx.home.join("follow-up").join(format!("{key}.txt"));
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, body)?;
    let from = shell_quote(&ctx.cfg.reply_from);
    let command = format!(
        "need send --from {from} --to {from} --subject {} --body-file {}",
        shell_quote(&subject),
        shell_quote(&path.to_string_lossy()),
    );
    let output = ctx.vivi(&command, 60);
    if output.code != 0 {
        return Err(VivariumError::Message(format!(
            "need send exited {}: {}",
            output.code,
            crate::pump::collect::tail(&output.text, 200)
        )));
    }
    crate::pump::flow::residual::parse_need_handle(&output.text).ok_or_else(|| {
        VivariumError::Message(format!(
            "need send gave no handle: {}",
            crate::pump::collect::tail(&output.text, 200)
        ))
    })
}

#[cfg(test)]
#[path = "reslice_apply_test.rs"]
mod tests;
