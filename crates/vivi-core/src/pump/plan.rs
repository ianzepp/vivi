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

#[derive(Clone, Copy)]
enum ExistingUnitPolicy {
    Reject,
    SameNeed,
    WithdrawnSameNeed,
}

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
///
/// A file that cannot be a plan is never dropped without a trace: it raises a
/// `bad-plan` exception once per distinct content (the marker in `plans_seen`
/// includes the length, so a corrected file is read again) and the log names
/// the file and the reason.
fn ingest_one(ctx: &Ctx, st: &mut State, path: &Path, key: &str) {
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    let plan = match serde_json::from_str::<Value>(&text) {
        Ok(plan) => plan,
        Err(error) => {
            reject_unreadable(
                ctx,
                st,
                key,
                text.len(),
                &format!("not valid JSON: {error}"),
            );
            return;
        }
    };
    let Some(need) = str_field(&plan, "need").filter(|s| !s.is_empty()) else {
        reject_unreadable(ctx, st, key, text.len(), "it names no `need`");
        return;
    };
    if st.plans_seen.iter().any(|seen| seen == key) {
        return;
    }
    let replaces = str_field(&plan, "replaces");
    let revision = key_revision(key, need).or_else(|| plan.get("revision").and_then(Value::as_u64));
    if ingest_existing_revision(ctx, st, key, need, revision, replaces, &plan, &text) {
        return;
    }
    st.plans_seen.push(key.to_string());
    let units = plan.get("units").and_then(Value::as_array);
    let policy = if replaces.is_some() {
        ExistingUnitPolicy::Reject
    } else {
        ExistingUnitPolicy::WithdrawnSameNeed
    };
    let (minted, bad) = validate_units(ctx, st, units.map_or(&[], Vec::as_slice), policy, need);
    if let Some(bad) = bad {
        reject_bad_units(ctx, st, need, key, replaces, &bad);
        return;
    }
    if replaces.is_none() {
        let waits = crate::pump::plan_check::unlanded_prerequisites(ctx, st, need, &minted);
        if !waits.is_empty() {
            hold_on_prerequisites(ctx, st, need, waits);
            return;
        }
    }
    crate::pump::plan_check::warn_scope_overlaps(ctx, st, need, &minted);
    let revision = revision.unwrap_or(1);
    mint(ctx, st, need, &minted, revision);
    record(ctx, st, &plan, need, replaces, &minted, revision);
}

/// A plan whose units failed validation: a first plan is asked again, a
/// replacement plan raises `bad-plan` and re-splits (or consumes) the unit.
fn reject_bad_units(
    ctx: &Ctx,
    st: &mut State,
    need: &str,
    key: &str,
    replaces: Option<&str>,
    bad: &str,
) {
    if replaces.is_none() {
        reject_first_plan(ctx, st, need, key, bad);
    } else {
        raise_bad_plan(ctx, key, bad);
    }
    if let Some(uid) = replaces
        && !crate::pump::collect::resplit_bad_plan(ctx, st, uid, bad)
    {
        consume_bad_split(ctx, st, uid, key, bad);
    }
}

/// Log a rejected plan with its reasons, then raise the `bad-plan`
/// exception. The log line carries the whole reason; the exception text is
/// cut at 600 characters.
fn raise_bad_plan(ctx: &Ctx, unit: &str, reason: &str) {
    let shown: String = reason.chars().take(2000).collect();
    ctx.log(&format!("bad-plan {unit}: {shown}"));
    ctx.exception("bad-plan", unit, reason);
}

/// The findings kept for the planner prompt: long enough for every finding of
/// a large plan, short enough to leave the prompt room.
fn capped(findings: &str) -> String {
    findings.chars().take(4000).collect()
}

/// How many times a rejected first plan is sent back to its planner before
/// the need is held with the findings as its reason.
pub(crate) const PLAN_REASKS: u32 = 2;

/// The `planner_fails` key counting a need's rejected first plans.
pub(crate) fn invalid_key(need: &str) -> String {
    format!("{need}.invalid")
}

