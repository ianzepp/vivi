//! Ordinary-mail reporting for staged command-check failures.

use crate::mailspace::{Mailspace, SendRequest};
use crate::pump::host::Ctx;
use crate::pump::jobs::{self, rung_result};
use crate::pump::state::State;
use vivi_mail::letter::plain;
use vivi_mail::letter::work::NeedFront;

use super::result::{self, AWAITING_RED};
use super::{CadenceState, EXTRA_KEY};

/// Apply staged command reports after the mailspace is open. A failed effect
/// leaves the execution staged so its shell slot remains held.
pub(super) fn report(
    ctx: &Ctx,
    st: &mut State,
    cadence: &mut CadenceState,
    mail: &Mailspace,
    now: u64,
) {
    let names: Vec<String> = cadence.checks.keys().cloned().collect();
    for name in names {
        if let Err(error) = report_role(ctx, st, cadence, mail, &name, now) {
            let reason = error.to_string();
            ctx.exception("cadence-command-report", &name, &reason);
        }
    }
}

fn report_role(
    ctx: &Ctx,
    st: &mut State,
    cadence: &mut CadenceState,
    mail: &Mailspace,
    name: &str,
    now: u64,
) -> Result<(), vivi_error::VivariumError> {
    let Some(check) = cadence.checks.get(name) else {
        return Ok(());
    };
    let Some(execution) = check
        .latest
        .as_ref()
        .filter(|run| run.awaiting.as_deref() == Some(AWAITING_RED))
        .cloned()
    else {
        return Ok(());
    };
    if let Some(root) = cadence
        .checks
        .get(name)
        .and_then(|check| check.red_need.clone())
    {
        reply_red(ctx, st, cadence, mail, name, &execution, &root, now)?;
    } else {
        open_red(ctx, st, cadence, mail, name, &execution, now)?;
    }
    Ok(())
}

fn open_red(
    ctx: &Ctx,
    st: &mut State,
    cadence: &mut CadenceState,
    mail: &Mailspace,
    name: &str,
    run: &super::CommandExecution,
    now: u64,
) -> Result<(), vivi_error::VivariumError> {
    let target = (!run.target.is_empty()).then(|| run.target.clone());
    let done = format!(
        "Saved command for role {} must pass on target {} main {}.",
        run.role, run.target, run.main
    );
    let front = NeedFront {
        target,
        ..NeedFront::new(vec![done])
    };
    let rung = st
        .jobs
        .get(&run.job)
        .map(rung_result::parse)
        .ok_or_else(|| vivi_error::VivariumError::Message("staged command job missing".into()))?;
    let prose = red_text(run, &rung.tail);
    let body = plain::render(&front, &prose)
        .map_err(|error| vivi_error::VivariumError::Message(error.to_string()))?;
    let request = SendRequest {
        from: run.role.clone(),
        to: vec![ctx.cfg.reply_to()],
        cc: Vec::new(),
        bcc: Vec::new(),
        subject: root_subject(&run.fingerprint),
        body,
        role: "needs".into(),
        kind: Some("need".into()),
        reply_to: None,
    };
    let sent = mail.send(request)?;
    let root = sent
        .delivered
        .first()
        .map(|item| item.handle.clone())
        .unwrap_or(sent.sent);
    let check = cadence
        .checks
        .get_mut(name)
        .ok_or_else(|| vivi_error::VivariumError::Message("command check disappeared".into()))?;
    check.red_need = Some(root.clone());
    if let Some(latest) = check.latest.as_mut() {
        latest.evidence = Some(format!("{root}:{}", result_marker(run)));
        latest.awaiting = None;
    }
    if !run.job.is_empty() {
        jobs::mark_applied(st, &run.job, now);
    }
    Ok(())
}

fn reply_red(
    ctx: &Ctx,
    st: &mut State,
    cadence: &mut CadenceState,
    mail: &Mailspace,
    name: &str,
    run: &super::CommandExecution,
    root: &str,
    now: u64,
) -> Result<(), vivi_error::VivariumError> {
    let events = mail.item_event_state(root)?;
    if events.is_some_and(|event| event.state != "open") {
        return Ok(());
    }
    let rung = st
        .jobs
        .get(&run.job)
        .map(rung_result::parse)
        .ok_or_else(|| vivi_error::VivariumError::Message("staged command job missing".into()))?;
    let marker = result_marker(run);
    let body = format!("[{}]\n{}", marker, red_text(run, &rung.tail));
    mail.reply(
        root,
        &run.role,
        vec![ctx.cfg.reply_to()],
        Vec::new(),
        Some(format!("{} [{}]", root_subject(&run.fingerprint), marker)),
        body,
    )?;
    let check = cadence
        .checks
        .get_mut(name)
        .ok_or_else(|| vivi_error::VivariumError::Message("command check disappeared".into()))?;
    if let Some(latest) = check.latest.as_mut() {
        latest.evidence = Some(format!("{root}:{marker}"));
        latest.awaiting = None;
    }
    jobs::mark_applied(st, &run.job, now);
    Ok(())
}

fn root_subject(fingerprint: &str) -> String {
    format!("command check {}", fingerprint)
}
fn result_marker(run: &super::CommandExecution) -> String {
    format!("result-{}", run.job)
}
fn red_text(run: &super::CommandExecution, tail: &str) -> String {
    format!(
        "Command check failed.\nrole: {}\ntarget: {}\ncommand: {}\ntested: {}\nexit: {}\n\n{}",
        run.role,
        run.target,
        run.command,
        run.tested_commit.as_deref().unwrap_or("unknown"),
        run.exit
            .map_or_else(|| "unknown".into(), |code| code.to_string()),
        tail
    )
}

#[allow(dead_code)]
fn state_key() -> &'static str {
    EXTRA_KEY
}
#[allow(dead_code)]
fn _state_type(_: &State) {}
#[allow(dead_code)]
fn _result(_: &result::TipGate) {}
