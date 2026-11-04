//! The steps of the live test. Each returns `Ok(detail)` or an `Err` that
//! says what was expected and what was observed.

use std::time::{Duration, Instant};

use super::github::Secret;
use super::run;
use super::selftest_source::{LINGER_SECS, MAIN_BRANCH};
use super::selftest_world::{LONG_JOB_SECS, World};

/// The result of one step.
pub(super) type StepResult = Result<String, String>;

/// Budget of the job that is killed, in seconds, and how long it would run.
const SHORT_BUDGET: u64 = 8;
const SLEEPER_SECS: u64 = 120;
/// Budget of an ordinary stand-in job.
const JOB_BUDGET: u64 = 120;
/// A killed job must have run well under this long.
const KILLED_WITHIN: u64 = 60;

const FIRST_BRANCH: &str = "doctor/first";

pub(super) fn observed(what: &str, saw: impl std::fmt::Display) -> String {
    format!("expected {what}; observed {saw}")
}

/// Provider: create two slots and see each seed cleanly.
pub(super) fn create_slots(w: &mut World) -> StepResult {
    let seed = || Ok((Secret::new(String::new()), MAIN_BRANCH.to_string()));
    let report = run::create_slots(
        w.provider,
        &w.config,
        &w.id,
        &w.timing.create,
        w.progress,
        seed,
    )
    .map_err(|e| observed("two slots created", format!("create failed: {e}")))?;
    let bad: Vec<String> = report
        .created
        .iter()
        .filter(|o| !o.seeded())
        .map(|o| {
            format!(
                "{} state {} exit {:?} timed_out {} {}",
                o.name,
                o.state,
                o.exit_code,
                o.timed_out,
                o.error.as_deref().unwrap_or("")
            )
        })
        .collect();
    if report.created.len() != 2 || !bad.is_empty() {
        let saw = format!(
            "{} created; not seeded: {}",
            report.created.len(),
            bad.join("; ")
        );
        return Err(observed("two slots seeded cleanly and stopped", saw));
    }
    let slowest = report.created.iter().map(|o| o.seconds).max().unwrap_or(0);
    Ok(format!("2 slots seeded, slowest {slowest}s"))
}

/// Provider: both slots read `free`.
pub(super) fn slots_free(w: &mut World) -> StepResult {
    let slots = w.until("both slots to read free", w.timing.settle, || {
        let slots = w.slots()?;
        Ok((slots.len() == 2 && slots.iter().all(|s| s.state == "free")).then_some(slots))
    });
    match slots {
        Ok(slots) => Ok(World::describe(&slots)),
        Err(e) => Err(observed(
            "two free slots",
            format!("{e} ({})", describe_now(w)),
        )),
    }
}

fn describe_now(w: &World) -> String {
    w.slots().map_or_else(|e| e, |s| World::describe(&s))
}

/// Provider: start a free slot, see it run, stop it, see it stopped.
pub(super) fn round_trip(w: &mut World) -> StepResult {
    let id = w
        .slots()?
        .first()
        .map(|s| s.id.clone())
        .ok_or("no slot to start")?;
    let state = |w: &World| {
        w.provider
            .machine(&id)
            .map(|m| m.state)
            .map_err(|e| e.to_string())
    };
    w.provider
        .start(&id)
        .map_err(|e| observed("start accepted", e))?;
    w.until("the machine to read started", w.timing.settle, || {
        Ok((state(w)? == "started").then_some(()))
    })
    .map_err(|e| observed("a started machine", e))?;
    w.provider
        .stop(&id)
        .map_err(|e| observed("stop accepted", e))?;
    w.until("the machine to read stopped", w.timing.settle, || {
        Ok((state(w)? == "stopped").then_some(()))
    })
    .map_err(|e| observed("a stopped machine", e))?;
    Ok("started, then stopped".to_string())
}

/// Vivi: two dispatches in a row take two different slots.
pub(super) fn dispatch_two(w: &mut World) -> StepResult {
    let first = w
        .dispatch("first", 0, JOB_BUDGET, (MAIN_BRANCH, FIRST_BRANCH))
        .map_err(|e| observed("the first dispatch to start", e))?;
    let second = w
        .dispatch(
            "budget",
            SLEEPER_SECS,
            SHORT_BUDGET,
            (MAIN_BRANCH, "doctor/budget"),
        )
        .map_err(|e| observed("the second dispatch to start", e))?;
    if first.machine == second.machine {
        return Err(observed(
            "two different slots",
            format!("both dispatches on {}", first.slot),
        ));
    }
    Ok(format!("{} and {}", first.slot, second.slot))
}

