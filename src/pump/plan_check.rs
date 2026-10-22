//! Deterministic checks on a planner's plan.
//!
//! [`findings`] runs when a plan or revision is ingested, after the
//! structural checks in [`crate::pump::plan`] pass. Each finding is one
//! sentence naming the unit; a plan with any finding is rejected, the findings
//! are logged and go into the next planner prompt. The heuristics lean to
//! accepting: when a check cannot tell, the plan passes.
//!
//! The checks:
//! - configured proof rules (`[[pump.plan.proof_rules]]`);
//! - proofs that cannot fail by construction: only negated searches, or a
//!   `cargo test` filter that appears in neither the brief nor the write scope;
//! - a unit the plan marks merged that the pump does not have as merged, or a
//!   new unit that repeats a merged unit of the same need.
//!
//! [`unlanded_prerequisites`] names the needs a first plan waits on, and
//! [`warn_scope_overlaps`] logs write-scope overlaps with other needs' live
//! units.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::pump::host::Ctx;
use crate::pump::plan::{overlaps, same_fields, str_field, str_list, target_for_plan};
use crate::pump::proof;
use crate::pump::state::{State, Status};

/// Most files read to look for a test name in a unit's write scope; a scope
/// that is bigger than this is not searched and its filters are accepted.
const MAX_SCOPE_FILES: usize = 4000;

/// Most bytes read from one scope.
const MAX_SCOPE_BYTES: u64 = 24 * 1024 * 1024;

/// The longest file read for a test-name search.
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

/// Every finding for `units`, the validated `(id, unit)` pairs of one plan.
/// Empty when the plan passes.
#[must_use]
pub(super) fn findings(ctx: &Ctx, st: &State, need: &str, units: &[(&str, &Value)]) -> Vec<String> {
    let mut found = Vec::new();
    for (uid, plan) in units {
        if let Some(text) = merged_claim(st, need, uid, plan) {
            found.push(format!("unit {uid}: {text}"));
        }
        if let Some(text) = merged_duplicate(st, need, uid, plan) {
            found.push(format!("unit {uid}: {text}"));
        }
        if !checkable(st, uid, plan) {
            continue;
        }
        let scope = str_list(plan.get("write_scope"));
        let proofs = str_list(plan.get("proof"));
        for text in proof_rule_findings(ctx, &scope, &proofs) {
            found.push(format!("unit {uid}: {text}"));
        }
        if let Some(text) = vacuous_proof(&proofs) {
            found.push(format!("unit {uid}: {text}"));
        }
        let brief = str_field(plan, "brief").unwrap_or("");
        for text in unfindable_filters(ctx, plan, brief, &scope, &proofs) {
            found.push(format!("unit {uid}: {text}"));
        }
    }
    found
}

/// Whether a unit's proof and scope are checked: a new unit, a withdrawn one
/// being planned again, or a pending one the plan changes. A unit the pump
/// already moved past, or one the plan merely repeats, is not re-judged.
fn checkable(st: &State, uid: &str, plan: &Value) -> bool {
    match st.units.get(uid) {
        None => true,
        Some(unit) => match unit.status {
            Status::Pending | Status::Held | Status::NeedsReplan | Status::Withdrawn => {
                !same_fields(unit, plan)
            }
            _ => false,
        },
    }
}

/// A configured proof rule the unit breaks.
fn proof_rule_findings(ctx: &Ctx, scope: &[String], proofs: &[String]) -> Vec<String> {
    let mut found = Vec::new();
    for rule in &ctx.cfg.plan.proof_rules {
        if !overlaps(std::slice::from_ref(&rule.scope), scope) {
            continue;
        }
        let Ok(pattern) = rule.pattern() else {
            continue;
        };
        if !proofs.iter().any(|command| pattern.is_match(command)) {
            found.push(format!(
                "{} (the write scope matches `{}` but no proof command matches `{}`)",
                rule.reason_text(),
                rule.scope,
                rule.proof
            ));
        }
    }
    found
}

/// The finding for a unit whose every proof command only shows that
/// something is absent. Such a proof passes before the unit's change as well
/// as after it, so it proves nothing about the change.
fn vacuous_proof(proofs: &[String]) -> Option<String> {
    if proofs.is_empty() || !proofs.iter().all(|command| absence_only(command, 0)) {
        return None;
    }
    Some(format!(
        "every proof command only checks that something is absent ({}); that cannot fail \
         for the change, so add a proof that fails until the change exists, such as a test \
         that exercises the new behavior",
        proofs.join(" ; ")
    ))
}

/// Whether a proof command only searches for absence. `None` structure
/// (`||`, an empty list) is not absence: when unsure, accept.
fn absence_only(command: &str, depth: usize) -> bool {
    let Some(lines) = proof::pipelines(command) else {
        return false;
    };
    !lines.is_empty() && lines.iter().all(|stages| absence_pipeline(stages, depth))
}

