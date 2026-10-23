//! Unit satisfaction and the dependency readers.
//!
//! `satisfied`, `blocked_needs`, `dependency_holds` and `need_landed`.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::pump::host::Ctx;
use crate::pump::state::{State, Status};

/// A unit is satisfied when it merged, found no defect, or was replaced and
/// every replacement is itself satisfied (recursion depth capped at 6).
#[must_use]
pub fn satisfied(st: &State, uid: &str, depth: u32) -> bool {
    let Some(u) = st.units.get(uid) else {
        return false;
    };
    if depth > 6 {
        return false;
    }
    match u.status {
        Status::Merged | Status::NoDefect => true,
        Status::Replaced => {
            !u.replaced_by.is_empty() && u.replaced_by.iter().all(|x| satisfied(st, x, depth + 1))
        }
        Status::Pending
        | Status::Held
        | Status::Inflight
        | Status::Delivered
        | Status::Merging
        | Status::Quarantined
        | Status::Stuck
        | Status::NeedsReplan
        | Status::Splitting
        | Status::Withdrawn
        | Status::Other(_) => false,
    }
}

/// A row of `vivi graph blocked --json`.
#[derive(Debug, Deserialize)]
struct BlockedRow {
    #[serde(default)]
    source_id: String,
    #[serde(default)]
    blocked_by: Vec<String>,
}

/// Raw dependency read from the backlog graph: `need -> prerequisite
/// handles`, exactly as Vivi reports them. Rows with no need handle or no
/// prerequisites are dropped; nothing is filtered by whether the pump
/// already delivered a prerequisite.
///
/// An unreadable answer logs and gives `None`: callers hold nothing and the
/// pump keeps dispatching rather than stalling on a broken read.
#[must_use]
pub fn blocked_needs(ctx: &Ctx) -> Option<BTreeMap<String, Vec<String>>> {
    let out = ctx.vivi(
        "graph blocked backlog --kind need --json --confirm-large",
        60,
    );
    let Some(rows) = parse_blocked(&out.text) else {
        let detail: String = out.text.trim().chars().take(150).collect();
        ctx.log(&format!("graph blocked unreadable: {detail}"));
        return None;
    };
    let mut needs = BTreeMap::new();
    for row in rows {
        if !row.source_id.is_empty() && !row.blocked_by.is_empty() {
            needs.insert(row.source_id, row.blocked_by);
        }
    }
    Some(needs)
}

/// Needs held by an unlanded dependency: `need -> prerequisite handles still
/// unfinished`, from the raw read in [`blocked_needs`]. A dependency counts as
/// landed when its need is closed or all its units are satisfied.
///
/// An unreadable answer holds nothing: the pump keeps dispatching rather
/// than stalling on a broken read.
#[must_use]
pub fn dependency_holds(ctx: &Ctx, st: &State) -> BTreeMap<String, Vec<String>> {
    let waiting = st
        .units
        .values()
        .any(|unit| matches!(unit.status, Status::Pending | Status::Delivered));
    if !waiting {
        return BTreeMap::new();
    }
    let Some(raw) = blocked_needs(ctx) else {
        return BTreeMap::new();
    };
    let mut holds = BTreeMap::new();
    for (need, blocked_by) in raw {
        let unmet: Vec<String> = blocked_by
            .into_iter()
            .filter(|dependency| !need_landed(st, dependency))
            .collect();
        if !unmet.is_empty() {
            holds.insert(need, unmet);
        }
    }
    holds
}

/// Parse the JSON array of a `graph blocked --json` output. Empty output
/// (a host that answers nothing) means no blocked needs; text before the
/// first `[` is skipped, and invalid JSON gives `None`.
fn parse_blocked(text: &str) -> Option<Vec<BlockedRow>> {
    let text = text.trim();
    if text.is_empty() {
        return Some(Vec::new());
    }
    let start = text.find('[')?;
    match serde_json::from_str(&text[start..]) {
        Ok(rows) => Some(rows),
        Err(_) => None,
    }
}

/// Whether a graph prerequisite can unblock dependents: the need is closed or
/// all its units are satisfied.
pub(crate) fn need_landed(st: &State, need: &str) -> bool {
    if st.closed_needs.iter().any(|closed| closed == need) {
        return true;
    }
    st.needs.get(need).is_some_and(|record| {
        !record.units.is_empty() && record.units.iter().all(|uid| satisfied(st, uid, 0))
    })
}