/// Record a rejected first plan. A reason that names a parked design choice
/// (a repository target, a cross-target dependency) holds the need at once.
/// Any other rejection records the need as `invalid` with the findings as its
/// reason; the board pass then supersedes the plan and asks the planner again
/// with those findings ([`crate::pump::board::reask_invalid_plans`]), up to
/// [`PLAN_REASKS`] times, after which the need is `hold` with the findings as
/// its reason.
fn reject_first_plan(ctx: &Ctx, st: &mut State, need: &str, key: &str, bad: &str) {
    let spent = st
        .board
        .planner_fails
        .get(&invalid_key(need))
        .copied()
        .unwrap_or(0);
    let hold = is_hold_reason(bad) || spent >= PLAN_REASKS;
    st.needs.insert(
        need.to_string(),
        NeedRecord {
            verdict: if hold { "hold" } else { "invalid" }.to_string(),
            reason: bad.to_string(),
            units: Vec::new(),
            ..NeedRecord::default()
        },
    );
    if hold {
        st.board.plan_findings.remove(need);
    } else {
        st.board.plan_findings.insert(need.to_string(), capped(bad));
    }
    raise_bad_plan(ctx, key, bad);
    if hold && !is_hold_reason(bad) {
        ctx.exception(
            "plan-held",
            need,
            &format!("{} plans were rejected; the need is held: {bad}", spent + 1),
        );
    }
}

/// Hold a need whose plan is sound but whose prerequisites have not landed:
/// no units are minted, so nothing sits held on every tick. The board plans
/// the need again once every prerequisite has landed
/// ([`crate::pump::board::replan_unblocked`]).
fn hold_on_prerequisites(ctx: &Ctx, st: &mut State, need: &str, waits: Vec<String>) {
    let reason = format!("waits on {}", waits.join(", "));
    ctx.log(&format!("plan {need} held: {reason}"));
    st.board.plan_findings.remove(need);
    st.needs.insert(
        need.to_string(),
        NeedRecord {
            verdict: "hold".to_string(),
            reason,
            units: Vec::new(),
            waits_on: waits,
            ..NeedRecord::default()
        },
    );
}

/// Record, once per content, a plan file that cannot be read as a plan.
fn reject_unreadable(ctx: &Ctx, st: &mut State, key: &str, len: usize, why: &str) {
    let marker = format!("unreadable:{key}:{len}");
    if st.plans_seen.iter().any(|seen| seen == &marker) {
        return;
    }
    st.plans_seen.push(marker);
    ctx.log(&format!("plan file {key} rejected: {why}"));
    ctx.exception("bad-plan", key, &format!("plan file {key}: {why}"));
}

/// The revision a revision plan file is named for (`<need>.r<N>.json`): the
/// pump asked the planner for exactly that file, so it is the revision the
/// plan answers even when the planner left the field out or wrote another.
fn key_revision(key: &str, need: &str) -> Option<u64> {
    let Ok(revision) = key
        .strip_prefix(need)?
        .strip_prefix(".r")?
        .strip_suffix(".json")?
        .parse()
    else {
        return None;
    };
    Some(revision)
}

/// Consume a current-need revision plan, returning `true` when the file was
/// stale or revision application handled it.
fn ingest_existing_revision(
    ctx: &Ctx,
    st: &mut State,
    key: &str,
    need: &str,
    revision: Option<u64>,
    replaces: Option<&str>,
    plan: &Value,
    text: &str,
) -> bool {
    let Some(current) = st.needs.get(need).filter(|_| replaces.is_none()) else {
        return false;
    };
    // A plan without a revision is the old one-shot path; a revision at or
    // below the current one is stale.
    if revision.is_none_or(|revision| revision <= current.revision) {
        let (have, pending) = (current.revision, current.amendment_pending);
        st.plans_seen.push(key.to_string());
        let why = format!(
            "plan file {key} is stale: it carries revision {} and the need is at r{have}",
            revision.map_or_else(|| String::from("none"), |r| format!("r{r}"))
        );
        ctx.log(&why);
        if pending {
            // A revision was asked for and this answer cannot be applied:
            // never drop it quietly.
            ctx.exception("revision-stale", need, &why);
        }
        return true;
    }
    st.plans_seen.push(key.to_string());
    apply_revision(ctx, st, need, revision.unwrap_or(0), plan, text);
    true
}

