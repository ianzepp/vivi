//! Storage-free contextual predicates shared by structured submissions and pump readers.

use crate::letter::roles::{CheckResult, Hand, HandAnswer, Slice, SlicerMode};

pub const NEW_FACT_RUN: u32 = 3;

pub fn step_answer_compatible(permitted: &[String], answer: &str) -> bool {
    permitted.iter().any(|word| word == answer)
}

pub fn cto_missing_new_fact(run: u32, answer: &str, front: &serde_json::Value) -> bool {
    run >= NEW_FACT_RUN
        && answer != "lock"
        && !front
            .get("new_fact")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|fact| !fact.trim().is_empty())
}

pub fn built_reports_failed_check(hand: &Hand) -> bool {
    hand.answer == HandAnswer::Built
        && hand
            .check
            .iter()
            .any(|check| check.result == CheckResult::Fail)
}

pub fn wrong_slicer_mode(actual: SlicerMode, expected: SlicerMode) -> bool {
    actual != expected
}

pub fn validate_slices(
    slices: &[Slice],
    known_targets: &[String],
    earlier: &[String],
) -> Vec<String> {
    let mut errors = Vec::new();
    for slice in slices {
        if !known_targets.contains(&slice.target) {
            errors.push(format!(
                "slice {} names unknown target {}",
                slice.id, slice.target
            ));
        }
        if slice.write_scope.iter().all(|path| path.trim().is_empty()) {
            errors.push(format!("slice {} has an empty write_scope", slice.id));
        }
    }
    if let Err(error) = topological_order(slices, earlier) {
        errors.push(error);
    }
    errors
}

/// Stable topological sort. `earlier` is the caller's snapshot of completed/previous slice ids.
pub fn topological_order<'a>(
    slices: &'a [Slice],
    earlier: &[String],
) -> Result<Vec<&'a Slice>, String> {
    for (index, slice) in slices.iter().enumerate() {
        if slices[..index].iter().any(|other| other.id == slice.id) {
            return Err(format!("slice id `{}` appears twice", slice.id));
        }
    }
    for slice in slices {
        for dep in &slice.deps {
            if dep == &slice.id {
                return Err(format!("slice `{}` depends on itself", slice.id));
            }
            if !slices.iter().any(|other| other.id == *dep) && !earlier.contains(dep) {
                return Err(format!(
                    "slice `{}` depends on unknown slice `{dep}`",
                    slice.id
                ));
            }
        }
    }
    let mut placed: Vec<&Slice> = Vec::with_capacity(slices.len());
    while placed.len() < slices.len() {
        let next = slices.iter().find(|slice| {
            !placed.iter().any(|done| done.id == slice.id)
                && slice.deps.iter().all(|dep| {
                    placed.iter().any(|done| done.id == *dep)
                        || (!slices.iter().any(|other| other.id == *dep) && earlier.contains(dep))
                })
        });
        match next {
            Some(slice) => placed.push(slice),
            None => {
                let members: Vec<String> = slices
                    .iter()
                    .filter(|slice| !placed.iter().any(|done| done.id == slice.id))
                    .map(|slice| format!("`{}`", slice.id))
                    .collect();
                return Err(format!(
                    "slice dependencies form a cycle among {}",
                    members.join(", ")
                ));
            }
        }
    }
    Ok(placed)
}

#[cfg(test)]
#[path = "context_test.rs"]
mod tests;
