//! Ordinary backlog reporting for staged command failures.

use crate::mailspace::{Mailspace, SendRequest};
use crate::pump::host::Ctx;
use crate::pump::jobs::{self, rung_result};
use crate::pump::state::State;
use vivi_mail::letter::plain::render;
use vivi_mail::letter::work::NeedFront;

use super::check::{CommandException, RED};
use super::{CadenceState, backoff_secs};

const REPORT_EXCEPTION: &str = "cadence-command-report";

/// Apply staged red executions only after their ordinary need/reply effect succeeds.
pub fn report(
    ctx: &Ctx,
    st: &mut State,
    cadence: &mut CadenceState,
    mailspace: &Mailspace,
    now: u64,
) {
    let roles: Vec<String> = cadence.checks.keys().cloned().collect();
    for role in roles {
        let Some(entry) = cadence.checks.get_mut(&role) else {
            continue;
        };
        let Some(execution) = entry.latest.as_mut().filter(|run| {
            run.awaiting.as_deref() == Some("red") && run.outcome.as_deref() == Some(RED)
        }) else {
            continue;
        };
        let Some(job) = st.jobs.get(&execution.job) else {
            continue;
        };
        let tail = rung_result::parse(job).tail;
        let marker = format!(
            "[cadence-result:{}:{}]",
            execution.fingerprint, execution.job
        );
        let subject = if entry.red_need.is_some() {
            format!("cadence result {}", execution.job)
        } else {
            root_subject(&execution.fingerprint)
        };
        let body = format!(
            "{marker}\nRole: {}\nTarget: {}\nCommand: {}\nTested commit: {}\nExit: {}\n\nOutput tail:\n{}",
            execution.role,
            execution.target,
            execution.command,
            execution.tested_commit.as_deref().unwrap_or("unknown"),
            execution
                .exit
                .map_or_else(|| "unknown".into(), |x| x.to_string()),
            tail
        );
        let effect = if let Some(need) = entry.red_need.as_deref() {
            let result = mailspace.reply(
                need,
                &execution.role,
                vec![ctx.cfg.reply_to().to_string()],
                Vec::new(),
                Some(subject),
                body,
            );
            result.map(|_| ())
        } else {
            let mut front = NeedFront::new(vec![format!(
                "{} passes on {} main",
                execution.command, execution.target
            )]);
            front.target = (!execution.target.is_empty()).then(|| execution.target.clone());
            let letter = render(&front, &format!("Command check failed.\n\n{body}"))
                .map_err(|error| vivi_error::VivariumError::Message(error.to_string()));
            letter
                .and_then(|letter| {
                    mailspace
                        .send(SendRequest {
                            from: execution.role.clone(),
                            to: vec![ctx.cfg.reply_to().to_string()],
                            cc: Vec::new(),
                            bcc: Vec::new(),
                            subject: subject.clone(),
                            body: letter,
                            role: "needs".into(),
                            kind: Some("need".into()),
                            reply_to: None,
                        })
                        .map(|sent| {
                            sent.delivered
                                .first()
                                .map(|d| d.handle.clone())
                                .unwrap_or(sent.sent)
                        })
                })
                .map(|need| {
                    entry.red_need = Some(need);
                })
        };
        match effect {
            Ok(()) => {
                execution.evidence = Some(marker);
                execution.awaiting = None;
                jobs::mark_applied(st, &execution.job, now);
            }
            Err(error) => {
                let attempt = entry
                    .exception
                    .as_ref()
                    .map_or(1, |e| e.attempt.saturating_add(1));
                entry.exception = Some(CommandException {
                    kind: REPORT_EXCEPTION.into(),
                    reason: error.to_string(),
                    at: now,
                    attempt,
                    retry_at: Some(now.saturating_add(backoff_secs(attempt))),
                });
                ctx.exception(REPORT_EXCEPTION, &execution.role, &error.to_string());
            }
        }
    }
}

/// The stable root subject marker used to recover a filed command need.
#[allow(dead_code)]
fn root_subject(fingerprint: &str) -> String {
    format!("cadence-check-{fingerprint}")
}
