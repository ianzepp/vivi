//! Main's failing set and the last green gate over it.
//!
//! These helpers take the slices they read, so they hold for any recorded
//! promotion and gate history, not just the live state.

use crate::pump::host::Ctx;
use crate::pump::state::{GateRecord, Promotion, State};

/// The failing names recorded for the newest promotion: those stored with it,
/// else the failing set of the gate that tested that tip (a promotion made
/// before the names were recorded). `None` with no promotion or no record.
pub(crate) fn promoted_set(promotions: &[Promotion], gates: &[GateRecord]) -> Option<Vec<String>> {
    let promotion = promotions.last()?;
    promotion.failing.clone().or_else(|| {
        gates
            .iter()
            .rev()
            .find(|gate| gate.tip == promotion.tip)
            .and_then(|gate| gate.names.clone())
    })
}

/// The failing names main carries: the newest promotion's recorded set, else
/// the baseline.
pub(crate) fn main_set(
    promotions: &[Promotion],
    gates: &[GateRecord],
    baseline: Option<&[String]>,
) -> Vec<String> {
    promoted_set(promotions, gates)
        .unwrap_or_else(|| baseline.map_or_else(Vec::new, |b| b.to_vec()))
}

/// The newest gate whose recorded failing set main carries whole: every one
/// of its names is in main's set. An empty set qualifies; a gate with no
/// recorded names never does. `None` when no gate qualifies.
pub(crate) fn green_gate<'a>(
    gates: &'a [GateRecord],
    promotions: &[Promotion],
    baseline: Option<&[String]>,
) -> Option<&'a GateRecord> {
    let main = main_set(promotions, gates, baseline);
    gates.iter().rev().find(|gate| {
        gate.names
            .as_ref()
            .is_some_and(|names| names.iter().all(|name| main.contains(name)))
    })
}

/// The newest gated commit main may take: the tip being finished when its
/// names are within main's set (the gate is not recorded yet), else the
/// newest green gate below it, with the failing set to record.
pub(super) fn green_candidate(
    st: &State,
    tip: &str,
    names: &[String],
) -> Option<(String, Vec<String>)> {
    let main = main_set(&st.promotions, &st.gates, st.baseline.as_deref());
    if names.iter().all(|name| main.contains(name)) {
        return Some((tip.to_owned(), names.to_vec()));
    }
    green_gate(&st.gates, &st.promotions, st.baseline.as_deref())
        .map(|gate| (gate.tip.clone(), gate.names.clone().unwrap_or_default()))
}

/// Fast-forward main to the last green gated commit, skipping the push when
/// main already sits at that candidate.
pub(super) fn promote_last_green(ctx: &Ctx, st: &mut State, tip: &str, names: &[String]) {
    let Some((candidate, failing)) = green_candidate(st, tip, names) else {
        return;
    };
    if st.promotions.last().is_some_and(|p| p.tip == candidate) {
        return;
    }
    super::promote(ctx, st, &candidate, &failing);
}