fn absence_pipeline(stages: &[String], depth: usize) -> bool {
    let first = crate::pump::host::shell_split(&stages[0]);
    if first.is_empty() {
        return false;
    }
    if stages.len() == 1
        && depth < 2
        && matches!(basename(&first[0]), "sh" | "bash" | "zsh")
        && first.len() > 2
        && first[1] == "-c"
    {
        return absence_only(&first[2], depth + 1);
    }
    let all_stages: Vec<Vec<String>> = stages
        .iter()
        .map(|stage| crate::pump::host::shell_split(stage))
        .collect();
    if first[0] == "!" {
        return all_stages
            .iter()
            .enumerate()
            .all(|(index, words)| is_search(if index == 0 { &words[1..] } else { words }));
    }
    all_stages.len() >= 2
        && is_search(&first)
        && counts_matches(&first)
        && all_stages[1..all_stages.len() - 1]
            .iter()
            .all(|w| is_search(w))
        && all_stages.last().is_some_and(|last| asks_for_zero(last))
}

/// A `grep`-family command (`git grep` too).
fn is_search(words: &[String]) -> bool {
    let Some(first) = words.first() else {
        return false;
    };
    match basename(first) {
        "grep" | "egrep" | "fgrep" | "rg" | "ag" => true,
        "git" => words
            .iter()
            .skip(1)
            .find(|word| !word.starts_with('-'))
            .is_some_and(|word| word == "grep"),
        _ => false,
    }
}

/// A search that prints a count of matching lines (`-c`, `--count`).
fn counts_matches(words: &[String]) -> bool {
    words.iter().skip(1).any(|word| {
        word == "--count"
            || (word.starts_with('-')
                && !word.starts_with("--")
                && word.len() > 1
                && word[1..].contains('c'))
    })
}

/// The last stage of a count-zero check: a search whose pattern is `0`,
/// `^0` or `^0$`.
fn asks_for_zero(words: &[String]) -> bool {
    is_search(words)
        && words
            .iter()
            .skip(1)
            .any(|word| matches!(word.as_str(), "0" | "^0" | "^0$" | "0$"))
}

/// `cargo test` filters of a unit that appear in neither its brief nor the
/// files of its write scope. The files are read from the target's checkout;
/// when no checkout has files, or the scope is too big to read, the filters
/// are accepted.
fn unfindable_filters(
    ctx: &Ctx,
    plan: &Value,
    brief: &str,
    scope: &[String],
    proofs: &[String],
) -> Vec<String> {
    let filters = test_filters(proofs);
    if filters.is_empty() {
        return Vec::new();
    }
    let Ok(target) = target_for_plan(ctx, plan) else {
        return Vec::new();
    };
    let Some(texts) = scope_texts(ctx, &target, scope) else {
        return Vec::new();
    };
    filters
        .into_iter()
        .filter(|filter| !appears(filter, brief, &texts))
        .map(|filter| {
            format!(
                "`cargo test` filter `{filter}` appears neither in the brief nor in any file of \
                 the write scope, so it would match no test; name a test that exists or that \
                 the brief tells the hand to write"
            )
        })
        .collect()
}

/// The single `cargo test` filters of `proofs` that look like test names,
/// without repeats.
fn test_filters(proofs: &[String]) -> Vec<String> {
    let mut filters = Vec::new();
    for command in proofs {
        for words in simple_commands(command, 0) {
            let words = skip_wrappers(&words);
            if words.first().map(|word| basename(word)) != Some("cargo") {
                continue;
            }
            let Some(found) = proof::cargo_test_filters(words) else {
                continue;
            };
            if let [filter] = found.as_slice()
                && is_test_name(filter)
                && !filters.contains(filter)
            {
                filters.push(filter.clone());
            }
        }
    }
    filters
}

/// A filter made only of identifier characters and `::`; anything else
/// (a glob, a shell expansion) is not judged.
fn is_test_name(filter: &str) -> bool {
    !filter.is_empty()
        && filter
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
}

/// Whether `filter`, or its last `::` segment, is named in the brief or in
/// any of the texts.
fn appears(filter: &str, brief: &str, texts: &[String]) -> bool {
    let last = filter.rsplit("::").next().unwrap_or(filter);
    [filter, last].iter().any(|needle| {
        !needle.is_empty()
            && (brief.contains(needle) || texts.iter().any(|text| text.contains(needle)))
    })
}

/// Every simple command of a proof, with `sh -c` scripts opened up to two
/// levels.
fn simple_commands(command: &str, depth: usize) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    for (_, text) in proof::scan_list(command) {
        let words = crate::pump::host::shell_split(&text);
        if words.is_empty() {
            continue;
        }
        if depth < 2
            && matches!(basename(&words[0]), "sh" | "bash" | "zsh")
            && words.len() > 2
            && words[1] == "-c"
        {
            out.extend(simple_commands(&words[2], depth + 1));
        } else {
            out.push(words);
        }
    }
    out
}

