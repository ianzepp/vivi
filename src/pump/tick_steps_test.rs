//! A failing step ends neither the tick nor the pump.

use std::fs;

use tempfile::tempdir;

use crate::VivariumError;
use crate::pump::boardpass::BoardPass;
use crate::pump::config::PumpConfig;
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::run::tick;
use crate::pump::state::State;
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
fn a_tick_completes_when_the_need_state_is_unreadable() {
    let dir = tempdir().unwrap();
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let ctx = ctx.with_mailspace(String::from("mailspace"));
    let mut st = State::default();
    // Every need step reads this and fails; the tick must still finish.
    st.extra
        .insert(String::from("need_branches"), serde_json::json!("garbage"));

    let (mut pass, _requests, _results) = BoardPass::detached();
    tick(&ctx, &mut st, &mut pass).expect("the tick survives failing steps");

    let raised = exceptions(&ctx);
    assert!(
        raised.contains("tick-step-failed refresh-freezes"),
        "{raised}"
    );
    assert!(
        raised.contains("tick-step-failed dispatch-landings"),
        "{raised}"
    );
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