fn is_hold_reason(reason: &str) -> bool {
    reason.contains("repository target") || reason.contains("cross-target dependency")
}

/// Archive a second invalid split result and park its unit instead of leaving
/// the `plans/<uid>.split.json` guard active forever.
fn consume_bad_split(ctx: &Ctx, st: &mut State, uid: &str, key: &str, reason: &str) {
    let rejected = ctx.home.join("plans/rejected");
    drop(fs::create_dir_all(&rejected));
    let archive = rejected.join(format!("{key}.{}", ctx.now()));
    if fs::rename(ctx.home.join("plans").join(key), archive).is_ok() {
        st.plans_seen.retain(|seen| seen != key);
    } else {
        ctx.log(&format!("split-plan-archive-failed {uid} {key}"));
    }
    let Some(unit) = st.units.get_mut(uid) else {
        ctx.log(&format!("split-plan-discarded {uid}"));
        return;
    };
    if unit.status != Status::Splitting {
        ctx.log(&format!("split-plan-discarded {uid}"));
        return;
    }
    let need = unit.need.clone();
    unit.status = Status::Stuck;
    unit.split_retry_at = None;
    unit.since = ctx.now();
    ctx.exception(
        "split-plan-rejected",
        uid,
        &format!(
            "split re-ask spent; the second plan was rejected: {reason}; replan with `vivi pump replan {need}`"
        ),
    );
    crate::pump::quarantine::repair_stuck(ctx, st, uid);
}

/// Apply a revision to a known need.
fn apply_revision(ctx: &Ctx, st: &mut State, need: &str, revision: u64, plan: &Value, text: &str) {
    let units = plan.get("units").and_then(Value::as_array);
    let (planned, bad) = validate_units(
        ctx,
        st,
        units.map_or(&[], Vec::as_slice),
        ExistingUnitPolicy::SameNeed,
        need,
    );
    if let Some(bad) = bad {
        ctx.log(&format!("revision r{revision} of {need} rejected: {bad}"));
        raise_bad_plan(ctx, need, &bad);
        // The need stays `amendment_pending`: ask the planner again, with the
        // findings, on the revision re-ask schedule.
        st.board
            .plan_findings
            .insert(need.to_string(), capped(&bad));
        crate::pump::amend::schedule_reask(ctx, st, need);
        return;
    }
    crate::pump::plan_check::warn_scope_overlaps(ctx, st, need, &planned);
    let counts = apply_planned(ctx, st, need, revision, &planned);
    let withdrawn = apply_withdrawals(ctx, st, plan);
    record_revision(ctx, st, need, revision, plan, text, &counts, &withdrawn);
}

/// What a revision did, for the log line.
#[derive(Default)]
struct Counts {
    added: usize,
    amended: usize,
    conflicts: usize,
    new_ids: Vec<String>,
}

/// Apply the planned units and return the counts.
fn apply_planned(
    ctx: &Ctx,
    st: &mut State,
    need: &str,
    revision: u64,
    planned: &[(&str, &Value)],
) -> Counts {
    let mut counts = Counts::default();
    for (uid, u) in planned {
        match st.units.get_mut(*uid) {
            None => {
                mint_one(ctx, st, need, uid, u, revision);
                counts.new_ids.push((*uid).to_string());
                counts.added += 1;
            }
            Some(unit) if unit.revision == revision => {}
            Some(unit) => match apply_one(ctx, unit, u, revision, uid) {
                Apply::Amended => counts.amended += 1,
                Apply::Kept => {}
                Apply::Conflict => counts.conflicts += 1,
            },
        }
    }
    counts
}

/// Withdraw the named pending units and return the removed ids.
fn apply_withdrawals(ctx: &Ctx, st: &mut State, plan: &Value) -> Vec<String> {
    let mut removed = Vec::new();
    for uid in str_list(plan.get("withdraw")) {
        match st.units.get_mut(&uid) {
            Some(unit) if unit.status == Status::Pending => {
                unit.status = Status::Withdrawn;
                removed.push(uid);
            }
            Some(_) => ctx.exception(
                "revision-conflict",
                &uid,
                "withdraw refused: the unit is no longer pending",
            ),
            None => {}
        }
    }
    removed
}

