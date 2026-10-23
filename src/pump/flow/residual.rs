//! Residual and requirement-gap needs filed from an audit, plus the build auditor collector that replaces the default one.

use std::fs;
use std::path::PathBuf;

use sha2::{Digest, Sha256};

use crate::error::VivariumError;
use crate::pump::board::{NeedRow, parse_rows};
use crate::pump::envelope::correct;
use crate::pump::envelope::review::{
    Auditor, AuditorAnswer, Finding, Requirement, RequirementStatus,
};
use crate::pump::envelope::roles::RoleKind;
use crate::pump::flow::collect::{self, Collected};
use crate::pump::flow::collect_step::{RunCx, StepCollector};
use crate::pump::flow::def::{FAILED, FlowKind, NodeClass};
use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::flow::index::{StepJobRow, StepRow};
use crate::pump::host::{Ctx, shell_quote};
use crate::pump::jobs::{JobStore, PoolJob};

pub(crate) fn register<S: JobStore, M: FlowMail>(registry: &mut Registry<S, M>) {
    registry.collector(
        FlowKind::Build,
        NodeClass::Auditor,
        Box::new(AuditCollector),
    );
}

/// Build auditor steps: the role collector, plus the residual and gap needs
/// the audit found are filed before the answer routes.
pub(crate) struct AuditCollector;

impl<S: JobStore, M: FlowMail> StepCollector<S, M> for AuditCollector {
    fn collect(
        &self,
        cx: &mut RunCx<'_, S, M>,
        job: &PoolJob,
        row: &StepJobRow,
        step: &StepRow,
        class: &NodeClass,
    ) -> Result<(), VivariumError> {
        let expected = job.publish_ref.clone().unwrap_or_default();
        match collect::collect(cx.ctx, cx.mail, job, row, class, &expected) {
            Collected::Outcome(word, payload) => {
                let front: Auditor = serde_json::from_value(payload["front"].clone())
                    .map_err(|e| VivariumError::Parse(format!("auditor envelope: {e}")))?;
                file(cx.ctx, &cx.run.need, cx.run.run, &step.task, &front)?;
                cx.advance_word(step, &word, payload)
            }
            Collected::Failed => cx.advance_word(step, FAILED, serde_json::json!({"job": job.id})),
            Collected::Verify => cx.verify_step(job, step),
            Collected::Retry if job.kind == correct::KIND => {
                cx.advance_word(step, FAILED, serde_json::json!({"job": job.id}))
            }
            Collected::Retry => cx.rerun(job, row, step),
            Collected::Correct { error } => {
                let kind = RoleKind::from_class(class).ok_or_else(|| {
                    VivariumError::Parse(format!("node {} has no role kind", step.node))
                })?;
                cx.correct(job, row, kind, &error)
            }
        }
    }
}

/// File one need per requirement gap (any answer) and, when the answer is
/// `residual`, one per finding. Returns the handles in filing order. A repeat
/// with the same keys finds the existing need by subject and files nothing new.
pub(crate) fn file(
    ctx: &Ctx,
    need: &str,
    run: u32,
    audit_task: &str,
    front: &Auditor,
) -> Result<Vec<String>, VivariumError> {
    let base = format!("{need} r{run} {audit_task}");
    let recipient = ordinary_recipient(ctx);
    let mut handles = Vec::new();
    for requirement in front.requirement.iter().filter(|r| is_gap(r.status)) {
        let key = format!("{base}:requirement:{}", requirement.id);
        let subject = format!("pump: audit requirement gap {need} {}", short_hash(&key));
        let body = requirement_body(need, run, audit_task, requirement);
        handles.push(ensure_need(ctx, &key, &subject, &recipient, &body)?);
    }
    if front.answer == AuditorAnswer::Residual {
        for finding in &front.finding {
            let key = format!("{base}:finding:{}", finding.id);
            let subject = format!("pump: audit finding {need} {}", short_hash(&key));
            let body = finding_body(need, run, audit_task, finding);
            handles.push(ensure_need(ctx, &key, &subject, &recipient, &body)?);
        }
    }
    Ok(handles)
}

fn is_gap(status: RequirementStatus) -> bool {
    matches!(
        status,
        RequirementStatus::Partial | RequirementStatus::Missing | RequirementStatus::Unverified
    )
}

fn status_label(status: RequirementStatus) -> &'static str {
    match status {
        RequirementStatus::Satisfied => "SATISFIED",
        RequirementStatus::Partial => "PARTIAL",
        RequirementStatus::Missing => "MISSING",
        RequirementStatus::Unverified => "UNVERIFIED",
        RequirementStatus::NotApplicable => "NOT_APPLICABLE",
    }
}

