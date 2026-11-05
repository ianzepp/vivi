//! Pure validation of role replies against a small reply contract.

use serde::{Deserialize, Serialize};
use vivi_mail::letter::LetterError;
use vivi_mail::letter::roles::{Parsed, RoleKind, parse_for};

use super::context::{ReplyContract, SlicerRequirements};

/// Starting run for the head-CTO `new_fact` constraint.
pub const NEW_FACT_RUN: u32 = 3;

/// A content contradiction identified independently of its prose diagnosis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationIssue {
    /// Stable machine-readable issue name.
    pub name: String,
    /// Human-readable detail suitable for a correction prompt.
    pub detail: String,
}

/// A refusal retains the parser's exact error variant or named content issues.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    Envelope(LetterError),
    Issues(Vec<ValidationIssue>),
}

/// Parse result returned by [`validate_reply`].
pub type ValidationResult = Result<Parsed, Refusal>;

/// Parse the original reply text and apply pure, contextual checks.
pub fn validate_reply(contract: &ReplyContract, text: &str) -> ValidationResult {
    let Some(role) = contract.role_kind() else {
        return Err(Refusal::Issues(vec![issue(
            "unknown_role",
            format!("role {:?} is not a canonical role name", contract.role),
        )]));
    };
    let parsed = parse_for(role, text).map_err(Refusal::Envelope)?;
    let issues = contract_issues(contract, role, &parsed);
    if issues.is_empty() {
        Ok(parsed)
    } else {
        Err(Refusal::Issues(issues))
    }
}

/// Shared underlying diagnostic formatter; pump-specific evidence is added elsewhere.
#[must_use]
pub fn diagnostic(error: &LetterError) -> String {
    error.to_string()
}

/// Step answer and run-sensitive constraints shared with the collector.
#[must_use]
pub fn step_issues(
    answer: &str,
    front: &serde_json::Value,
    allowed: &[&str],
    run: u32,
    role: RoleKind,
) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();
    if !allowed.contains(&answer) {
        issues.push(issue(
            "answer_not_allowed",
            format!("answer {answer:?} is not one of this step's words"),
        ));
    }
    if role == RoleKind::Cto && cto_needs_fact(answer, front, run) {
        issues.push(issue(
            "cto_new_fact_required",
            format!("run {run}: every head-cto answer but `lock` needs a non-empty `new_fact` (expected a new fact since the last run; actual: none)"),
        ));
    }
    issues
}

/// Whether this CTO answer needs a non-blank `new_fact` on this run.
#[must_use]
pub fn cto_needs_fact(answer: &str, front: &serde_json::Value, run: u32) -> bool {
    run >= NEW_FACT_RUN
        && answer != "lock"
        && !front
            .get("new_fact")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|fact| !fact.trim().is_empty())
}

/// Failing check commands that contradict a hand's `built` answer.
#[must_use]
pub fn failing_hand_checks<'a>(
    answer: &str,
    checks: impl IntoIterator<Item = (&'a str, bool)>,
) -> Vec<String> {
    if answer != "built" {
        return Vec::new();
    }
    checks
        .into_iter()
        .filter(|(_, failed)| *failed)
        .map(|(command, _)| command.to_owned())
        .collect()
}

fn contract_issues(
    contract: &ReplyContract,
    role: RoleKind,
    parsed: &Parsed,
) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();
    if let Some(answers) = &contract.answers
        && !answers.contains(&parsed.answer)
    {
        issues.push(issue(
            "answer_not_allowed",
            format!(
                "answer {:?} is not allowed by this reply contract",
                parsed.answer
            ),
        ));
    }
    if role == RoleKind::Cto
        && contract
            .run
            .is_some_and(|run| cto_needs_fact(&parsed.answer, &parsed.front, run))
    {
        issues.push(issue(
            "cto_new_fact_required",
            format!("run {}: every head-cto answer but `lock` needs a non-empty `new_fact` (expected a new fact since the last run; actual: none)", contract.run.unwrap_or_default()),
        ));
    }
    if role == RoleKind::Hand {
        let checks = parsed
            .front
            .get("check")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|check| {
                Some((
                    check.get("cmd")?.as_str()?,
                    check.get("result")?.as_str()? == "fail",
                ))
            });
        issues.extend(
            failing_hand_checks(&parsed.answer, checks)
                .into_iter()
                .map(|command| {
                    issue(
                        "built_with_failed_checks",
                        format!("built answer reports failing check: {command}"),
                    )
                }),
        );
    }
    if role == RoleKind::Slicer
        && let Some(requirements) = &contract.slicer
    {
        slicer_issues(requirements, &parsed.front, &mut issues);
    }
    issues
}

