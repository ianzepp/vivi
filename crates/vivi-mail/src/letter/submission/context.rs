//! Storage-free predicates for contextual reply rules.
use crate::letter::roles::{CheckResult, Hand, HandAnswer, RoleKind, Slicer, SlicerMode};
use serde::{Deserialize, Serialize};

/// Current reply-contract wire version.
pub const REPLY_CONTRACT_VERSION: u32 = 1;

/// Execution and slicing context kept separate from the transport body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyContract {
    pub version: u32,
    pub role: RoleKind,
    pub bound_step: String,
    pub run: u32,
    pub permitted_answers: Vec<String>,
    pub expected_slicer_mode: Option<SlicerMode>,
    pub target: Option<String>,
    pub scope: Vec<String>,
    pub dependencies: Vec<String>,
}

impl ReplyContract {
    #[must_use]
    pub fn new(role: RoleKind, bound_step: impl Into<String>, run: u32) -> Self {
        Self {
            version: REPLY_CONTRACT_VERSION,
            role,
            bound_step: bound_step.into(),
            run,
            permitted_answers: role
                .answer_words()
                .iter()
                .map(|word| (*word).to_string())
                .collect(),
            expected_slicer_mode: None,
            target: None,
            scope: Vec::new(),
            dependencies: Vec::new(),
        }
    }
}

/// Whether the answer is allowed by a structured reply contract.
#[must_use]
pub fn answer_compatible(permitted: &[String], answer: &str) -> bool {
    permitted.iter().any(|word| word == answer)
}

/// Whether a CTO reply violates the run-3 new-fact rule.
#[must_use]
pub fn cto_missing_new_fact(
    role: RoleKind,
    run: u32,
    answer: &str,
    front: &serde_json::Value,
) -> bool {
    role == RoleKind::Cto
        && run >= 3
        && answer != "lock"
        && !front
            .get("new_fact")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|s| !s.trim().is_empty())
}

/// Whether a built hand claims success while reporting a failed final-tree check.
#[must_use]
pub fn failed_check_commands(hand: &Hand) -> Vec<&str> {
    hand.check
        .iter()
        .filter(|c| c.result == CheckResult::Fail)
        .map(|c| c.cmd.as_str())
        .collect()
}
#[must_use]
pub fn built_reports_failed_check(hand: &Hand) -> bool {
    hand.answer == HandAnswer::Built && !failed_check_commands(hand).is_empty()
}

/// Whether a slicer reply has the requested mode.
#[must_use]
pub fn slicer_mode_matches(slicer: &Slicer, expected: SlicerMode) -> bool {
    slicer.mode == expected
}

/// Check target and write-scope predicates shared by slicer consumers.
#[must_use]
pub fn target_known(target: &str, known: &[String]) -> bool {
    known.iter().any(|item| item == target)
}
#[must_use]
pub fn scope_nonempty(scope: &[String]) -> bool {
    scope.iter().any(|path| !path.trim().is_empty())
}

/// Return stable topological indices, using the supplied earlier-slice snapshot.
pub fn slice_order(
    ids_deps: &[(String, Vec<String>)],
    earlier: &[String],
) -> Result<Vec<usize>, String> {
    for (i, (id, _)) in ids_deps.iter().enumerate() {
        if ids_deps[..i].iter().any(|(other, _)| other == id) {
            return Err(format!("slice id `{id}` appears twice"));
        }
    }
    for (id, deps) in ids_deps {
        for dep in deps {
            if dep == id {
                return Err(format!("slice `{id}` depends on itself"));
            }
            if !ids_deps.iter().any(|(other, _)| other == dep) && !earlier.contains(dep) {
                return Err(format!("slice `{id}` depends on unknown slice `{dep}`"));
            }
        }
    }
    let mut placed = Vec::new();
    while placed.len() < ids_deps.len() {
        let next = ids_deps
            .iter()
            .enumerate()
            .find(|(i, (_, deps))| {
                !placed.contains(i)
                    && deps.iter().all(|dep| {
                        placed.iter().any(|p| ids_deps[*p].0 == *dep)
                            || !ids_deps.iter().any(|(other, _)| other == dep)
                    })
            })
            .map(|(i, _)| i);
        match next {
            Some(i) => placed.push(i),
            None => {
                let members = ids_deps
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| !placed.contains(i))
                    .map(|(_, (id, _))| format!("`{id}`"))
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(format!("slice dependencies form a cycle among {members}"));
            }
        }
    }
    Ok(placed)
}

#[cfg(test)]
#[path = "context_test.rs"]
mod tests;

const _: fn(&Hand) -> bool = built_reports_failed_check;
const _: fn(RoleKind, u32, &str, &serde_json::Value) -> bool = cto_missing_new_fact;