fn requirement_body(need: &str, run: u32, audit_task: &str, requirement: &Requirement) -> String {
    let content = format!(
        "Declared requirement {} was {}: {}",
        requirement.id,
        status_label(requirement.status),
        requirement.text,
    );
    let trigger = format!(
        "Recheck declared requirement {} against the need branch and record evidence.",
        requirement.id
    );
    let mut body = body_head(need, run, audit_task, &requirement.id, &content);
    body.push_str("Evidence:\n");
    append_bullets(&mut body, &requirement.evidence);
    body_tail(body, &trigger)
}

fn finding_body(need: &str, run: u32, audit_task: &str, finding: &Finding) -> String {
    let mut body = body_head(need, run, audit_task, &finding.id, &finding.content);
    body.push_str("Affected paths:\n");
    append_bullets(&mut body, &finding.paths);
    body_tail(body, &finding.recheck_trigger)
}

fn body_head(need: &str, run: u32, audit_task: &str, id: &str, content: &str) -> String {
    format!(
        "Audit follow-up for need {}.\nRun: r{run}\nAudit task: {}\nFinding or requirement: {}\n{}\n",
        one_line(need),
        one_line(audit_task),
        one_line(id),
        one_line(content),
    )
}

fn body_tail(mut body: String, recheck_trigger: &str) -> String {
    let trigger = one_line(recheck_trigger);
    body.push_str(&format!(
        "\nRecheck trigger: {trigger}\n\ndone_when: {trigger}\n"
    ));
    body
}

fn append_bullets(out: &mut String, items: &[String]) {
    if items.is_empty() {
        out.push_str("- none recorded\n");
    } else {
        for item in items {
            out.push_str(&format!("- {}\n", one_line(item)));
        }
    }
}

fn one_line(text: &str) -> String {
    text.chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn ordinary_recipient(ctx: &Ctx) -> String {
    let configured = ctx.cfg.reply_to();
    if configured.is_empty() {
        String::from("operator")
    } else {
        configured
    }
}

fn short_hash(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    hex::encode(&digest[..10])
}

fn need_body_path(ctx: &Ctx, key: &str) -> PathBuf {
    ctx.home
        .join("need-audit")
        .join(format!("{}.txt", short_hash(key)))
}

fn ensure_need(
    ctx: &Ctx,
    key: &str,
    subject: &str,
    recipient: &str,
    body: &str,
) -> Result<String, VivariumError> {
    if let Some(handle) = find_outgoing_need(ctx, subject)? {
        return Ok(handle);
    }
    let path = need_body_path(ctx, key);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, body)?;
    let mut command = format!("need send --from {}", shell_quote(&ctx.cfg.reply_from));
    if !recipient.is_empty() {
        command.push_str(&format!(" --to {}", shell_quote(recipient)));
    }
    command.push_str(&format!(
        " --subject {} --body-file {}",
        shell_quote(subject),
        shell_quote(&path.to_string_lossy()),
    ));
    let output = ctx.vivi(&command, 60);
    if output.code != 0 {
        return Err(VivariumError::Message(format!(
            "audit work could not be filed ({subject}): {}",
            tail(&output.text, 200)
        )));
    }
    parse_need_handle(&output.text).ok_or_else(|| {
        VivariumError::Message(format!(
            "audit work could not be filed ({subject}): {}",
            tail(&output.text, 200)
        ))
    })
}

fn find_outgoing_need(ctx: &Ctx, subject: &str) -> Result<Option<String>, VivariumError> {
    let command = format!(
        "need list --status all --json --from {}",
        shell_quote(&ctx.cfg.reply_from)
    );
    let output = ctx.vivi(&command, 60);
    if output.code != 0 {
        return Err(VivariumError::Message(format!(
            "cannot deduplicate audit work: {}",
            tail(&output.text, 160)
        )));
    }
    let rows = parse_rows(&output.text).ok_or_else(|| {
        VivariumError::Message(format!(
            "cannot parse audit need list: {}",
            tail(&output.text, 160)
        ))
    })?;
    Ok(rows
        .into_iter()
        .find(|row: &NeedRow| row.subject == subject)
        .map(|row| row.handle))
}

fn parse_need_handle(output: &str) -> Option<String> {
    let token = output
        .lines()
        .find(|line| line.starts_with("created "))
        .and_then(|line| line.split_whitespace().nth(2))
        .or_else(|| {
            output
                .lines()
                .find(|line| line.starts_with("sent "))
                .and_then(|line| line.split_whitespace().nth(1))
        });
    token
        .map(ToOwned::to_owned)
        .filter(|handle| !handle.is_empty() && !handle.chars().any(char::is_whitespace))
}

fn tail(text: &str, max: usize) -> String {
    text.trim().chars().take(max).collect()
}

#[cfg(test)]
#[path = "residual_test.rs"]
mod tests;
