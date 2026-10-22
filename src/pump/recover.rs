//! Operator recovery of a held need branch: `vivi pump recover`, both halves.
//!
//! A need merge whose published tip the pump could not accept leaves the need
//! branch `unverified` ([`crate::pump::need::NeedBranchStatus::Unverified`]):
//! the remote `vivi/need/<id>` ref sits at a tip the pump never verified, and
//! dispatch for the need stops until an operator decides. This is that
//! decision. The CLI posts a recover action on the need's open pump record
//! ([`crate::mailspace::PumpActionRequest`]); the pump applies it once
//! ([`crate::pump::action`]):
//!
//! - only a branch held `unverified` is touched; any other status is logged
//!   (`need-recover-skipped`) and the request is dropped;
//! - the remote ref is read again. If it no longer sits at the tip the hold
//!   recorded, the request is refused (exception `need-recover-refused`) and
//!   the hold stays: the operator judged a tip that is gone;
//! - otherwise that tip becomes the branch's trusted tip, the hold clears and
//!   the branch is `active` again. The need's delivered units merge onto it on
//!   the next dispatch: one the tip already holds is reported "Already up to
//!   date" and accepted, so nothing is applied twice;
//! - an exception `need-recovered` records the old and the adopted tip.
//!
//! The pump does not re-prove ancestry of the adopted tip: adopting it is the
//! operator's statement that it is sound.

use std::path::Path;

use crate::VivariumError;
use crate::cli::PumpRecoverCommand;
use crate::mailspace::{Mailspace, PumpActionRequest};
use crate::pump::control::project_home;
use crate::pump::host::Ctx;
use crate::pump::need::{self, NeedBranch, NeedBranchStatus};
use crate::pump::state::{State, Status};
use crate::pump::store::{DB_FILENAME, PumpStore};

/// The reason a request carries when the operator gave none.
const DEFAULT_REASON: &str = "operator recover";

/// The remote ref a need branch lives at.
fn remote_ref(record: &NeedBranch) -> String {
    format!("refs/heads/{}", record.branch)
}

/// The need's branch records a request covers: `need`'s, on `target` only
/// when one is named.
fn candidates(
    st: &State,
    need: &str,
    target: Option<&str>,
) -> Result<Vec<NeedBranch>, VivariumError> {
    Ok(need::all(st)?
        .into_iter()
        .filter(|record| record.need == need && target.is_none_or(|target| record.target == target))
        .collect())
}

/// Apply one recover request for `need` to `st`: every covered branch held
/// `unverified` is recovered, every other one is skipped with a log line.
pub fn apply_one(ctx: &Ctx, st: &mut State, need: &str, target: Option<&str>, reason: &str) {
    let records = match candidates(st, need, target) {
        Ok(records) => records,
        Err(error) => {
            ctx.log(&format!("need-recover-skipped {need}: {error}"));
            return;
        }
    };
    if records.is_empty() {
        let scope = target.map_or_else(String::new, |target| format!(" on target {target}"));
        ctx.log(&format!(
            "need-recover-skipped {need}: no need branch record{scope}"
        ));
        return;
    }
    for record in records {
        recover_branch(ctx, st, &record, reason);
    }
}

/// Recover one branch record, or say why it was left alone.
fn recover_branch(ctx: &Ctx, st: &mut State, record: &NeedBranch, reason: &str) {
    let (target, need) = (record.target.as_str(), record.need.as_str());
    if record.status != NeedBranchStatus::Unverified {
        ctx.log(&format!(
            "need-recover-skipped {target}/{need}: status is {}, not unverified",
            record.status.as_str()
        ));
        return;
    }
    let held = record.observed_unverified_tip_sha.as_deref().unwrap_or("");
    let remote = crate::pump::merge::observed_ref_sha(ctx, target, &remote_ref(record));
    if remote.as_deref() != Some(held) {
        ctx.exception(
            "need-recover-refused",
            need,
            &format!(
                "target {target}: the remote ref is at {} but the hold recorded {held}; the hold stays",
                remote.as_deref().unwrap_or("an unreadable or absent tip")
            ),
        );
        return;
    }
    let previous = match need::recover_unverified(st, target, need, held) {
        Ok(previous) => previous,
        Err(error) => {
            ctx.log(&format!("need-recover-skipped {target}/{need}: {error}"));
            return;
        }
    };
    let waiting = requeue_units(st, target, need);
    let trusted = previous.branch_tip_sha.as_deref().unwrap_or("");
    let text = format!(
        "target {target}: adopted {held} over trusted tip {trusted}; {waiting} unit(s) will merge onto it again ({reason})"
    );
    ctx.exception("need-recovered", need, &text);
    ctx.log(&format!("need branch recovered {target}/{need}: {text}"));
}

