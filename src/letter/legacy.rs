//! TRANSITIONAL (W3-B2): turns an old prose letter body into the typed front
//! matter, copying the line rules the old readers used so readers see the same
//! facts. Deleted by W3B2-39.

use crate::letter::Envelope;
use crate::letter::work::{NeedFront, StepFront, TaskFront, WantFront};

/// A legacy need body as a typed envelope.
pub(crate) fn need(body: &str) -> Envelope<NeedFront> {
    let mut front = NeedFront::new(labeled(body, "done_when"));
    front.write_scope = labeled(body, "write_scope");
    front.explores = explores(body);
    Envelope {
        front,
        body: body.trim().to_string(),
    }
}

/// A legacy want body as a typed envelope. Nothing reads a legacy want's
/// question, so it is empty.
pub(crate) fn want(body: &str) -> Envelope<WantFront> {
    Envelope {
        front: WantFront {
            question: String::new(),
            depends_on: Vec::new(),
        },
        body: body.trim().to_string(),
    }
}

/// A legacy task body as a typed envelope.
pub(crate) fn task(body: &str) -> Envelope<TaskFront> {
    let mut front = TaskFront {
        done_when: labeled(body, "done_when"),
        write_scope: labeled(body, "write_scope"),
        target: target(body),
        step: step(body),
        ..TaskFront::default()
    };
    for (key, value) in ref_lines(body) {
        match key.to_ascii_lowercase().as_str() {
            "from" if front.from.is_none() => front.from = Some(value.to_string()),
            "branch" if front.branch.is_none() => front.branch = Some(value.to_string()),
            "band" if front.band.is_none() => front.band = Some(value.to_string()),
            "with" => front.with.push(value.to_string()),
            _ => {}
        }
    }
    Envelope {
        front,
        body: body.trim().to_string(),
    }
}

/// Every line whose lowercased `trim_start()` begins with `label:`; the value
/// is the rest of the line, trimmed.
fn labeled(body: &str, label: &str) -> Vec<String> {
    let prefix = format!("{label}:");
    body.lines()
        .filter_map(|line| {
            let trimmed = line.trim_start();
            trimmed
                .to_ascii_lowercase()
                .starts_with(&prefix)
                .then(|| trimmed[prefix.len()..].trim().to_string())
        })
        .collect()
}

/// The want a research need names with its first `for:` line, without
/// surrounding `<>` or backticks; empty values are skipped.
fn explores(body: &str) -> Option<String> {
    body.lines().find_map(|line| {
        let rest = line.trim().strip_prefix("for:")?;
        let want = rest
            .trim()
            .trim_matches(|c| matches!(c, '<' | '>' | '`'))
            .trim();
        (!want.is_empty()).then(|| want.to_owned())
    })
}

/// The first line starting `target:` (not trimmed first); empty values are skipped.
fn target(body: &str) -> Option<String> {
    body.lines()
        .find_map(|line| line.strip_prefix("target:"))
        .map(str::trim)
        .filter(|target| !target.is_empty())
        .map(str::to_string)
}

/// The `key: value` lines of a task body whose value is one word: lines are
/// trimmed and a leading list dash is ignored.
fn ref_lines(body: &str) -> impl Iterator<Item = (&str, &str)> {
    body.lines().filter_map(|line| {
        let line = line.trim().trim_start_matches(['-', '*']).trim();
        let (key, value) = line.split_once(':')?;
        let value = value.trim();
        (!value.is_empty() && !value.contains(char::is_whitespace)).then_some((key.trim(), value))
    })
}

/// The step named by the first non-blank line of a task body.
fn step(body: &str) -> Option<StepFront> {
    let first = body.lines().find(|line| !line.trim().is_empty())?;
    let first = first.trim_start();
    if let Some(rest) = first.strip_prefix("pump-step:") {
        let mut tokens = rest.split_whitespace();
        let need = tokens.next()?.to_string();
        let run = run_number(tokens.next());
        let node = tokens.next().map(str::to_string);
        return Some(StepFront {
            need,
            run,
            node,
            sentinel: false,
        });
    }
    let rest = first.strip_prefix("pump-run-sentinel:")?;
    let mut tokens = rest.split_whitespace();
    let need = tokens.next()?.to_string();
    let run = run_number(tokens.next());
    Some(StepFront {
        need,
        run,
        node: None,
        sentinel: true,
    })
}

/// A run token such as `r3`: the number after a leading `r`, or 0.
fn run_number(token: Option<&str>) -> u32 {
    let Some(token) = token else { return 0 };
    token
        .strip_prefix('r')
        .unwrap_or(token)
        .parse::<u32>()
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "legacy_test.rs"]
mod tests;
