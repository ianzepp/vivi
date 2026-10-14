//! Plan files, ingest and scope overlap.
//!
//! `overlaps` and
//! `ingest`: every `plans/*.json` file under the pump home is read
//! leniently, validated, and minted into pending units.

use std::fs;
use std::path::Path;

use serde_json::Value;

use crate::pump::host::Ctx;
use crate::pump::state::{NeedRecord, State, Status, Unit};

/// Message filed when a plan unit fails validation.
const BAD_UNIT: &str = "unit missing id, brief or write_scope";

/// The path segments of a scope: leading `.` and `/` characters and a
/// trailing slash are stripped. A glob with no `/` matches at any depth, the
/// way `.gitignore` reads it, so it gets a leading `**`. An empty scope has no
/// segments and covers the whole tree.
fn segments(scope: &str) -> Vec<&str> {
    let path = scope
        .trim()
        .trim_start_matches(['.', '/'])
        .trim_end_matches('/');
    let mut parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    if parts.len() == 1 && has_glob(parts[0]) && parts[0] != "**" {
        parts.insert(0, "**");
    }
    parts
}

fn has_glob(segment: &str) -> bool {
    segment.contains(['*', '?', '['])
}

/// True when some file could match both scopes. A scope covers what it names
/// and everything under it; `*`, `?` and `[...]` match within one path
/// segment and `**` matches any number of segments. When both segments at a
/// position are globs, the check assumes they can meet.
#[must_use]
pub fn overlaps(a: &[String], b: &[String]) -> bool {
    a.iter()
        .any(|x| b.iter().any(|y| meet(&segments(x), &segments(y))))
}

fn meet(a: &[&str], b: &[&str]) -> bool {
    match (a.first(), b.first()) {
        (None, _) | (_, None) => true,
        (Some(&"**"), _) => meet(&a[1..], b) || meet(a, &b[1..]),
        (_, Some(&"**")) => meet(a, &b[1..]) || meet(&a[1..], b),
        (Some(x), Some(y)) => segment_meets(x, y) && meet(&a[1..], &b[1..]),
    }
}

fn segment_meets(x: &str, y: &str) -> bool {
    match (has_glob(x), has_glob(y)) {
        (false, false) => x == y,
        (true, false) => glob_match(x.as_bytes(), y.as_bytes()),
        (false, true) => glob_match(y.as_bytes(), x.as_bytes()),
        (true, true) => true,
    }
}

/// One-segment glob match: `*` any run, `?` any one character, and a `[...]`
/// class read as any one character (a looser match only makes more scopes
/// overlap, which is the safe side).
fn glob_match(pattern: &[u8], text: &[u8]) -> bool {
    match pattern.first() {
        None => text.is_empty(),
        Some(b'*') => (0..=text.len()).any(|i| glob_match(&pattern[1..], &text[i..])),
        Some(b'?') => !text.is_empty() && glob_match(&pattern[1..], &text[1..]),
        Some(b'[') => {
            let close = pattern
                .iter()
                .position(|&c| c == b']')
                .unwrap_or(pattern.len() - 1);
            !text.is_empty() && glob_match(&pattern[close + 1..], &text[1..])
        }
        Some(c) => text.first() == Some(c) && glob_match(&pattern[1..], &text[1..]),
    }
}

/// Read every `plans/*.json` under the pump home, sorted by file name, and
/// mint units for each plan not seen before. A file that is not valid JSON
/// is skipped silently; a plan without a `need` skips the file. Invalid
/// units raise a `bad-plan` exception (recording the need as `invalid` for
/// non-split plans), and an empty split raises `split-empty`.
pub fn ingest(ctx: &Ctx, st: &mut State) {
    let dir = ctx.home.join("plans");
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .filter_map(|e| e.file_name().to_str().map(ToOwned::to_owned))
        .collect();
    names.sort();
    for name in names {
        ingest_one(ctx, st, &dir.join(&name), &name);
    }
}

