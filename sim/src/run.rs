//! Drive the real pump run loop over a scenario's virtual time, then check
//! the scenario's expectations.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Instant;

use vivi_core::pump::boardpass::BoardPass;
use vivi_core::pump::run::{Carry, step};
use vivi_core::pump::state::State;
use vivi_core::pump::store::{DB_FILENAME, PumpStore};

use crate::host::Record;
use crate::project::{self, Project};
use crate::scenario::Scenario;

/// What one scenario run produced.
pub struct Outcome {
    pub name: String,
    pub description: String,
    pub ticks: u64,
    pub virtual_secs: u64,
    pub wall_secs: f64,
    pub tick_errors: Vec<String>,
    pub record: Record,
    /// Expectations that did not hold; empty means the scenario passed.
    pub failures: Vec<String>,
}

/// Run `scenario` to the end of its virtual time.
pub fn run(scenario: &Scenario) -> Result<Outcome, String> {
    let mut project = project::build(scenario)?;
    let started = Instant::now();
    let ticks = scenario.run.virtual_hours * 3600 / scenario.run.tick_secs.max(1);
    let tick_errors = drive(&mut project, ticks, scenario.run.tick_secs);
    let wall_secs = started.elapsed().as_secs_f64();
    let record = project.host.take_record();
    let mut outcome = Outcome {
        name: scenario.name.clone(),
        description: scenario.description.clone(),
        ticks,
        virtual_secs: ticks * scenario.run.tick_secs,
        wall_secs,
        tick_errors,
        record,
        failures: Vec::new(),
    };
    outcome.failures = check(scenario, &outcome);
    Ok(outcome)
}

/// Run `ticks` passes of the pump's run loop (`run::step`: reload, control
/// check, config refresh, recorded tick), advancing the virtual clock between
/// passes where the real pump would sleep. Returns one line per pass that
/// errored or panicked.
///
/// Like the pump's own run loop, the store is opened once and held for the
/// whole run, so every read and save inside a pass reuses that connection.
fn drive(project: &mut Project, ticks: u64, tick_secs: u64) -> Vec<String> {
    let store = match PumpStore::open(&project.ctx.home.join(DB_FILENAME)) {
        Ok(store) => store,
        Err(e) => return vec![format!("open pump.db: {e}")],
    };
    let mut st = store.load().unwrap_or_default();
    let mut boardpass = BoardPass::start(&project.ctx);
    let mut carry = Carry::default();
    let mut errors = Vec::new();
    for n in 0..ticks {
        let ctx = &mut project.ctx;
        match catch_unwind(AssertUnwindSafe(|| {
            step(ctx, &mut st, &mut boardpass, &mut carry)
        })) {
            Ok(Ok(false)) => {}
            Ok(Ok(true)) => {
                errors.push(format!("tick {n}: control asked the pump to exit"));
                break;
            }
            Ok(Err(e)) => errors.push(format!("tick {n}: {e}")),
            Err(_) => errors.push(format!("tick {n}: panic")),
        }
        project.host.advance(tick_secs);
    }
    drop(store);
    errors
}

fn check(scenario: &Scenario, outcome: &Outcome) -> Vec<String> {
    let expect = &scenario.expect;
    let mut failures = Vec::new();
    let mut bound = |what: &str, got: usize, max: usize| {
        if got > max {
            failures.push(format!("{what}: {got} (at most {max})"));
        }
    };
    bound(
        "exceptions",
        outcome.record.exceptions.len(),
        expect.exceptions,
    );
    bound("tick errors", outcome.tick_errors.len(), expect.tick_errors);
    if let Some(max) = expect.commands {
        bound("commands", outcome.record.commands, max);
    }
    if outcome.wall_secs > expect.max_wall_secs as f64 {
        failures.push(format!(
            "wall time: {:.1}s (at most {}s)",
            outcome.wall_secs, expect.max_wall_secs
        ));
    }
    failures
}