/// Update the need record, persist the revision and log the outcome.
fn record_revision(
    ctx: &Ctx,
    st: &mut State,
    need: &str,
    revision: u64,
    plan: &Value,
    text: &str,
    counts: &Counts,
    removed: &[String],
) {
    let verdict = str_field(plan, "verdict").unwrap_or("units").to_string();
    let reason = recorded_reason(plan, &verdict);
    let record = st
        .needs
        .entry(need.to_string())
        .or_insert_with(|| NeedRecord {
            verdict: verdict.clone(),
            reason: reason.clone(),
            ..NeedRecord::default()
        });
    record.verdict.clone_from(&verdict);
    record.reason.clone_from(&reason);
    record.revision = revision;
    record.amendment_pending = false;
    record.units.retain(|uid| !removed.contains(uid));
    record.units.extend(counts.new_ids.iter().cloned());
    st.board.revision_planning.remove(need);
    st.board.plan_findings.remove(need);
    st.board.planner_fails.remove(&format!("{need}.revision"));
    st.board
        .planner_retry_at
        .remove(&format!("{need}.revision"));
    if let Err(err) = crate::pump::store::record_plan_revision(
        &ctx.home,
        need,
        revision,
        &verdict,
        &reason,
        text,
        ctx.now(),
    ) {
        ctx.log(&format!("plan revision record failed: {err}"));
    }
    ctx.log(&format!(
        "revise {need} r{revision} added={} amended={} withdrew={} conflicts={}",
        counts.added,
        counts.amended,
        removed.len(),
        counts.conflicts
    ));
}

/// What a revision did to one existing unit.
enum Apply {
    Amended,
    Kept,
    Conflict,
}

/// Apply one planned unit to an existing unit, respecting the state rules.
fn apply_one(ctx: &Ctx, unit: &mut Unit, plan: &Value, revision: u64, uid: &str) -> Apply {
    let changed = !same_fields(unit, plan);
    if unit.attempts > 0 && unit.target != str_field(plan, "target").unwrap_or("default") {
        ctx.exception(
            "revision-conflict",
            uid,
            "refused to change repository target after an attempt",
        );
        return Apply::Conflict;
    }
    match unit.status {
        Status::Pending | Status::Held | Status::NeedsReplan => {
            unit.status = Status::Pending;
            set_fields(unit, plan, revision);
            Apply::Amended
        }
        Status::Merged | Status::NoDefect => {
            ctx.log(&format!(
                "revision keeps {uid} as merged; it is not dispatched again"
            ));
            if changed {
                ctx.exception("revision-conflict", uid, "refused to change a merged unit");
                Apply::Conflict
            } else {
                Apply::Kept
            }
        }
        Status::Inflight
        | Status::Delivered
        | Status::Merging
        | Status::Quarantined
        | Status::Splitting
        | Status::Stuck => {
            if changed {
                ctx.exception(
                    "revision-conflict",
                    uid,
                    "refused to change a unit in flight",
                );
                Apply::Conflict
            } else {
                Apply::Kept
            }
        }
        Status::Replaced | Status::Withdrawn | Status::Other(_) => Apply::Kept,
    }
}

/// Check every unit of a plan before anything is minted: `id`, `brief` and a
/// non-empty `write_scope` must be present, `proof` must be a list of commands
/// this toolchain can parse (see [`crate::pump::proof`]), and ids must not
/// collide with existing units. Returns the valid `(id, unit)` pairs, or the
/// reason the plan was rejected.
fn validate_units<'a>(
    ctx: &Ctx,
    st: &State,
    units: &'a [Value],
    existing_policy: ExistingUnitPolicy,
    need: &str,
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
        let target = match target_for_plan(ctx, u) {
            Ok(target) => target,
            Err(reason) => return (minted, Some(reason)),
        };
        let repo = ctx
            .cfg
            .target_path(&ctx.project, &target)
            .unwrap_or_else(|| ctx.project.clone());
        for cmd in str_list(u.get("proof")) {
            if let Err(reason) = crate::pump::proof::check(&cmd, &repo, &ctx.cfg.proof.allow) {
                return (minted, Some(format!("unit {uid}: {reason}")));
            }
        }
        if let Some(existing) = st.units.get(uid) {
            match (existing_policy, existing.status) {
                (ExistingUnitPolicy::SameNeed, _) if existing.need == need => {}
                (ExistingUnitPolicy::SameNeed, _) => {
                    return (minted, Some(format!("unit {uid} belongs to another need")));
                }
                (ExistingUnitPolicy::WithdrawnSameNeed, Status::Withdrawn)
                    if existing.need == need => {}
                _ => return (minted, Some(format!("duplicate unit id {uid}"))),
            }
        }
        minted.push((uid, u));
    }
    if let Some(reason) = cross_target_dependency(st, ctx, &minted) {
        return (minted, Some(reason));
    }
    let found = crate::pump::plan_check::findings(ctx, st, need, &minted);
    if !found.is_empty() {
        return (minted, Some(found.join("; ")));
    }
    (minted, None)
}

