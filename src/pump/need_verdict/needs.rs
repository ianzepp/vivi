//! Idempotent mailspace needs filed from audit findings and holds.

use std::fs;

use sha2::{Digest, Sha256};

use crate::error::VivariumError;
use crate::pump::board::{NeedRow, parse_rows};
use crate::pump::host::{Ctx, shell_quote};
use crate::pump::need_audit::{NeedAuditReceipt, NeedAuditVerdict, NeedRequirementStatus};

use super::NeedVerdictState;

pub(super) fn route_requirement_gaps(
    ctx: &Ctx,
    state: &mut NeedVerdictState,
    job_id: &str,
    receipt: &NeedAuditReceipt,
) -> Result<(), VivariumError> {
    for requirement in &receipt.requirements {
        if matches!(
            requirement.status,
            NeedRequirementStatus::Satisfied | NeedRequirementStatus::NotApplicable
        ) {
            continue;
        }
        let key = format!("{}:requirement:{}", receipt.receipt_key, requirement.id);
        let evidence = requirement.evidence.join("\n- ");
        let content = format!(
            "Declared requirement {} was {}: {}\nEvidence: {}",
            requirement.id,
            requirement_status_label(requirement.status),
            requirement.text,
            if evidence.is_empty() {
                "none recorded"
            } else {
                &evidence
            },
        );
        let trigger = format!(
            "Recheck declared requirement {} against the frozen need range and record evidence.",
            requirement.id
        );
        let body = finding_need_body(job_id, receipt, &requirement.id, &content, &[], &trigger);
        let handle = ensure_need(
            ctx,
            &key,
            &format!(
                "pump: audit requirement gap {} {}",
                receipt.binding.need,
                short_hash(&key)
            ),
            &[ordinary_recipient(ctx)],
            &body,
        )?;
        state.residual_needs.insert(key, handle);
    }
    Ok(())
}

pub(super) fn ensure_need(
    ctx: &Ctx,
    key: &str,
    subject: &str,
    recipients: &[String],
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
    for recipient in recipients {
        if !recipient.is_empty() {
            command.push_str(&format!(" --to {}", shell_quote(recipient)));
        }
    }
    command.push_str(&format!(
        " --subject {} --body-file {}",
        shell_quote(subject),
        shell_quote(&path.to_string_lossy()),
    ));
    let output = ctx.vivi(&command, 60);
    if output.code != 0 {
        return Err(VivariumError::Message(format!(
            "need-audit work could not be filed ({subject}): {}",
            tail(&output.text, 200)
        )));
    }
    parse_need_handle(&output.text).ok_or_else(|| {
        VivariumError::Message(format!(
            "need-audit work could not be filed ({subject}): {}",
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
            "cannot deduplicate need-audit work: {}",
            tail(&output.text, 160)
        )));
    }
    let rows = parse_rows(&output.text).ok_or_else(|| {
        VivariumError::Message(format!(
            "cannot parse need-audit need list: {}",
            tail(&output.text, 160)
        ))
    })?;
    Ok(rows
        .into_iter()
        .find(|row: &NeedRow| row.subject == subject)
        .map(|row| row.handle))
}

pub(super) fn ensure_escalation(
    ctx: &Ctx,
    receipt: &NeedAuditReceipt,
    reason: &str,
    boundary: &[String],
) -> Result<String, VivariumError> {
    let key = format!("{}:escalation", receipt.receipt_key);
    let subject = format!(
        "pump: need audit hold {} {}",
        receipt.binding.need,
        short_hash(&key)
    );
    let mut body = format!(
        "The frozen need branch is held for operator and head-cto ruling.\nNeed: {}\nVerdict: {}\nAudit handle: {}\nReport: {}\nFull range: {}..{}\nReview range: {}..{}\nReason: {}\nBoundary:\n",
        one_line(&receipt.binding.need),
        verdict_label(receipt.verdict),
        one_line(report_task(receipt)),
        one_line(&receipt.report.reference),
        receipt.binding.base,
        receipt.binding.tip,
        receipt.audit_base,
        receipt.binding.tip,
        one_line(reason),
    );
    append_bullets(&mut body, boundary);
    append_findings(&mut body, receipt);
    body.push_str(
        "\n\ndone_when: The operator and head-cto record a disposition for this held need range.\n",
    );
    ensure_need(
        ctx,
        &key,
        &subject,
        &[String::from("operator"), String::from("head-cto")],
        &body,
    )
}