/// Ingest a single plan file (`key` is its base name).
fn ingest_one(ctx: &Ctx, st: &mut State, path: &Path, key: &str) {
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    let Ok(plan) = serde_json::from_str::<Value>(&text) else {
        return; // not valid JSON: skipped silently, no exception
    };
    let Some(need) = str_field(&plan, "need").filter(|s| !s.is_empty()) else {
        return; // a missing need skips the file
    };
    if st.plans_seen.iter().any(|seen| seen == key) {
        return;
    }
    let replaces = str_field(&plan, "replaces");
    if replaces.is_none() && st.needs.contains_key(need) {
        st.plans_seen.push(key.to_string());
        return;
    }
    st.plans_seen.push(key.to_string());
    let units = plan.get("units").and_then(Value::as_array);
    let (minted, bad) = validate_units(st, units.map_or(&[], Vec::as_slice));
    if let Some(bad) = bad {
        if replaces.is_none() {
            st.needs.insert(
                need.to_string(),
                NeedRecord {
                    verdict: "invalid".to_string(),
                    reason: bad.clone(),
                    units: Vec::new(),
                },
            );
        }
        ctx.exception("bad-plan", key, &bad);
        return;
    }
    mint(ctx, st, need, &minted);
    record(ctx, st, &plan, need, replaces, &minted);
}

/// Check every unit of a plan before anything is minted: `id`, `brief` and a
/// non-empty `write_scope` must be present, `proof` must be a list, and ids
/// must not collide with existing units. Returns the valid `(id, unit)`
/// pairs, or the reason the plan was rejected.
fn validate_units<'a>(
    st: &State,
    units: &'a [Value],
) -> (Vec<(&'a str, &'a Value)>, Option<String>) {
    let mut minted: Vec<(&str, &Value)> = Vec::new();
    for u in units {
        let Some(uid) = str_field(u, "id").filter(|s| !s.is_empty()) else {
            return (minted, Some(BAD_UNIT.to_string()));
        };
        let ok_brief = str_field(u, "brief").is_some_and(|s| !s.is_empty());
        let ok_scope = u
            .get("write_scope")
            .and_then(Value::as_array)
            .is_some_and(|a| !a.is_empty());
        let ok_proof = u.get("proof").is_none_or(Value::is_array);
        if !ok_brief || !ok_scope || !ok_proof {
            return (minted, Some(BAD_UNIT.to_string()));
        }
        if st.units.contains_key(uid) {
            return (minted, Some(format!("duplicate unit id {uid}")));
        }
        minted.push((uid, u));
    }
    (minted, None)
}

/// Mint one pending unit per validated plan unit and append it to `order`.
fn mint(ctx: &Ctx, st: &mut State, need: &str, minted: &[(&str, &Value)]) {
    for (uid, u) in minted {
        let unit = Unit {
            need: need.to_string(),
            title: str_field(u, "title")
                .unwrap_or("")
                .chars()
                .take(150)
                .collect(),
            brief: str_field(u, "brief").unwrap_or("").to_string(),
            write_scope: str_list(u.get("write_scope")),
            after: str_list(u.get("after")),
            proof: str_list(u.get("proof")),
            tools: str_field(u, "tools").unwrap_or("").to_string(),
            status: Status::Pending,
            since: ctx.now(),
            ..Unit::default()
        };
        st.units.insert((*uid).to_string(), unit);
        st.order.push((*uid).to_string());
    }
}

/// Record the need: a split plan replaces the named unit and extends the
/// need's units; any other plan sets the need's verdict and reason.
fn record(
    ctx: &Ctx,
    st: &mut State,
    plan: &Value,
    need: &str,
    replaces: Option<&str>,
    minted: &[(&str, &Value)],
) {
    let ids: Vec<String> = minted.iter().map(|(uid, _)| (*uid).to_string()).collect();
    let count = ids.len();
    if let Some(replaces) = replaces
        && let Some(old) = st.units.get_mut(replaces)
    {
        old.status = Status::Replaced;
        old.replaced_by.clone_from(&ids);
        let rec = st
            .needs
            .entry(need.to_string())
            .or_insert_with(|| NeedRecord {
                verdict: "units".to_string(),
                reason: String::new(),
                units: Vec::new(),
            });
        rec.units.extend(ids.iter().cloned());
        if ids.is_empty() {
            ctx.exception(
                "split-empty",
                replaces,
                str_field(plan, "reason").unwrap_or(""),
            );
        }
        ctx.log(&format!("split {replaces} into {count} units"));
        return;
    }
    let verdict = str_field(plan, "verdict").unwrap_or("units").to_string();
    let reason: String = str_field(plan, "reason")
        .unwrap_or("")
        .chars()
        .take(400)
        .collect();
    st.needs.insert(
        need.to_string(),
        NeedRecord {
            verdict: verdict.clone(),
            reason,
            units: ids,
        },
    );
    ctx.log(&format!("plan {need} verdict={verdict} units={count}"));
}

/// A string field of a JSON object.
fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

/// A JSON array of strings; anything else becomes an empty list.
fn str_list(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}
