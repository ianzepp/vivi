//! Gate-audit ownership: fix need ids and residual red names assigned to an
//! auditor after their prior owner settled or was orphaned.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use crate::pump::gate::is_settled;
use crate::pump::host::{Ctx, shell_quote};
use crate::pump::state::{State, Status};

/// The `state.json` key holding how many times each failing name has been
/// handed to a fresh auditor after its owner settled or was orphaned.
const REPLANS_KEY: &str = "replans";

/// How many times `name` has been audited after its owner settled.
fn replan_count(st: &State, name: &str) -> u64 {
    st.extra
        .get(REPLANS_KEY)
        .and_then(|counts| counts.get(name))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
}

/// `true` when `name` may be handed to a fresh auditor again: a baseline
/// exists and `[pump.gate] max_replans` is not used up for the name.
pub(crate) fn replans_left(ctx: &Ctx, st: &State, name: &str) -> bool {
    st.baseline.is_some() && replan_count(st, name) < u64::from(ctx.cfg.gate.max_replans)
}

/// `true` when the owner has units and every one of them is settled.
fn owner_settled(st: &State, owner: &str) -> bool {
    let statuses: Vec<Status> = st
        .units
        .values()
        .filter(|unit| unit.need == owner)
        .map(|unit| unit.status)
        .collect();
    !statuses.is_empty() && statuses.iter().all(|&s| is_settled(s))
}

/// `true` when the owner has no units, plan, or live audit.
pub(crate) fn orphaned(ctx: &Ctx, st: &State, owner: &str) -> bool {
    if st.units.values().any(|unit| unit.need == owner)
        || ctx
            .home
            .join("plans")
            .join(format!("{owner}.json"))
            .exists()
    {
        return false;
    }
    !crate::pump::audit::current(st).is_some_and(|run| run.need == owner)
}

/// Owned names that still fail after their owner settled or was orphaned,
/// with re-plans left, as `(name, owner)` sorted by name.
pub(crate) fn residual_failures(ctx: &Ctx, st: &State, names: &[String]) -> Vec<(String, String)> {
    let failing: BTreeSet<&String> = names.iter().collect();
    st.red
        .iter()
        .filter(|(name, owner)| {
            failing.contains(name)
                && replans_left(ctx, st, name)
                && (owner_settled(st, owner) || orphaned(ctx, st, owner))
        })
        .map(|(name, owner)| (name.clone(), owner.clone()))
        .collect()
}

/// Hand the residual names to `need`: count the re-audit, own the name by the
/// new need (so promotion stays held) and log one line per old owner.
pub(crate) fn take_residuals(ctx: &Ctx, st: &mut State, need: &str, residual: &[(String, String)]) {
    let mut by_owner: BTreeMap<&str, usize> = BTreeMap::new();
    let mut counts = match st.extra.get(REPLANS_KEY) {
        Some(serde_json::Value::Object(map)) => map.clone(),
        _ => serde_json::Map::new(),
    };
    for (name, owner) in residual {
        st.red.insert(name.clone(), need.to_owned());
        let next = replan_count(st, name) + 1;
        counts.insert(name.clone(), serde_json::json!(next));
        *by_owner.entry(owner.as_str()).or_insert(0) += 1;
    }
    if !residual.is_empty() {
        st.extra
            .insert(String::from(REPLANS_KEY), serde_json::Value::Object(counts));
    }
    for (owner, count) in by_owner {
        ctx.log(&format!(
            "replan: {count} residual failures of {owner} \u{2192} {need}"
        ));
    }
}

/// The needs the pump created itself: the owners of red names, the need of
/// a live audit run (global or per target), and any need whose plan is an
/// audit report. They are planned by the audit, never by the board planner.
pub(crate) fn own_needs(st: &State) -> BTreeSet<String> {
    let mut own: BTreeSet<String> = st.red.values().cloned().collect();
    for target in st.target_states.values() {
        own.extend(target.red.values().cloned());
    }
    own.extend(crate::pump::audit::run_needs(st));
    own.extend(
        st.needs
            .iter()
            .filter(|(_, record)| record.reason == crate::pump::audit::REPORT_REASON)
            .map(|(need, _)| need.clone()),
    );
    own
}

