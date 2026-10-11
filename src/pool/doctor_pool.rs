//! Vivi's own half of `vivi pool doctor`: the config, the slots, and the
//! dispatches recorded against them.

use super::collect::stale_claims;
use super::doctor::Check;
use super::doctor_repo::{GithubView, behind_text};
use super::record::OpenDispatch;
use super::run::annotate_running;
use super::slot::SlotStatus;

pub const GROUP: &str = "Pool (Vivi)";
const RECONCILE: &str = "vivi pool reconcile";
const CREATE: &str = "vivi pool create";

/// What the pool checks read.
pub struct PoolFacts<'a> {
    /// The `[pool]` validation result.
    pub config: Result<(), String>,
    pub max: u32,
    /// The pool's slots; `None` when the provider checks failed first.
    pub slots: Option<Result<Vec<SlotStatus>, String>>,
    pub open: &'a [OpenDispatch],
    pub now: u64,
    pub github: &'a GithubView,
}

/// Turn the facts into checks.
#[must_use]
pub fn checks(f: &PoolFacts) -> Vec<Check> {
    let mut out = vec![config_check(&f.config)];
    match &f.slots {
        None => out.push(Check::skipped(
            GROUP,
            "slots",
            "needs the provider checks to pass",
        )),
        Some(Err(e)) => out.push(Check::fail(
            GROUP,
            "slots",
            format!("cannot list machines: {e}"),
            "vivi pool status (shows the provider's own error)",
        )),
        Some(Ok(slots)) => out.extend(slot_checks(f, slots)),
    }
    out
}

fn config_check(config: &Result<(), String>) -> Check {
    match config {
        Ok(()) => Check::ok(GROUP, "config", "[pool] is valid"),
        Err(e) => Check::fail(
            GROUP,
            "config",
            e.clone(),
            "edit [pool] in .vivi/mailspace.toml",
        ),
    }
}

fn slot_checks(f: &PoolFacts, slots: &[SlotStatus]) -> Vec<Check> {
    let mut slots = slots.to_vec();
    annotate_running(&mut slots, f.open, f.now);
    let mut out = vec![count_check(f.max, &slots)];
    out.extend(slots.iter().map(|s| slot_check(f, s)));
    out.push(leak_check(f, &slots));
    out.push(orphan_check(f, &slots));
    out.push(stale_check(f, &slots));
    out.push(overdue_check(&slots));
    out
}

fn count_check(max: u32, slots: &[SlotStatus]) -> Check {
    let have = u32::try_from(slots.len()).unwrap_or(u32::MAX);
    let detail = format!("{have} of {max}");
    match have.cmp(&max) {
        std::cmp::Ordering::Equal => Check::ok(GROUP, "slot count", detail),
        std::cmp::Ordering::Less => Check::warn(GROUP, "slot count", detail, CREATE),
        std::cmp::Ordering::Greater => Check::warn(
            GROUP,
            "slot count",
            format!("{detail} (more than the configured maximum)"),
            "vivi pool destroy --slot <name> --yes (for the extra slots), or raise `slots` in [pool]",
        ),
    }
}

fn slot_check(f: &PoolFacts, slot: &SlotStatus) -> Check {
    let name = format!("slot {}", slot.name);
    let base = slot.base.as_deref().map_or_else(
        || "base unknown".to_string(),
        |b| {
            format!(
                "base {}, {}",
                &b[..12.min(b.len())],
                behind_text(f.github, b)
            )
        },
    );
    let arch = slot
        .arch
        .as_deref()
        .map_or(String::new(), |a| format!(", arch {a}"));
    let detail = format!("{}, {base}{arch}", slot.state);
    let reseed = format!(
        "vivi pool destroy --slot {} --yes, then {CREATE}",
        slot.name
    );
    match slot.state.as_str() {
        "free" => Check::ok(GROUP, &name, detail),
        "busy" if slot.running_secs.is_some() => Check::ok(
            GROUP,
            &name,
            format!(
                "{detail}, task {} running {}s of {}s",
                slot.task.as_deref().unwrap_or("?"),
                slot.running_secs.unwrap_or(0),
                slot.budget_secs.unwrap_or(0)
            ),
        ),
        "busy" => Check::warn(
            GROUP,
            &name,
            format!("{detail}, running with no open dispatch"),
            RECONCILE,
        ),
        "finished-uncollected" => Check::warn(
            GROUP,
            &name,
            format!("{detail}, a result is waiting on its disk"),
            RECONCILE,
        ),
        "seeding" => Check::warn(
            GROUP,
            &name,
            format!("{detail}, a `vivi pool create` may still be running"),
            format!("{CREATE} (waits for it and adopts a clean seed)"),
        ),
        "unseeded" => Check::fail(
            GROUP,
            &name,
            format!("{detail}, the seed failed or was interrupted"),
            format!("{CREATE} (adopts a clean seed); if it stays unseeded: {reseed}"),
        ),
        _ => Check::warn(GROUP, &name, detail, reseed),
    }
}

