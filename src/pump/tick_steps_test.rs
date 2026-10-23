//! A failing step ends neither the tick nor the pump.

use std::fs;

use tempfile::tempdir;

use crate::VivariumError;
use crate::pump::boardpass::BoardPass;
use crate::pump::config::PumpConfig;
use crate::pump::fixture::project;
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::run::tick;
use crate::pump::state::State;
use crate::pump::store::{DB_FILENAME, PumpStore};
use crate::pump::tick_steps::soft_step;
use crate::pump::tick_steps::{RERAISE_SECS, should_raise};

fn exceptions(ctx: &Ctx) -> String {
    fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap_or_default()
}

#[test]
fn a_failed_step_is_logged_and_raised_as_an_exception() {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());

    let none: Option<()> = soft_step(&ctx, "demo", Err(VivariumError::Message("boom".into())));
    let some = soft_step(&ctx, "demo", Ok(3));

    assert_eq!((none, some), (None, Some(3)));
    assert!(
        host.logs()
            .iter()
            .any(|line| line.contains("tick step demo failed: ") && line.contains("boom")),
        "{:?}",
        host.logs()
    );
    let raised = exceptions(&ctx);
    assert!(raised.contains("tick-step-failed demo"), "{raised}");
}

#[test]
fn a_tick_completes_when_the_flow_index_is_unreadable() {
    let project = project();
    project.host.reply("ls-remote", 0, "");
    let ctx = project.pump_ctx();
    // Create the schema, then plant a run row whose `run` column is text: the
    // flow step reads the finishing runs, fails on it, and must not end the tick.
    drop(PumpStore::open(&ctx.home.join(DB_FILENAME)).unwrap());
    let conn = rusqlite::Connection::open(ctx.home.join(DB_FILENAME)).unwrap();
    conn.execute(
        "INSERT INTO flow_runs (need, run, flow, flow_version, kind, entry, state) \
         VALUES ('n1', 'garbage', 'build', 2, 'build', 'plan', 'finishing')",
        [],
    )
    .unwrap();
    drop(conn);
    let mut st = State::default();

    let (mut pass, _requests, _results) = BoardPass::detached();
    tick(&ctx, &mut st, &mut pass).expect("the tick survives a failing flow step");

    let raised = exceptions(&ctx);
    assert!(raised.contains("tick-step-failed flow"), "{raised}");
    assert!(ctx.home.join("status.txt").is_file());
}

#[test]
fn a_repeated_step_failure_is_raised_again_only_when_it_changes_or_ages() {
    let step = "reraise-throttle-test-step";
    assert!(should_raise(step, "boom", 1_000));
    assert!(!should_raise(step, "boom", 1_020));
    assert!(should_raise(step, "different", 1_040));
    assert!(!should_raise(step, "different", 1_040 + RERAISE_SECS - 1));
    assert!(should_raise(step, "different", 1_040 + RERAISE_SECS));
}