fn append_findings(out: &mut String, receipt: &NeedAuditReceipt) {
    out.push_str("\nFindings:\n");
    for finding in &receipt.findings {
        out.push_str(&format!(
            "- {}: {} (paths: {}; recheck: {})\n",
            one_line(&finding.id),
            one_line(&finding.content),
            finding
                .paths
                .iter()
                .map(|path| one_line(path))
                .collect::<Vec<_>>()
                .join(", "),
            one_line(&finding.recheck_trigger),
        ));
    }
}

pub(super) fn finding_need_body(
    job_id: &str,
    receipt: &NeedAuditReceipt,
    finding_id: &str,
    content: &str,
    paths: &[String],
    recheck_trigger: &str,
) -> String {
    let mut body = format!(
        "Audit follow-up for need {}.\nAudit handle: {}\nAudit job: {job_id}\nReport: {}\nVerdict: {}\nFull frozen range: {}..{}\nReview range: {}..{}\nFinding or requirement: {finding_id}\n{}\nAffected paths:\n",
        one_line(&receipt.binding.need),
        one_line(report_task(receipt)),
        one_line(&receipt.report.reference),
        verdict_label(receipt.verdict),
        receipt.binding.base,
        receipt.binding.tip,
        receipt.audit_base,
        receipt.binding.tip,
        one_line(content),
    );
    append_bullets(&mut body, paths);
    let trigger = one_line(recheck_trigger);
    body.push_str(&format!(
        "\nRecheck trigger: {trigger}\n\ndone_when: {trigger}\n"
    ));
    body
}

pub(super) fn ordinary_recipient(ctx: &Ctx) -> String {
    let configured = ctx.cfg.reply_to();
    if configured.is_empty() {
        String::from("operator")
    } else {
        configured
    }
}

pub(super) fn requirement_status_label(status: NeedRequirementStatus) -> &'static str {
    match status {
        NeedRequirementStatus::Satisfied => "SATISFIED",
        NeedRequirementStatus::Partial => "PARTIAL",
        NeedRequirementStatus::Missing => "MISSING",
        NeedRequirementStatus::Unverified => "UNVERIFIED",
        NeedRequirementStatus::NotApplicable => "NOT_APPLICABLE",
    }
}

pub(super) fn short_hash(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    hex::encode(&digest[..10])
}

fn need_body_path(ctx: &Ctx, key: &str) -> std::path::PathBuf {
    ctx.home
        .join("need-audit")
        .join(format!("{}.txt", short_hash(key)))
}

pub(super) fn report_task(receipt: &NeedAuditReceipt) -> &str {
    receipt.report.task.as_deref().unwrap_or(&receipt.job_id)
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

pub(super) fn append_bullets(out: &mut String, items: &[String]) {
    if items.is_empty() {
        out.push_str("- none recorded\n");
    } else {
        for item in items {
            out.push_str(&format!("- {}\n", one_line(item)));
        }
    }
}

pub(super) fn one_line(text: &str) -> String {
    text.chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn verdict_label(verdict: NeedAuditVerdict) -> &'static str {
    match verdict {
        NeedAuditVerdict::CleanPass => "clean_pass",
        NeedAuditVerdict::Residual => "residual",
        NeedAuditVerdict::BlockRange => "block_range",
        NeedAuditVerdict::BlockShip => "block_ship",
    }
}

fn tail(text: &str, max: usize) -> String {
    text.trim().chars().take(max).collect()
}