/// Cross-target prerequisites would need an unpublished branch handoff, which
/// the shared-pool runner does not implement. Hold the whole plan safely.
fn cross_target_dependency(st: &State, ctx: &Ctx, units: &[(&str, &Value)]) -> Option<String> {
    let mut planned = std::collections::BTreeMap::new();
    for (uid, plan) in units {
        if let Ok(target) = target_for_plan(ctx, plan) {
            planned.insert(*uid, target);
        }
    }
    for (uid, plan) in units {
        let target = match target_for_plan(ctx, plan) {
            Ok(target) => target,
            Err(_) => continue,
        };
        for dependency in str_list(plan.get("after")) {
            let dependency_target = planned.get(dependency.as_str()).cloned().or_else(|| {
                st.units
                    .get(&dependency)
                    .map(|unit| unit.target_id().to_string())
            });
            if dependency_target.is_some_and(|dependency_target| dependency_target != target) {
                return Some(format!(
                    "cross-target dependency {dependency} for unit {uid} needs an unpublished branch handoff"
                ));
            }
        }
    }
    None
}

/// Resolve one plan unit's target. Legacy pump config has exactly one
/// implicit target; named target maps require the planner to choose one.
pub(super) fn target_for_plan(ctx: &Ctx, plan: &Value) -> Result<String, String> {
    let targets = ctx.cfg.target_configs();
    let Some(target) = str_field(plan, "target") else {
        return if ctx.cfg.targets.is_empty() {
            Ok(String::from("default"))
        } else {
            Err("unit must name a configured repository target".to_string())
        };
    };
    if targets.contains_key(target) {
        Ok(target.to_string())
    } else {
        Err(format!("unit names unknown repository target `{target}`"))
    }
}

/// Mint one pending unit per validated plan unit and append it to `order`.
fn mint(ctx: &Ctx, st: &mut State, need: &str, minted: &[(&str, &Value)], revision: u64) {
    for (uid, u) in minted {
        mint_one(ctx, st, need, uid, u, revision);
    }
}

/// Mint one pending unit and append it to `order`.
fn mint_one(ctx: &Ctx, st: &mut State, need: &str, uid: &str, plan: &Value, revision: u64) {
    let unit = Unit {
        need: need.to_string(),
        target: target_for_plan(ctx, plan).unwrap_or_else(|_| String::from("default")),
        revision,
        title: str_field(plan, "title")
            .unwrap_or("")
            .chars()
            .take(150)
            .collect(),
        brief: str_field(plan, "brief").unwrap_or("").to_string(),
        write_scope: str_list(plan.get("write_scope")),
        after: str_list(plan.get("after")),
        proof: str_list(plan.get("proof")),
        tools: str_field(plan, "tools").unwrap_or("").to_string(),
        status: Status::Pending,
        since: ctx.now(),
        ..Unit::default()
    };
    st.units.insert(uid.to_string(), unit);
    if !st.order.iter().any(|ordered| ordered == uid) {
        st.order.push(uid.to_string());
    }
}

/// True when the planned fields match the existing unit exactly.
pub(super) fn same_fields(unit: &Unit, plan: &Value) -> bool {
    unit.target == str_field(plan, "target").unwrap_or("default")
        && unit.title
            == str_field(plan, "title")
                .unwrap_or("")
                .chars()
                .take(150)
                .collect::<String>()
        && unit.brief == str_field(plan, "brief").unwrap_or("")
        && unit.write_scope == str_list(plan.get("write_scope"))
        && unit.after == str_list(plan.get("after"))
        && unit.proof == str_list(plan.get("proof"))
        && unit.tools == str_field(plan, "tools").unwrap_or("")
}