/// The words after a leading `!`, `env` and `NAME=value` wrappers.
fn skip_wrappers(words: &[String]) -> &[String] {
    let mut at = 0;
    while at < words.len()
        && (words[at] == "!"
            || basename(&words[at]) == "env"
            || (words[at].contains('=') && !words[at].starts_with('-')))
    {
        at += 1;
    }
    &words[at..]
}

/// The contents of the files `scope` names in the target's checkout. `None`
/// when no checkout is readable or the scope is too big to read.
fn scope_texts(ctx: &Ctx, target: &str, scope: &[String]) -> Option<Vec<String>> {
    let repo = checkouts(ctx, target)
        .into_iter()
        .find(|dir| populated(dir))?;
    let mut files = BTreeSet::new();
    for entry in scope {
        collect_scope(&repo, entry, &mut files)?;
    }
    let mut texts = Vec::new();
    let mut bytes = 0u64;
    for path in files {
        let Ok(meta) = fs::metadata(&path) else {
            continue;
        };
        if meta.len() > MAX_FILE_BYTES {
            continue;
        }
        bytes += meta.len();
        if bytes > MAX_SCOPE_BYTES {
            return None;
        }
        if let Ok(data) = fs::read(&path) {
            texts.push(String::from_utf8_lossy(&data).into_owned());
        }
    }
    Some(texts)
}

/// A directory with something in it besides `.git`: an empty or missing
/// checkout says nothing about which files exist.
fn populated(dir: &Path) -> bool {
    fs::read_dir(dir).is_ok_and(|entries| {
        entries
            .flatten()
            .any(|entry| entry.file_name().to_str() != Some(".git"))
    })
}

/// The checkouts that can stand for the plan base of `target`, best first:
/// the pump's integration clone, then the source repository.
fn checkouts(ctx: &Ctx, target: &str) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if ctx.cfg.targets.is_empty() {
        dirs.push(ctx.repo.clone());
    } else if let Some(dir) = ctx.cfg.target_repo_dir(&ctx.project, target) {
        dirs.push(dir);
    }
    if let Some(dir) = ctx.cfg.target_path(&ctx.project, target) {
        dirs.push(dir);
    }
    dirs
}

/// Add the files one scope entry covers to `files`. `None` when the walk
/// would pass [`MAX_SCOPE_FILES`].
fn collect_scope(repo: &Path, entry: &str, files: &mut BTreeSet<PathBuf>) -> Option<()> {
    let cleaned = entry
        .trim()
        .trim_start_matches("./")
        .trim_start_matches('/');
    let mut root = repo.to_path_buf();
    for segment in cleaned.split('/').filter(|s| !s.is_empty()) {
        if segment.contains(['*', '?', '[']) {
            break;
        }
        root.push(segment);
    }
    let globbed = cleaned.contains(['*', '?', '[']);
    walk(repo, &root, cleaned, globbed, files)
}

fn walk(
    repo: &Path,
    at: &Path,
    entry: &str,
    globbed: bool,
    files: &mut BTreeSet<PathBuf>,
) -> Option<()> {
    let Ok(meta) = fs::symlink_metadata(at) else {
        return Some(());
    };
    if meta.is_file() {
        let covered = !globbed
            || match at.strip_prefix(repo) {
                Ok(rel) => overlaps(&[entry.to_owned()], &[rel.to_string_lossy().into_owned()]),
                Err(_) => false,
            };
        if covered {
            files.insert(at.to_path_buf());
            if files.len() > MAX_SCOPE_FILES {
                return None;
            }
        }
        return Some(());
    }
    if !meta.is_dir() {
        return Some(());
    }
    let Ok(entries) = fs::read_dir(at) else {
        return Some(());
    };
    for child in entries.flatten() {
        let name = child.file_name();
        if matches!(name.to_str(), Some(".git" | "target" | "node_modules")) {
            continue;
        }
        walk(repo, &child.path(), entry, globbed, files)?;
    }
    Some(())
}