/// Vivi: with every slot taken, a third dispatch is refused.
pub(super) fn third_refused(w: &mut World) -> StepResult {
    match w.dispatch("third", 0, JOB_BUDGET, (MAIN_BRANCH, "doctor/third")) {
        Err(e) if e.to_string().contains("no free slot") => Ok(format!("refused: {e}")),
        Err(e) => Err(observed(
            "a \"no free slot\" refusal",
            format!("another error: {e}"),
        )),
        Ok(made) => Err(observed(
            "a refusal",
            format!("a dispatch to {}", made.slot),
        )),
    }
}

pub(super) fn require_pushed(
    w: &World,
    label: &str,
    line_state: &str,
) -> Result<std::collections::BTreeMap<String, String>, String> {
    let line = w.wait_for(label, w.timing.job)?;
    let fields = w.collected(label)?;
    let push = fields.get("push").map_or("", String::as_str);
    if line.state != line_state || push != "pushed" {
        let saw = format!("{} ({}); push={push}", line.state, line.detail);
        return Err(observed("a finished job that pushed", saw));
    }
    Ok(fields)
}

/// Vivi: `wait` collects a job that finished during the linger.
pub(super) fn wait_collects(w: &mut World) -> StepResult {
    let fields = require_pushed(w, "first", "finished")?;
    let sha = fields.get("sha").map_or("", String::as_str);
    if fields.get("branch").map(String::as_str) != Some(FIRST_BRANCH) || sha.is_empty() {
        return Err(observed(
            &format!("a push of {FIRST_BRANCH} with a commit"),
            format!("{fields:?}"),
        ));
    }
    let slot = &w.job("first")?.record.slot;
    let slots = w.slots()?;
    let freed = slots.iter().any(|s| &s.name == slot && s.state == "free");
    if !freed {
        return Err(observed(
            "the slot free after collection",
            World::describe(&slots),
        ));
    }
    Ok(format!(
        "pushed {FIRST_BRANCH} {}, slot {slot} free",
        &sha[..sha.len().min(10)]
    ))
}

/// Vivi: a job that outlives its budget is killed and recorded.
pub(super) fn budget_kills(w: &mut World) -> StepResult {
    let line = w.wait_for("budget", w.timing.job)?;
    let fields = w.collected("budget")?;
    let get = |k: &str| fields.get(k).map_or(String::new(), Clone::clone);
    let rc: i64 = get("agent_rc").parse().unwrap_or(-1);
    let ran: u64 = get("agent_s").parse().unwrap_or(u64::MAX);
    let killed = matches!(rc, 124 | 137) && ran < KILLED_WITHIN;
    if line.state != "finished" || !killed {
        let saw = format!("{} with agent_rc={rc} agent_s={ran}", line.state);
        return Err(observed(
            &format!(
                "a job killed near its {SHORT_BUDGET}s budget (exit 124 or 137, under {KILLED_WITHIN}s)"
            ),
            saw,
        ));
    }
    Ok(format!(
        "killed with exit {rc} after {ran}s of a {SHORT_BUDGET}s budget"
    ))
}

/// Vivi: a second dispatch continues a branch on the slot that holds it.
/// It also starts the job the next step leaves unattended, so that job runs
/// while this one is waited for.
pub(super) fn continue_branch(w: &mut World) -> StepResult {
    let held = w.job("first")?.record.slot.clone();
    let cont = w
        .dispatch("continue", 0, JOB_BUDGET, (FIRST_BRANCH, FIRST_BRANCH))
        .map_err(|e| observed("the continuing dispatch to start", e))?;
    if cont.slot != held {
        return Err(observed(
            &format!(
                "the continuing job on {held}, which holds {FIRST_BRANCH} (the remote is per slot)"
            ),
            format!("it landed on {}", cont.slot),
        ));
    }
    w.dispatch(
        "unattended",
        0,
        JOB_BUDGET,
        (MAIN_BRANCH, "doctor/unattended"),
    )
    .map_err(|e| observed("the unattended job to start", e))?;
    let fields = require_pushed(w, "continue", "finished")?;
    let before = w
        .collected("first")?
        .get("sha")
        .cloned()
        .unwrap_or_default();
    let (start, sha) = (
        fields.get("start").cloned().unwrap_or_default(),
        fields.get("sha").cloned().unwrap_or_default(),
    );
    if start != before || sha.is_empty() || sha == before {
        let saw = format!("start {start}, new commit {sha}");
        return Err(observed(&format!("a new commit on top of {before}"), saw));
    }
    Ok(format!(
        "{FIRST_BRANCH}: {} -> {}",
        &before[..before.len().min(10)],
        &sha[..sha.len().min(10)]
    ))
}