/// Replace a pending unit's planned fields.
fn set_fields(unit: &mut Unit, plan: &Value, revision: u64) {
    unit.revision = revision;
    unit.target = str_field(plan, "target").unwrap_or("default").to_string();
    unit.title = str_field(plan, "title")
        .unwrap_or("")
        .chars()
        .take(150)
        .collect();
    unit.brief = str_field(plan, "brief").unwrap_or("").to_string();
    unit.write_scope = str_list(plan.get("write_scope"));
    unit.after = str_list(plan.get("after"));
    unit.proof = str_list(plan.get("proof"));
    unit.tools = str_field(plan, "tools").unwrap_or("").to_string();
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
    revision: u64,
) {
    let ids: Vec<String> = minted.iter().map(|(uid, _)| (*uid).to_string()).collect();
    let count = ids.len();
    if let Some(replaces) = replaces
        && record_replacement(ctx, st, plan, need, replaces, &ids)
    {
        return;
    }
    let verdict = str_field(plan, "verdict").unwrap_or("units").to_string();
    let reason = recorded_reason(plan, &verdict);
    st.needs.insert(
        need.to_string(),
        NeedRecord {
            verdict: verdict.clone(),
            reason,
            units: ids,
            revision,
            ..NeedRecord::default()
        },
    );
    st.board.plan_findings.remove(need);
    st.board.planner_fails.remove(&invalid_key(need));
    ctx.log(&format!("plan {need} verdict={verdict} units={count}"));
}

/// Mark the split unit replaced by the minted units and add them to the
/// need's record. `false` when the unit is unknown, so the plan is recorded
/// as a whole-need plan instead.
fn record_replacement(
    ctx: &Ctx,
    st: &mut State,
    plan: &Value,
    need: &str,
    replaces: &str,
    ids: &[String],
) -> bool {
    let Some(old) = st.units.get_mut(replaces) else {
        return false;
    };
    let was_stuck = old.status == Status::Stuck;
    let fails = old.split_fails;
    old.status = Status::Replaced;
    old.replaced_by = ids.to_vec();
    let rec = st
        .needs
        .entry(need.to_string())
        .or_insert_with(|| NeedRecord {
            verdict: "units".to_string(),
            reason: String::new(),
            units: Vec::new(),
            ..NeedRecord::default()
        });
    rec.units.extend(ids.iter().cloned());
    if ids.is_empty() {
        ctx.exception(
            "split-empty",
            replaces,
            str_field(plan, "reason").unwrap_or(""),
        );
    }
    ctx.log(&format!("split {replaces} into {} units", ids.len()));
    if was_stuck {
        resolve_stuck(ctx, replaces, fails);
    }
    true
}

/// Resolve the exception a late split plan recovers: a unit parked by the
/// planner's repeated failures names that kind, a silent one names its own.
fn resolve_stuck(ctx: &Ctx, uid: &str, fails: u32) {
    let kind = if fails >= crate::pump::collect::SPLIT_RETRY_MAX {
        "split-planner-failed"
    } else {
        "split-no-answer"
    };
    ctx.resolve(kind, uid, "a late split plan replaced the stuck unit");
}

/// A string field of a JSON object.
pub(super) fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

/// A JSON array of strings; anything else becomes an empty list.
pub(super) fn str_list(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Characters of a plan's reason kept on the need record. A parked verdict
/// (`operator`, `design`, `hold`) carries its whole answer in the reason and
/// is posted on the need thread, so it keeps far more than a units plan's
/// one-line note; both stay well inside the 64 KiB letter cap.
const UNITS_REASON_CHARS: usize = 400;
const PARKED_REASON_CHARS: usize = 16_000;

fn recorded_reason(plan: &Value, verdict: &str) -> String {
    let cap = if crate::pump::board::PARKED.contains(&verdict) {
        PARKED_REASON_CHARS
    } else {
        UNITS_REASON_CHARS
    };
    str_field(plan, "reason")
        .unwrap_or("")
        .chars()
        .take(cap)
        .collect()
}