fn basename(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

/// The finding for a unit the plan marks merged (`"status": "merged"`) that
/// the pump does not have as merged in this need.
fn merged_claim(st: &State, need: &str, uid: &str, plan: &Value) -> Option<String> {
    let marked = str_field(plan, "status")?.to_ascii_lowercase();
    if !matches!(
        marked.as_str(),
        "merged" | "no-defect" | "no_defect" | "nodefect"
    ) {
        return None;
    }
    let held = st
        .units
        .get(uid)
        .filter(|unit| unit.need == need)
        .map(|unit| unit.status);
    match held {
        Some(Status::Merged | Status::NoDefect) => None,
        Some(status) => Some(format!(
            "the plan marks it {marked} but the pump has it as {}; leave out a unit that is \
             already done, or return it with its real status",
            status.as_str()
        )),
        None => Some(format!(
            "the plan marks it {marked} but the pump has no merged unit with that id; leave \
             out work that is already done instead of listing it as a live unit"
        )),
    }
}

/// The finding for a new unit that repeats a merged unit of the same need:
/// same brief and the same write scope.
fn merged_duplicate(st: &State, need: &str, uid: &str, plan: &Value) -> Option<String> {
    if st
        .units
        .get(uid)
        .is_some_and(|unit| unit.status != Status::Withdrawn)
    {
        return None;
    }
    let brief = str_field(plan, "brief")?.trim();
    if brief.is_empty() {
        return None;
    }
    let mut scope = str_list(plan.get("write_scope"));
    scope.sort();
    st.units
        .iter()
        .filter(|(other, unit)| {
            other.as_str() != uid
                && unit.need == need
                && matches!(unit.status, Status::Merged | Status::NoDefect)
                && unit.brief.trim() == brief
        })
        .find(|(_, unit)| {
            let mut have = unit.write_scope.clone();
            have.sort();
            have == scope
        })
        .map(|(other, _)| {
            format!(
                "it repeats merged unit {other} (same brief and write scope); do not list \
                 finished work again, and name {other} in `after` where a unit builds on it"
            )
        })
}

/// The needs a first plan waits on: needs the backlog graph says block
/// `need` that have not landed, and the owners of other needs' units named in
/// a unit's `after` whose need has not landed. Sorted, without repeats.
#[must_use]
pub(super) fn unlanded_prerequisites(
    ctx: &Ctx,
    st: &State,
    need: &str,
    units: &[(&str, &Value)],
) -> Vec<String> {
    let mut waits = BTreeSet::new();
    if let Some(graph) = crate::pump::ready::blocked_needs(ctx)
        && let Some(blockers) = graph.get(need)
    {
        for blocker in blockers {
            if blocker != need && !crate::pump::ready::need_landed(st, blocker) {
                waits.insert(blocker.clone());
            }
        }
    }
    for (_, plan) in units {
        for dependency in str_list(plan.get("after")) {
            let Some(owner) = st.units.get(&dependency).map(|unit| unit.need.as_str()) else {
                continue;
            };
            if owner != need && !crate::pump::ready::need_landed(st, owner) {
                waits.insert(owner.to_owned());
            }
        }
    }
    waits.into_iter().collect()
}

/// Log, as a warning, every write-scope overlap between a new unit and a live
/// (not yet merged) unit of another need on the same target. Dispatch already
/// keeps two overlapping units from running together, so this only tells the
/// operator which needs will queue behind each other.
pub(super) fn warn_scope_overlaps(ctx: &Ctx, st: &State, need: &str, units: &[(&str, &Value)]) {
    /// Pairs named per new unit before the line says how many more.
    const MAX_NAMED: usize = 4;
    for (uid, plan) in units {
        let target = target_for_plan(ctx, plan).unwrap_or_else(|_| String::from("default"));
        let generated = ctx.cfg.generated_paths_for(&target);
        let mine = without_generated(&str_list(plan.get("write_scope")), &generated);
        let mut hits = Vec::new();
        for (other, unit) in &st.units {
            if unit.need == need || unit.target_id() != target || !is_live(unit.status) {
                continue;
            }
            let theirs = without_generated(&unit.write_scope, &generated);
            if overlaps(&mine, &theirs) {
                hits.push(format!(
                    "{other} (need {}, {})",
                    unit.need,
                    unit.status.as_str()
                ));
            }
        }
        if hits.is_empty() {
            continue;
        }
        let more = hits.len().saturating_sub(MAX_NAMED);
        hits.truncate(MAX_NAMED);
        let tail = if more > 0 {
            format!(" and {more} more")
        } else {
            String::new()
        };
        ctx.log(&format!(
            "overlap-warn unit {uid} (need {need}) write scope overlaps live unit {}{tail}; \
             they will not run at the same time",
            hits.join(", ")
        ));
    }
}

fn without_generated(scope: &[String], generated: &[String]) -> Vec<String> {
    scope
        .iter()
        .filter(|path| !overlaps(std::slice::from_ref(*path), generated))
        .cloned()
        .collect()
}

/// A unit that has not merged and is still going to (or is) running.
fn is_live(status: Status) -> bool {
    matches!(
        status,
        Status::Pending
            | Status::Held
            | Status::NeedsReplan
            | Status::Inflight
            | Status::Delivered
            | Status::Merging
            | Status::Quarantined
            | Status::Splitting
            | Status::Stuck
    )
}

#[cfg(test)]
#[path = "plan_check_test.rs"]
mod tests;