/// Vivi: a job that finishes while nobody waits is collected from the
/// stopped slot, and the slot ends stopped and free.
pub(super) fn unattended(w: &mut World) -> StepResult {
    let record = w.job("unattended")?.record.clone();
    w.until(
        "the unwatched slot to stop by itself",
        w.timing.stops_by_itself,
        || {
            let state = w
                .provider
                .machine(&record.machine)
                .map_err(|e| e.to_string())?
                .state;
            Ok((state == "stopped").then_some(()))
        },
    )
    .map_err(|e| {
        observed(
            &format!("the slot to stop itself after the {LINGER_SECS}s linger"),
            e,
        )
    })?;
    let slots = w.slots()?;
    let waiting = slots
        .iter()
        .any(|s| s.name == record.slot && s.state == "finished-uncollected");
    if !waiting {
        return Err(observed(
            "the slot finished-uncollected",
            World::describe(&slots),
        ));
    }
    w.until("reconcile to collect the result", w.timing.settle, || {
        w.reconcile()?;
        Ok((!w.is_open("unattended")?).then_some(()))
    })
    .map_err(|e| observed("reconcile to collect the stopped slot's result", e))?;
    let fields = w.collected("unattended")?;
    let slots = w.slots()?;
    let free = slots
        .iter()
        .any(|s| s.name == record.slot && s.state == "free");
    if fields.get("push").map(String::as_str) != Some("pushed") || !free {
        let saw = format!("push={:?}; {}", fields.get("push"), World::describe(&slots));
        return Err(observed(
            "a pushed result and the slot stopped and free",
            saw,
        ));
    }
    Ok(format!("collected from stopped {}; slot free", record.slot))
}

/// Vivi: a running slot with no dispatch is found and stopped by reconcile.
pub(super) fn reconcile_orphan(w: &mut World) -> StepResult {
    let slot = w.slots()?.into_iter().next().ok_or("no slot to start")?;
    w.provider
        .start(&slot.id)
        .map_err(|e| observed("start accepted", e))?;
    w.until("the slot to read busy", w.timing.settle, || {
        let now = w.slots()?;
        Ok(now
            .iter()
            .any(|s| s.id == slot.id && s.state == "busy")
            .then_some(()))
    })
    .map_err(|e| observed("a running slot with no dispatch", e))?;
    let report = w.reconcile()?;
    let stopped = report
        .out
        .iter()
        .any(|l| l.contains("orphan-stopped") && l.contains(&slot.name));
    let slots = w.slots()?;
    let free = slots.iter().any(|s| s.id == slot.id && s.state == "free");
    if !stopped || !free {
        let saw = format!(
            "reconcile said {:?}; {}",
            report.out,
            World::describe(&slots)
        );
        return Err(observed(
            "reconcile to stop the orphan and leave the slot free",
            saw,
        ));
    }
    Ok(format!(
        "{} found running with no task, stopped, free",
        slot.name
    ))
}

/// Vivi (`--long`): a `wait` that runs past the provider token's six minutes
/// still collects.
pub(super) fn long_wait(w: &mut World) -> StepResult {
    w.dispatch(
        "long",
        LONG_JOB_SECS,
        LONG_JOB_SECS + 120,
        (MAIN_BRANCH, "doctor/long"),
    )
    .map_err(|e| observed("the long job to start", e))?;
    let started = Instant::now();
    let line = w.wait_for("long", w.timing.long_wait)?;
    let took = started.elapsed();
    let push = w
        .collected("long")?
        .get("push")
        .cloned()
        .unwrap_or_default();
    if line.state != "finished" || push != "pushed" {
        let saw = format!(
            "{} ({}); push={push} after {}s",
            line.state,
            line.detail,
            took.as_secs()
        );
        return Err(observed("the long job finished and pushed", saw));
    }
    Ok(format!("one wait of {}s collected the job", took.as_secs()))
}

/// Provider: destroy every slot of the test pool. Runs even after a failure.
pub(super) fn destroy_slots(w: &mut World) -> StepResult {
    let gone = run::destroy_slots(w.provider, &w.id, &run::DestroyTarget::All)
        .map_err(|e| observed("every test slot destroyed", e))?;
    Ok(format!("destroyed {} slot(s)", gone.len()))
}

/// Provider: nothing of the test pool remains. Runs even after a failure.
pub(super) fn nothing_left(w: &mut World) -> StepResult {
    let left = w.until(
        "the test pool to be empty",
        Duration::from_mins(1).min(w.timing.settle),
        || {
            let slots = w.slots()?;
            Ok(slots.is_empty().then_some(()))
        },
    );
    if left.is_ok() {
        return Ok(format!("no machine of pool {} remains", w.id.pool));
    }
    let slots = w.slots()?;
    let commands: Vec<String> = slots
        .iter()
        .map(|s| w.provider.destroy_command(&s.id))
        .collect();
    Err(observed(
        "no machine of the test pool left",
        format!(
            "{} remain; remove them with: {}",
            World::describe(&slots),
            commands.join(" ; ")
        ),
    ))
}
