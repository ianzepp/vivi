use crate::mailspace::{Mailspace, SendRequest};
use crate::pump::host::Ctx;
use crate::pump::jobs::{self, rung_result};
use crate::pump::state::State;
use vivi_mail::letter::fixture::need_body_with;
use vivi_mail::letter::work::NeedFront;

use super::check::CommandExecution;
use super::{CadenceState, backoff_secs};

/// Deliver staged genuine command outcomes before allowing their jobs to settle.
pub fn collect(ctx: &Ctx, st: &mut State, cadence: &mut CadenceState, mail: &Mailspace, now: u64) {
    let names: Vec<String> = cadence.checks.keys().cloned().collect();
    for name in names {
        let Some(execution) = cadence.checks.get(&name).and_then(|c| c.latest.clone()) else {
            continue;
        };
        let Some(awaiting) = execution.awaiting.as_deref() else {
            continue;
        };
        let sent = match awaiting {
            super::result::AWAITING_RED => report_red(ctx, st, mail, &name, &execution),
            super::result::AWAITING_RECOVERY => report_recovery(ctx, st, mail, &name, &execution),
            _ => continue,
        };
        match sent {
            Ok(need) => {
                let check = cadence.checks.get_mut(&name).expect("check exists");
                if let Some(handle) = need {
                    check.red_need = Some(handle);
                }
                if let Some(latest) = check.latest.as_mut() {
                    latest.awaiting = None;
                }
                if awaiting == super::result::AWAITING_RECOVERY {
                    check.last_green_commit.clone_from(&execution.tested_commit);
                }
                jobs::mark_applied(st, &execution.job, now);
            }
            Err(error) => {
                ctx.exception("cadence-command-reporting", &name, &error.to_string());
                let check = cadence.checks.get_mut(&name).expect("check exists");
                check.exception = Some(super::CommandException {
                    kind: "cadence-command-reporting".into(),
                    reason: error.to_string(),
                    at: now,
                    attempt: execution.attempt.max(1),
                    retry_at: Some(now.saturating_add(backoff_secs(execution.attempt.max(1)))),
                });
            }
        }
    }
}

fn report_red(
    ctx: &Ctx,
    st: &State,
    mail: &Mailspace,
    role: &str,
    run: &CommandExecution,
) -> Result<Option<String>, vivi_error::VivariumError> {
    let check = super::state(st)
        .checks
        .get(role)
        .cloned()
        .unwrap_or_default();
    if let Some(handle) = check.red_need.as_deref() {
        match mail.item_event_state(handle)? {
            Some(state) if state.state != "done" && state.state != "withdrawn" => {
                mail.reply(
                    handle,
                    role,
                    vec![ctx.cfg.reply_to()],
                    Vec::new(),
                    Some(format!("command result {}", run.job)),
                    evidence(st, run),
                )?;
                return Ok(None);
            }
            None => {
                return Err(vivi_error::VivariumError::Message(
                    "recorded command need cannot be resolved".into(),
                ));
            }
            _ => {}
        }
    }
    let target = (!run.target.is_empty()).then(|| run.target.clone());
    let front = NeedFront {
        target,
        ..NeedFront::new(vec![format!(
            "Configured command `{}` passes on the main of target `{}`.",
            run.command,
            if run.target.is_empty() {
                "default"
            } else {
                &run.target
            }
        )])
    };
    let body = need_body_with(
        &front,
        &format!(
            "Command check for role `{}` failed. Target: `{}`; target main: `{}`. {}",
            role,
            run.target,
            run.main,
            evidence(st, run)
        ),
    );
    let sent = mail.send(SendRequest {
        from: role.into(),
        to: vec![ctx.cfg.reply_to()],
        cc: Vec::new(),
        bcc: Vec::new(),
        subject: format!("command check {}/{}", run.fingerprint, run.job),
        body,
        role: "needs".into(),
        kind: Some("need".into()),
        reply_to: None,
    })?;
    Ok(sent
        .delivered
        .into_iter()
        .next()
        .map(|message| message.handle)
        .or(Some(sent.sent)))
}

fn report_recovery(
    ctx: &Ctx,
    st: &State,
    mail: &Mailspace,
    role: &str,
    run: &CommandExecution,
) -> Result<Option<String>, vivi_error::VivariumError> {
    let check = super::state(st)
        .checks
        .get(role)
        .cloned()
        .unwrap_or_default();
    let handle = check.red_need.as_deref().ok_or_else(|| {
        vivi_error::VivariumError::Message("recovery has no recorded red need".into())
    })?;
    match mail.item_event_state(handle)? {
        Some(state) if state.state != "done" && state.state != "withdrawn" => {
            mail.reply(
                handle,
                role,
                vec![ctx.cfg.reply_to()],
                Vec::new(),
                Some(format!("command recovery {}", run.job)),
                format!(
                    "The configured command is green again. Full tested commit: {}.",
                    run.tested_commit.as_deref().unwrap_or("unknown")
                ),
            )?;
            Ok(None)
        }
        Some(_) => Ok(None),
        None => Err(vivi_error::VivariumError::Message(
            "recorded command need cannot be resolved".into(),
        )),
    }
}

fn evidence(st: &State, run: &CommandExecution) -> String {
    let tail = st
        .jobs
        .get(&run.job)
        .map(rung_result::parse)
        .map(|r| r.tail)
        .unwrap_or_default();
    let tail: String = tail
        .chars()
        .rev()
        .take(2000)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!(
        "Exact command: `{}`. Runner-start commit: `{}`. Exit: {}. Output tail:\n{}",
        run.command,
        run.tested_commit.as_deref().unwrap_or("unknown"),
        run.exit.map_or_else(|| "unknown".into(), |v| v.to_string()),
        tail
    )
}