/// The ids of every pump-created gate-fix need and of each of their units.
/// Gate-fix needs carry ordinary handle ids, so the `fix_prefix` spelling
/// alone no longer marks their units; this set (the pump's own needs, see
/// [`own_needs`]) is what ranking treats as fix work beside that legacy
/// prefix.
pub(crate) fn fix_ids(st: &State) -> BTreeSet<String> {
    let needs = own_needs(st);
    let mut ids = needs.clone();
    ids.extend(
        st.units
            .iter()
            .filter(|(_, unit)| needs.contains(&unit.need))
            .map(|(uid, _)| uid.clone()),
    );
    ids
}

/// The gate-fix need id for a tip: `<fix_prefix>-g<tip8>`.
pub(crate) fn fix_need(ctx: &Ctx, tip: &str) -> String {
    let short: String = tip.chars().take(8).collect();
    format!("{}-g{short}", ctx.cfg.fix_prefix)
}

/// Send the real backing need for an audit, open its pump record, and return
/// its handle: the mailspace thread fix units' attempt records reply on (on
/// that record). Returns `None`
/// (with the reason logged) when the send fails or no handle can be parsed.
pub(crate) fn send_fix_need(
    ctx: &Ctx,
    tip: &str,
    new: &[String],
    residual: &[(String, String)],
) -> Option<String> {
    let dir = ctx.home.join("audit");
    drop(fs::create_dir_all(&dir));
    let path = dir.join(format!("{}-need.txt", fix_need(ctx, tip)));
    let mut body = format!("The integration branch went red at {tip}.\nNew failures:\n");
    for name in new {
        body.push_str(&format!("- {name}\n"));
    }
    body.push_str("Residual failures still failing after their owner settled:\n");
    for (name, _) in residual {
        body.push_str(&format!("- {name}\n"));
    }
    body.push_str(
        "The pump starts an auditor for these; the auditor's report becomes fix units whose attempt records reply on this thread.\n",
    );
    drop(fs::write(&path, body));
    let short: String = tip.chars().take(8).collect();
    let subject = format!(
        "pump: gate fix for g{short} ({} new, {} residual)",
        new.len(),
        residual.len()
    );
    let args = format!(
        "need send --from {} --to {} --subject {} --body-file {}",
        ctx.cfg.reply_from,
        ctx.cfg.reply_to(),
        shell_quote(&subject),
        shell_quote(&path.display().to_string()),
    );
    let out = ctx.vivi(&args, 60);
    let handle = if out.code == 0 {
        parse_need_handle(&out.text)
    } else {
        None
    };
    match handle {
        Some(handle) => {
            crate::pump::record::open_own(ctx, &handle);
            Some(handle)
        }
        None => {
            ctx.log(&format!("audit need send failed: {}", last_line(&out.text)));
            None
        }
    }
}

/// The need handle from `need send` output: the third whitespace token of
/// the first `created ` line, else the second token of the first `sent ` line.
fn parse_need_handle(text: &str) -> Option<String> {
    let created = text
        .lines()
        .find(|line| line.starts_with("created "))
        .and_then(|line| line.split_whitespace().nth(2));
    let token = created.or_else(|| {
        text.lines()
            .find(|line| line.starts_with("sent "))
            .and_then(|line| line.split_whitespace().nth(1))
    });
    token
        .map(str::to_owned)
        .filter(|handle| !handle.is_empty() && !handle.contains(char::is_whitespace))
}

/// The last non-blank line of `text`, cut to 80 chars.
fn last_line(text: &str) -> String {
    let line = text.trim().lines().last().unwrap_or("");
    line.chars().take(80).collect()
}
