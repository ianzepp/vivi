//! `vivi pool doctor --test`: a live exercise that answers two questions
//! apart. Is the machine host working (steps under the Provider heading)? Is
//! Vivi's own pool machinery working (steps under the Vivi heading)?
//!
//! The test depends on nothing of the project: it runs a throwaway pool of two
//! small slots under its own name (`<pool>-doctor`) in the real pool's machine
//! host application, delivers an embedded source tree to each slot (see
//! [`selftest_source`](super::selftest_source)), and runs a stand-in harness
//! against it. Its dispatch records live in a temporary mailspace. Cleanup
//! always runs: the slots it created are destroyed, a failed or interrupted
//! step included, and a final check lists anything left.

use std::time::Instant;

use super::config::PoolConfig;
use super::doctor::Check;
use super::progress::Progress;
use super::provider::PoolProvider;
use super::run;
use super::selftest_steps::{self as steps, StepResult};
use super::selftest_world::{Timing, World};

/// Heading of the machine-host steps.
pub const PROVIDER_GROUP: &str = "Live test: provider";
/// Heading of the steps on Vivi's own machinery.
pub const VIVI_GROUP: &str = "Live test: Vivi";

/// What `run` is given besides the provider and the config.
pub struct TestOptions<'a> {
    /// Add the step that waits past six minutes.
    pub long: bool,
    pub timing: Timing,
    /// True once the user interrupted.
    pub cancelled: &'a dyn Fn() -> bool,
    /// Called with each finished step, for progress output.
    pub progress: &'a dyn Fn(&Check),
    /// Called with the name of each step as it starts.
    pub starting: &'a dyn Fn(&str),
    /// Where the start lines and heartbeats inside a step go.
    pub detail: &'a Progress,
}

struct Step {
    group: &'static str,
    name: &'static str,
    /// Runs even after an earlier failure: the cleanup.
    always: bool,
    run: fn(&mut World) -> StepResult,
}

const fn step(group: &'static str, name: &'static str, run: fn(&mut World) -> StepResult) -> Step {
    Step {
        group,
        name,
        always: false,
        run,
    }
}

const fn cleanup(name: &'static str, run: fn(&mut World) -> StepResult) -> Step {
    Step {
        group: PROVIDER_GROUP,
        name,
        always: true,
        run,
    }
}

/// The steps in order. `--long` adds one.
fn plan(long: bool) -> Vec<Step> {
    let mut list = vec![
        step(PROVIDER_GROUP, "create two slots", steps::create_slots),
        step(PROVIDER_GROUP, "both slots reach free", steps::slots_free),
        step(
            PROVIDER_GROUP,
            "start and stop round trip",
            steps::round_trip,
        ),
        step(VIVI_GROUP, "dispatch two at once", steps::dispatch_two),
        step(
            VIVI_GROUP,
            "third dispatch is refused",
            steps::third_refused,
        ),
        step(
            VIVI_GROUP,
            "wait collects a finished job",
            steps::wait_collects,
        ),
        step(
            VIVI_GROUP,
            "a job past its budget is killed",
            steps::budget_kills,
        ),
        step(VIVI_GROUP, "continue a branch", steps::continue_branch),
        step(
            VIVI_GROUP,
            "unwatched job is collected later",
            steps::unattended,
        ),
        step(
            VIVI_GROUP,
            "reconcile stops a slot with no dispatch",
            steps::reconcile_orphan,
        ),
    ];
    if long {
        list.push(step(
            VIVI_GROUP,
            "wait past six minutes still collects",
            steps::long_wait,
        ));
    }
    list.push(cleanup("destroy the test slots", steps::destroy_slots));
    list.push(cleanup(
        "nothing of the test pool remains",
        steps::nothing_left,
    ));
    list
}

/// What to tell the reader to do about a failed step, by side.
fn fix_for(group: &str) -> &'static str {
    if group == PROVIDER_GROUP {
        "the machine host did not behave as expected; check its status and your account limits, then run `vivi pool doctor --test` again"
    } else {
        "Vivi's pool machinery did not behave as expected; run `vivi pool doctor --test` again, and report the observed text if it repeats"
    }
}

fn outcome(step: &Step, result: StepResult, took: std::time::Duration) -> Check {
    match result {
        Ok(detail) => Check::ok(step.group, step.name, detail).timed(took),
        Err(detail) => Check::fail(step.group, step.name, detail, fix_for(step.group)).timed(took),
    }
}

/// Slots left by an earlier run that was killed: remove them first.
fn clear_earlier(world: &World) -> Option<Check> {
    // A provider that cannot list fails the first step with its own error.
    let Ok(found) = run::pool_status(world.provider, &world.pool) else {
        return None;
    };
    if found.is_empty() {
        return None;
    }
    let names = World::describe(&found);
    let check = match run::destroy_slots(world.provider, &world.pool, &run::DestroyTarget::All) {
        Ok(_) => Check::warn(
            PROVIDER_GROUP,
            "earlier test slots",
            format!("removed {names} left by an earlier run"),
            "none needed; they belonged to the test pool",
        ),
        Err(e) => Check::fail(
            PROVIDER_GROUP,
            "earlier test slots",
            format!("could not remove {names}: {e}"),
            "vivi pool doctor --test will try again; or destroy them in the provider",
        ),
    };
    Some(check)
}

/// Run the live test. Never panics and never leaves its slots behind unless
/// the provider refuses to destroy them (the last step says which).
pub fn run_test(
    provider: &dyn PoolProvider,
    real: &PoolConfig,
    real_pool: &str,
    options: &TestOptions,
) -> Vec<Check> {
    let mut world = match World::new(
        provider,
        real,
        real_pool,
        options.timing.clone(),
        options.cancelled,
        options.detail,
    ) {
        Ok(world) => world,
        Err(e) => {
            let fail = Check::fail(
                VIVI_GROUP,
                "set up",
                format!("cannot create the temporary mailspace: {e}"),
                "check that the temporary directory is writable",
            );
            return vec![fail];
        }
    };
    let mut checks = Vec::new();
    let note = |check: Check, checks: &mut Vec<Check>| {
        (options.progress)(&check);
        checks.push(check);
    };
    if let Some(check) = clear_earlier(&world) {
        note(check, &mut checks);
    }
    let mut failed: Option<&'static str> = None;
    for item in plan(options.long) {
        if item.always {
            world.cleaning = true;
        } else if let Some(first) = failed {
            let reason = format!("not run: \"{first}\" failed");
            note(Check::skipped(item.group, item.name, reason), &mut checks);
            continue;
        }
        (options.starting)(item.name);
        let started = Instant::now();
        let result = if !item.always && (options.cancelled)() {
            Err("interrupted before this step ran".to_string())
        } else {
            (item.run)(&mut world)
        };
        let check = outcome(&item, result, started.elapsed());
        if check.status == super::doctor::Status::Fail && failed.is_none() {
            failed = Some(item.name);
        }
        note(check, &mut checks);
    }
    checks
}

#[cfg(test)]
#[path = "selftest_test.rs"]
mod tests;
