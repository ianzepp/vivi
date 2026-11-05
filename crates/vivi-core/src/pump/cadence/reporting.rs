//! Ordinary need and reply reporting for staged command results.

use crate::mailspace::{Mailspace, SendRequest};
use crate::pump::host::Ctx;
use crate::pump::jobs::{self, rung_result};
use crate::pump::state::State;
use vivi_mail::letter::plain;
use vivi_mail::letter::work::NeedFront;

use super::result::{AWAITING_RECOVERY, AWAITING_RED};
use super::{CadenceState, backoff_secs};

/// Deliver staged command results through ordinary mailspace work letters.
pub fn deliver(
    ctx: &Ctx,
    st: &mut State,
    cadence: &mut CadenceState,
    mailspace: &Mailspace,
    now: u64,
) {
    let names: Vec<String> = cadence.checks.keys().cloned().collect();
    for name in names {
        let Some(entry) = cadence.checks.get_mut(&name) else {
            continue;
        };
        let Some(run) = entry.latest.as_ref() else {
            continue;
        };
        let Some(awaiting) = run.awaiting.as_deref() else {
            continue;
        };
        let Some(job) = st.jobs.get(&run.job) else {
            continue;
        };
        let result = rung_result::parse(job);
        let operation = match awaiting {
            AWAITING_RED => report_red(ctx, mailspace, entry, run, &result),
            AWAITING_RECOVERY => report_green(ctx, mailspace, entry, run),
            _ => continue,
        };
        match operation {
            Ok(need) => {
                if awaiting == AWAITING_RED {
                    if let Some(handle) = need {
                        entry.red_need = Some(handle);
                    }
                }
                if let Some(latest) = entry.latest.as_mut() {
                    latest.awaiting = None;
                    jobs::mark_applied(st, &latest.job, now);
                }
            }
            Err(error) => {
                let attempt = run.attempt.max(1);
                let retry = now.saturating_add(backoff_secs(attempt));
                entry.exception = Some(super::check::CommandException {
                    kind: String::from("cadence-command-reporting"),
                    reason: error.to_string(),
                    at: now,
                    attempt,
                    retry_at: Some(retry),
                });
                ctx.exception("cadence-command-reporting", &name, &error.to_string());
            }
        }
    }
}

fn report_red(
    ctx: &Ctx,
    mailspace: &Mailspace,
    entry: &super::CommandCheck,
    run: &super::check::CommandExecution,
    result: &rung_result::RungResult,
) -> Result<Option<String>, vivi_error::VivariumError> {
    if let Some(handle) = entry.red_need.as_deref() {
        let is_open = need_is_open(mailspace, &run.role, handle)?;
        if is_open {
            let subject = reply_subject(&entry.fingerprint, run);
            let _sent = mailspace.reply(
                handle,
                &run.role,
                vec![ctx.cfg.reply_to()],
                vec![],
                Some(subject),
                result_body(run, result),
            )?;
            return Ok(Some(handle.to_owned()));
        }
    }
    let target = (!run.target.is_empty()).then(|| run.target.clone());
    let done = format!(
        "The exact configured command `{}` passes on target `{}` main `{}`.",
        run.command,
        if run.target.is_empty() {
            "legacy"
        } else {
            &run.target
        },
        run.main
    );
    let front = NeedFront {
        target,
        ..NeedFront::new(vec![done])
    };
    let body = plain::render(&front, &result_body(run, result))
        .map_err(|error| vivi_error::VivariumError::Message(error.to_string()))?;
    let sent = mailspace.send(SendRequest {
        from: run.role.clone(),
        to: vec![ctx.cfg.reply_to()],
        cc: vec![],
        bcc: vec![],
        subject: format!("Command check {} failed", entry.fingerprint),
        body,
        role: run.role.clone(),
        kind: Some(String::from("need")),
        reply_to: None,
    })?;
    Ok(Some(sent.sent))
}

fn report_green(
    ctx: &Ctx,
    mailspace: &Mailspace,
    entry: &super::CommandCheck,
    run: &super::check::CommandExecution,
) -> Result<Option<String>, vivi_error::VivariumError> {
    let Some(handle) = entry.red_need.as_deref() else {
        return Ok(None);
    };
    let is_open = need_is_open(mailspace, &run.role, handle)?;
    if is_open {
        let body = format!(
            "The command check is green again. The runner tested full commit {} for command `{}` on target `{}` main `{}`. This report does not close the need.",
            run.tested_commit.as_deref().unwrap_or("unknown"),
            run.command,
            run.target,
            run.main
        );
        mailspace.reply(
            handle,
            &run.role,
            vec![ctx.cfg.reply_to()],
            vec![],
            Some(reply_subject(&entry.fingerprint, run)),
            body,
        )?;
    }
    Ok(Some(handle.to_owned()))
}

fn need_is_open(
    mailspace: &Mailspace,
    role: &str,
    handle: &str,
) -> Result<bool, vivi_error::VivariumError> {
    let needs = mailspace.list_kind_all(Some(role), "sent", "need")?;
    if !needs.iter().any(|need| need.handle == handle) {
        return Err(vivi_error::VivariumError::Message(format!(
            "recorded command need {handle} cannot be resolved"
        )));
    }
    let state = mailspace.item_event_state(handle)?;
    Ok(state.as_ref().is_none_or(|state| state.state != "done"))
}

fn reply_subject(fingerprint: &str, run: &super::check::CommandExecution) -> String {
    format!("Command check {fingerprint} execution {}", run.job)
}

fn result_body(run: &super::check::CommandExecution, result: &rung_result::RungResult) -> String {
    format!(
        "Role: {}\nTarget: {}\nTarget main: {}\nCommand: {}\nRunner-start commit: {}\nExit: {}\n\nBounded runner output tail:\n{}",
        run.role,
        if run.target.is_empty() {
            "legacy"
        } else {
            &run.target
        },
        run.main,
        run.command,
        run.tested_commit.as_deref().unwrap_or("unknown"),
        run.exit
            .map_or_else(|| "unknown".into(), |exit| exit.to_string()),
        result.tail
    )
}