/// Put the need's units on `target` that were mid-merge back to `delivered`,
/// so the next need merge takes them. Units already `delivered` stay, merged
/// ones are settled and stay. Returns how many units are waiting to merge.
fn requeue_units(st: &mut State, target: &str, need: &str) -> usize {
    let mut waiting = 0;
    for uid in need::units_for(st, target, need) {
        let Some(unit) = st.units.get_mut(&uid) else {
            continue;
        };
        if unit.status == Status::Merging {
            unit.status = Status::Delivered;
        }
        if unit.status == Status::Delivered {
            waiting += 1;
        }
    }
    waiting
}

/// `vivi pump recover <NEED>... [--target ID] [--reason TEXT] [--dry-run]`:
/// post one recover action per need on its open pump record, for the running
/// pump to apply at its next tick. Every need is checked first against the
/// saved state and the mailspace, so a need that cannot be recovered refuses
/// the whole command before anything is posted. `--dry-run` prints what each
/// held branch would do, with the remote ref as it reads now, and posts
/// nothing.
///
/// # Errors
/// Returns [`VivariumError::Message`] for a blank reason, a mailspace without
/// a pump database, a need with no branch held `unverified` (for the target,
/// when one is named), or a need with no open pump record, and whatever
/// reading the state or posting an action fails with.
pub fn run(project: &Path, cmd: &PumpRecoverCommand) -> Result<(), VivariumError> {
    let reason = cmd.reason.as_deref().unwrap_or(DEFAULT_REASON).trim();
    if reason.is_empty() {
        return Err(VivariumError::Message(String::from(
            "recover needs a non-empty --reason",
        )));
    }
    let home = project_home(project)?;
    let path = home.join(DB_FILENAME);
    let Some(store) = PumpStore::open_read_only(&path)? else {
        return Err(VivariumError::Message(format!(
            "no pump.db at {}",
            path.display()
        )));
    };
    let st = store.load()?;
    let target = cmd.target.as_deref();
    let mut held: Vec<NeedBranch> = Vec::new();
    for need in &cmd.needs {
        held.extend(unverified_branches(&st, need, target)?);
    }
    if cmd.dry_run {
        return print_dry_run(project, &home, &held);
    }
    let mailspace = Mailspace::discover(Some(project))?;
    for need in &cmd.needs {
        if mailspace.pump_open_record(need)?.is_none() {
            return Err(VivariumError::Message(format!(
                "need {need} has no open pump record, so there is no dispatch to recover; open one with `vivi pump send --need {need}` first"
            )));
        }
    }
    for need in &cmd.needs {
        mailspace.pump_post_action(None, need, &PumpActionRequest::recover(target, reason))?;
        println!("recover {need} requested; the pump applies it at its next tick");
    }
    Ok(())
}

/// The branches of `need` held `unverified`, or an error saying why there is
/// nothing to recover.
fn unverified_branches(
    st: &State,
    need: &str,
    target: Option<&str>,
) -> Result<Vec<NeedBranch>, VivariumError> {
    let records = candidates(st, need, target)?;
    if records.is_empty() {
        return Err(VivariumError::Message(format!(
            "need {need} has no need branch record{}",
            target.map_or_else(String::new, |target| format!(" on target {target}"))
        )));
    }
    let held: Vec<NeedBranch> = records
        .iter()
        .filter(|record| record.status == NeedBranchStatus::Unverified)
        .cloned()
        .collect();
    if held.is_empty() {
        let states: Vec<String> = records
            .iter()
            .map(|record| format!("{} is {}", record.target, record.status.as_str()))
            .collect();
        return Err(VivariumError::Message(format!(
            "need {need} has no branch held unverified ({}); only an unverified branch is recoverable",
            states.join(", ")
        )));
    }
    Ok(held)
}

/// Print what recovering each held branch would do, reading its remote ref
/// as it is now.
fn print_dry_run(project: &Path, home: &Path, held: &[NeedBranch]) -> Result<(), VivariumError> {
    let ctx = crate::pump::requeue::cli_ctx(project, home)?;
    for record in held {
        let tip = record.observed_unverified_tip_sha.as_deref().unwrap_or("");
        let trusted = record.branch_tip_sha.as_deref().unwrap_or("");
        let remote =
            crate::pump::merge::observed_ref_sha(&ctx, &record.target, &remote_ref(record));
        let verdict = match remote.as_deref() {
            Some(sha) if sha == tip => {
                String::from("the remote ref still sits there, so it would be adopted")
            }
            Some(sha) => format!("the remote ref moved to {sha}, so it would be refused"),
            None => String::from("the remote ref could not be read, so it would be refused"),
        };
        println!(
            "would recover {}/{}: adopt {tip} over trusted tip {trusted} (job {}); {verdict}",
            record.target,
            record.need,
            record.unverified_job_id.as_deref().unwrap_or("?")
        );
    }
    Ok(())
}
