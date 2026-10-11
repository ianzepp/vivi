//! Vivi's own half of `vivi pool doctor`: the config, the slots, and the
//! dispatches recorded against them.

use super::collect::stale_claims;
use super::doctor::Check;
use super::doctor_repo::{GithubView, behind_text};
use super::record::OpenDispatch;
use super::requires;
use super::run::annotate_running;
use super::slot::SlotStatus;
use std::collections::BTreeMap;

pub const GROUP: &str = "Pool (Vivi)";

/// What the doctor read from one slot's disk about the pool's `requires`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkView {
    /// The slot's requirements mark, as written by the boot script.
    Mark(String),
    /// The slot was read and has no mark (seeded before `requires` was set).
    Absent,
    /// The slot could not be read, and why.
    Unread(String),
}

/// The pool's `requires` and what each slot's disk says about them.
#[derive(Debug, Default)]
pub struct RequiresView<'a> {
    pub configured: &'a [String],
    /// By slot name. Empty when nothing is configured.
    pub marks: BTreeMap<String, MarkView>,
}

/// What the pool checks read.
pub struct PoolFacts<'a> {
    /// The pool's name, for the commands the fixes name.
    pub pool: &'a str,
    /// The `[pools.<name>]` validation result.
    pub config: Result<(), String>,
    pub max: u32,
    /// The pool's slots; `None` when the provider checks failed first.
    pub slots: Option<Result<Vec<SlotStatus>, String>>,
    pub open: &'a [OpenDispatch],
    pub now: u64,
    pub github: &'a GithubView,
    pub requires: &'a RequiresView<'a>,
}

/// Turn the facts into checks.
#[must_use]
pub fn checks(f: &PoolFacts) -> Vec<Check> {
    let mut out = vec![config_check(f.pool, &f.config)];
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

fn config_check(pool: &str, config: &Result<(), String>) -> Check {
    match config {
        Ok(()) => Check::ok(GROUP, "config", format!("[pools.{pool}] is valid")),
        Err(e) => Check::fail(
            GROUP,
            "config",
            e.clone(),
            format!("edit [pools.{pool}] in .vivi/mailspace.toml"),
        ),
    }
}

fn slot_checks(f: &PoolFacts, slots: &[SlotStatus]) -> Vec<Check> {
    let mut slots = slots.to_vec();
    annotate_running(&mut slots, f.open, f.now);
    let mut out = vec![count_check(f.pool, f.max, &slots)];
    for slot in &slots {
        out.push(slot_check(f, slot));
        out.extend(requires_check(f, slot));
    }
    out.push(leak_check(f, &slots));
    out.push(orphan_check(f, &slots));
    out.push(stale_check(f, &slots));
    out.push(overdue_check(f.pool, &slots));
    out
}

fn count_check(pool: &str, max: u32, slots: &[SlotStatus]) -> Check {
    let have = u32::try_from(slots.len()).unwrap_or(u32::MAX);
    let detail = format!("{have} of {max}");
    match have.cmp(&max) {
        std::cmp::Ordering::Equal => Check::ok(GROUP, "slot count", detail),
        std::cmp::Ordering::Less => Check::warn(GROUP, "slot count", detail, create(pool)),
        std::cmp::Ordering::Greater => Check::warn(
            GROUP,
            "slot count",
            format!("{detail} (more than the configured maximum)"),
            format!(
                "vivi pool destroy --pool {pool} --slot <name> --yes (for the extra slots), or raise `slots` in [pools.{pool}]"
            ),
        ),
    }
}

/// `base abcdef012345, 3 commits behind the default branch tip`, or just the
/// base for a pool with no repository to compare against.
fn base_text(f: &PoolFacts, slot: &SlotStatus) -> String {
    slot.base.as_deref().map_or_else(
        || "base unknown".to_string(),
        |b| {
            let short = &b[..12.min(b.len())];
            if f.github.absent {
                format!("base {short}")
            } else {
                format!("base {short}, {}", behind_text(f.github, b))
            }
        },
    )
}

fn slot_check(f: &PoolFacts, slot: &SlotStatus) -> Check {
    let name = format!("slot {}", slot.name);
    let base = base_text(f, slot);
    let arch = slot
        .arch
        .as_deref()
        .map_or(String::new(), |a| format!(", arch {a}"));
    let detail = format!("{}, {base}{arch}", slot.state);
    let (create, reconcile) = (create(f.pool), reconcile(f.pool));
    let reseed = format!(
        "vivi pool destroy --pool {} --slot {} --yes, then {create}",
        f.pool, slot.name
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
            reconcile,
        ),
        "finished-uncollected" => Check::warn(
            GROUP,
            &name,
            format!("{detail}, a result is waiting on its disk"),
            reconcile,
        ),
        "seeding" => Check::warn(
            GROUP,
            &name,
            format!("{detail}, a `vivi pool create` may still be running"),
            format!("{create} (waits for it and adopts a clean seed)"),
        ),
        "unseeded" => Check::fail(
            GROUP,
            &name,
            format!("{detail}, the seed failed or was interrupted"),
            format!("{create} (adopts a clean seed); if it stays unseeded: {reseed}"),
        ),
        _ => Check::warn(GROUP, &name, detail, reseed),
    }
}

/// Whether a seeded slot's requirements were checked when it was seeded. Slots
/// that are not seeded have nothing to say here (their state check does).
fn requires_check(f: &PoolFacts, slot: &SlotStatus) -> Option<Check> {
    let configured = f.requires.configured;
    if configured.is_empty() || matches!(slot.state.as_str(), "seeding" | "unseeded") {
        return None;
    }
    let name = format!("requires {}", slot.name);
    let reseed = format!(
        "vivi pool destroy --pool {} --slot {} --yes, then {}",
        f.pool,
        slot.name,
        create(f.pool)
    );
    let count = configured.len();
    Some(match f.requires.marks.get(&slot.name) {
        Some(MarkView::Mark(mark)) if requires::mark_matches(configured, mark) => Check::ok(
            GROUP,
            &name,
            format!("{count} requirement(s) passed at seed time"),
        ),
        Some(MarkView::Mark(_) | MarkView::Absent) => Check::warn(
            GROUP,
            &name,
            "requirements not checked on this slot (configured, or changed, after it was seeded); re-create it",
            reseed,
        ),
        Some(MarkView::Unread(why)) => Check::skipped(GROUP, &name, format!("not read: {why}")),
        None => Check::skipped(GROUP, &name, "not read"),
    })
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
        reconcile(f.pool),
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
            "{} (records each as lost and closes it; a destroyed machine has no result to collect)",
            reconcile(f.pool)
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
        reconcile(f.pool),
    )
}

fn overdue_check(pool: &str, slots: &[SlotStatus]) -> Check {
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
        format!(
            "{} (stops a slot once the grace after its budget has passed)",
            reconcile(pool)
        ),
    )
}

fn create(pool: &str) -> String {
    format!("vivi pool create --pool {pool}")
}

fn reconcile(pool: &str) -> String {
    format!("vivi pool reconcile --pool {pool}")
}

fn age_text(secs: u64) -> String {
    match secs {
        0..120 => format!("{secs}s"),
        120..7200 => format!("{}m", secs / 60),
        _ => format!("{}h", secs / 3600),
    }
}
