//! Plan files, ingest and scope overlap.
//!
//! Behaviour port of pump.py `scope_root` (:134), `overlaps` (:143) and
//! `ingest` (:191): every `plans/*.json` file under the pump home is read
//! leniently, validated, and minted into pending units.

use std::fs;
use std::path::Path;

use serde_json::Value;

use crate::pump::host::Ctx;
use crate::pump::state::{NeedRecord, State, Status, Unit};

/// Message filed when a plan unit fails validation (pump.py:215).
const BAD_UNIT: &str = "unit missing id, brief or write_scope";

/// The directory root of a scope path: leading `.` and `/` characters are
/// stripped, the path is cut at the first glob character down to its last
/// directory, and trailing slashes are removed. A glob with no directory
/// part gives the empty root, which overlaps everything.
#[must_use]
pub fn scope_root(path: &str) -> String {
    let mut trimmed = path.trim().trim_start_matches(['.', '/']).to_string();
    if let Some(pos) = trimmed.find(['*', '?', '[']) {
        trimmed.truncate(pos);
        trimmed = match trimmed.rfind('/') {
            Some(i) => trimmed[..i].to_string(),
            None => String::new(),
        };
    }
    trimmed.trim_end_matches('/').to_string()
}

/// True when any scope of `a` and any scope of `b` share a directory root,
/// or either root is empty — a root-level or glob-only scope overlaps
/// everything.
#[must_use]
pub fn overlaps(a: &[String], b: &[String]) -> bool {
    a.iter().any(|x| {
        b.iter().any(|y| {
            let rx = scope_root(x);
            let ry = scope_root(y);
            rx.is_empty() || ry.is_empty() || rx == ry || under(&rx, &ry) || under(&ry, &rx)
        })
    })
}

/// True when `child` is exactly `parent` followed by `/` and more path.
fn under(parent: &str, child: &str) -> bool {
    child
        .strip_prefix(parent)
        .is_some_and(|rest| rest.starts_with('/'))
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
        .filter_map(|e| e.file_name().into_string().ok())
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
            tools: str_field(u, "tools").unwrap_or("plain").to_string(),
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