fn slicer_issues(
    requirements: &SlicerRequirements,
    front: &serde_json::Value,
    issues: &mut Vec<ValidationIssue>,
) {
    check_slicer_mode(requirements, front, issues);
    let Some(slices) = front.get("slice").and_then(serde_json::Value::as_array) else {
        return;
    };
    let ids: Vec<&str> = slices
        .iter()
        .filter_map(|slice| slice.get("id").and_then(serde_json::Value::as_str))
        .collect();
    for slice in slices {
        validate_slice(requirements, slice, &ids, issues);
    }
}

fn check_slicer_mode(
    requirements: &SlicerRequirements,
    front: &serde_json::Value,
    issues: &mut Vec<ValidationIssue>,
) {
    if let Some(mode) = requirements.mode
        && front.get("mode").and_then(serde_json::Value::as_str) != Some(mode_name(mode))
    {
        issues.push(issue(
            "slicer_mode_mismatch",
            "slicer mode differs from the requested mode",
        ));
    }
}

fn validate_slice(
    requirements: &SlicerRequirements,
    slice: &serde_json::Value,
    ids: &[&str],
    issues: &mut Vec<ValidationIssue>,
) {
    check_slice_target(requirements, slice, issues);
    if requirements.check_scope
        && slice
            .get("write_scope")
            .and_then(serde_json::Value::as_array)
            .is_none_or(Vec::is_empty)
    {
        issues.push(issue(
            "slicer_scope_empty",
            "slice requires a non-empty write_scope",
        ));
    }
    check_dependencies(requirements, slice, ids, issues);
}

fn check_slice_target(
    requirements: &SlicerRequirements,
    slice: &serde_json::Value,
    issues: &mut Vec<ValidationIssue>,
) {
    let target = slice.get("target").and_then(serde_json::Value::as_str);
    if requirements.check_targets
        && let (Some(target), Some(configured)) = (target, &requirements.configured_targets)
        && !configured.iter().any(|configured| configured == target)
    {
        issues.push(issue(
            "slicer_target_unconfigured",
            format!("slice target {target:?} is not configured"),
        ));
    }
}

fn check_dependencies(
    requirements: &SlicerRequirements,
    slice: &serde_json::Value,
    ids: &[&str],
    issues: &mut Vec<ValidationIssue>,
) {
    let Some(earlier) = &requirements.earlier_slice_ids else {
        return;
    };
    for dependency in slice
        .get("deps")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
    {
        if !ids.contains(&dependency) && !earlier.iter().any(|id| id == dependency) {
            issues.push(issue(
                "slicer_dependency_unknown",
                format!("slice dependency {dependency:?} is not in this or an earlier slice"),
            ));
        }
    }
}

fn mode_name(mode: vivi_mail::letter::roles::SlicerMode) -> &'static str {
    match mode {
        vivi_mail::letter::roles::SlicerMode::Slice => "slice",
        vivi_mail::letter::roles::SlicerMode::Rework => "rework",
        vivi_mail::letter::roles::SlicerMode::Split => "split",
    }
}

fn issue(name: &str, detail: impl Into<String>) -> ValidationIssue {
    ValidationIssue {
        name: name.to_owned(),
        detail: detail.into(),
    }
}

#[cfg(test)]
#[path = "validation_test.rs"]
mod tests;