/// Running slots that no open dispatch explains: a cost until stopped. A claim
/// younger than the stale limit is a dispatch still being set up, not a leak.
fn leak_check(f: &PoolFacts, slots: &[SlotStatus]) -> Check {
    let stale = stale_claims(slots, f.open, f.now);
    let leaked: Vec<&str> = slots
        .iter()
        .filter(|s| s.state == "busy" && s.running_secs.is_none())
        .filter(|s| s.task.is_none() || stale.iter().any(|t| t.id == s.id))
        .map(|s| s.name.as_str())
        .collect();
    if leaked.is_empty() {
        return Check::ok(
            GROUP,
            "leaked runners",
            "no running slot without a dispatch",
        );
    }
    Check::fail(
        GROUP,
        "leaked runners",
        format!(
            "{} running with no open dispatch (holding a machine, and billed by the second on Fly)",
            leaked.join(", ")
        ),
        RECONCILE,
    )
}

/// Open dispatches whose machine is no longer a slot of the pool.
fn orphan_check(f: &PoolFacts, slots: &[SlotStatus]) -> Check {
    let orphans: Vec<String> = f
        .open
        .iter()
        .filter(|d| !slots.iter().any(|s| s.id == d.record.machine))
        .map(|d| format!("{} (machine {})", d.task, d.record.machine))
        .collect();
    if orphans.is_empty() {
        return Check::ok(
            GROUP,
            "orphaned dispatches",
            "every open dispatch has its machine",
        );
    }
    Check::fail(
        GROUP,
        "orphaned dispatches",
        format!("open dispatch with no machine: {}", orphans.join(", ")),
        format!(
            "{RECONCILE} (records each as lost and closes it; a destroyed machine has no result to collect)"
        ),
    )
}

/// Held slots with no open dispatch, past the grace for a dispatch in setup.
fn stale_check(f: &PoolFacts, slots: &[SlotStatus]) -> Check {
    let stale = stale_claims(slots, f.open, f.now);
    if stale.is_empty() {
        return Check::ok(GROUP, "stale claims", "no claim without a dispatch");
    }
    let parts: Vec<String> = stale
        .iter()
        .map(|s| {
            let age = s.claimed_at.map_or("age unknown".to_string(), |at| {
                age_text(f.now.saturating_sub(at))
            });
            format!(
                "{} (task {}, claimed {age} ago)",
                s.name,
                s.task.as_deref().unwrap_or("?")
            )
        })
        .collect();
    Check::warn(
        GROUP,
        "stale claims",
        format!("held with no open dispatch: {}", parts.join(", ")),
        RECONCILE,
    )
}

fn overdue_check(slots: &[SlotStatus]) -> Check {
    let late: Vec<String> = slots
        .iter()
        .filter(|s| s.state == "busy")
        .filter_map(|s| Some((s, s.running_secs?, s.budget_secs?)))
        .filter(|(_, ran, budget)| ran > budget)
        .map(|(s, ran, budget)| format!("{} ({ran}s of {budget}s)", s.name))
        .collect();
    if late.is_empty() {
        return Check::ok(GROUP, "overdue", "no busy slot is past its budget");
    }
    Check::warn(
        GROUP,
        "overdue",
        format!("past its budget: {}", late.join(", ")),
        format!("{RECONCILE} (stops a slot once the grace after its budget has passed)"),
    )
}

fn age_text(secs: u64) -> String {
    match secs {
        0..120 => format!("{secs}s"),
        120..7200 => format!("{}m", secs / 60),
        _ => format!("{}h", secs / 3600),
    }
}
