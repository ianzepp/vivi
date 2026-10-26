//! Tests for the admitted-plan resolution in [`super`].

use serde_json::json;
use tempfile::{TempDir, tempdir};

use super::admitted_plan;
use crate::error::VivariumError;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::store::step_payload::StepPayloadRow;
use crate::pump::store::{DB_FILENAME, PumpStore};

type R = Result<(), VivariumError>;

const NEED: &str = "need-1";

struct Fixture {
    _dir: TempDir,
    store: PumpStore,
}

fn fixture() -> Fixture {
    let dir = tempdir().expect("tempdir");
    let store = PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open");
    Fixture { _dir: dir, store }
}

fn run(f: &Fixture, number: u32, entry: &str) -> RunRow {
    RunRow {
        need: NEED.into(),
        run: number,
        flow: "build".into(),
        flow_version: 2,
        kind: "build".into(),
        entry: entry.into(),
        state: "live".into(),
    }
}

/// A done planner step `node` whose stored payload carries `body`.
fn planner_step(f: &Fixture, run: &RunRow, node: &str, body: serde_json::Value) -> R {
    let task = format!("t-{node}");
    f.store.record_step_payload(&StepPayloadRow {
        need: NEED.into(),
        run: run.run,
        node: node.into(),
        class: "planner".into(),
        task: task.clone(),
        payload: body,
    })?;
    f.store
        .insert_step(
            NEED,
            &StepRow {
                run: run.run,
                node: node.into(),
                task,
                state: StepState::Done,
                verdict: Some("planned".into()),
            },
        )
        .map_err(Into::into)
}

/// A done admitter step `node` answering `verdict`.
fn admitter_step(f: &Fixture, run: &RunRow, node: &str, verdict: &str) -> R {
    f.store
        .insert_step(
            NEED,
            &StepRow {
                run: run.run,
                node: node.into(),
                task: format!("t-{node}"),
                state: StepState::Done,
                verdict: Some(verdict.into()),
            },
        )
        .map_err(Into::into)
}

/// Run `number` entered at `entry`; its planner `planner` carries `body` and
/// its admitter `admitter` answered `verdict`.
fn seeded(
    f: &Fixture,
    number: u32,
    entry: &str,
    planner: Option<&str>,
    admitter: Option<(&str, &str)>,
) -> R {
    let run = run(f, number, entry);
    f.store.insert_run(&run)?;
    if let Some(planner) = planner {
        planner_step(
            f,
            &run,
            planner,
            json!({ "body": format!("PLAN OF {planner}") }),
        )?;
    }
    if let Some((node, verdict)) = admitter {
        admitter_step(f, &run, node, verdict)?;
    }
    Ok(())
}

#[test]
fn the_first_plan_is_the_planner_that_fed_the_admitting_admitter() -> R {
    let f = fixture();
    seeded(&f, 1, "plan", Some("plan"), Some(("admit1", "admitted")))?;

    let plan = admitted_plan(&f.store, &run(&f, 1, "plan"))?;

    assert_eq!(plan.as_deref(), Some("PLAN OF plan"));
    Ok(())
}

#[test]
fn the_revised_plan_is_the_planner_that_fed_the_admitting_admitter() -> R {
    let f = fixture();
    let run = run(&f, 1, "plan");
    f.store.insert_run(&run)?;
    planner_step(&f, &run, "plan", json!({ "body": "PLAN ONE" }))?;
    admitter_step(&f, &run, "admit1", "revise")?;
    planner_step(&f, &run, "plan2", json!({ "body": "PLAN TWO" }))?;
    admitter_step(&f, &run, "admit2", "admitted")?;

    let plan = admitted_plan(&f.store, &run)?;

    assert_eq!(plan.as_deref(), Some("PLAN TWO"));
    Ok(())
}

#[test]
fn a_planned_but_unadmitted_revision_is_not_sufficient() -> R {
    let f = fixture();
    let run = run(&f, 1, "plan");
    f.store.insert_run(&run)?;
    planner_step(&f, &run, "plan", json!({ "body": "PLAN ONE" }))?;
    admitter_step(&f, &run, "admit1", "admitted")?;
    planner_step(&f, &run, "plan2", json!({ "body": "PLAN TWO" }))?;

    let plan = admitted_plan(&f.store, &run)?;

    assert_eq!(plan.as_deref(), Some("PLAN ONE"));
    Ok(())
}

#[test]
fn the_payload_body_includes_the_routed_correction_output() -> R {
    let f = fixture();
    let run = run(&f, 1, "plan");
    f.store.insert_run(&run)?;
    // The collect path stores the corrected reply as `{front, body}`.
    planner_step(
        &f,
        &run,
        "plan",
        json!({ "front": { "answer": "planned" }, "body": "CORRECTED PLAN" }),
    )?;
    admitter_step(&f, &run, "admit1", "admitted")?;

    let plan = admitted_plan(&f.store, &run)?;

    assert_eq!(plan.as_deref(), Some("CORRECTED PLAN"));
    Ok(())
}

#[test]
fn no_admission_anywhere_is_none_not_an_error() -> R {
    let f = fixture();
    seeded(&f, 1, "plan", Some("plan"), Some(("admit1", "revise")))?;

    let plan = admitted_plan(&f.store, &run(&f, 1, "plan"))?;

    assert_eq!(plan, None);
    Ok(())
}

#[test]
fn an_admission_without_a_feeding_planner_is_an_error() -> R {
    let f = fixture();
    seeded(&f, 1, "plan", None, Some(("admit1", "admitted")))?;

    let err = admitted_plan(&f.store, &run(&f, 1, "plan")).unwrap_err();

    assert!(err.to_string().contains("no completed planner"), "{err}");
    Ok(())
}

#[test]
fn a_planner_without_a_stored_payload_is_an_error() -> R {
    let f = fixture();
    let run = run(&f, 1, "plan");
    f.store.insert_run(&run)?;
    f.store
        .insert_step(
            NEED,
            &StepRow {
                run: 1,
                node: "plan".into(),
                task: "t-plan".into(),
                state: StepState::Done,
                verdict: Some("planned".into()),
            },
        )
        .unwrap();
    admitter_step(&f, &run, "admit1", "admitted")?;

    let err = admitted_plan(&f.store, &run).unwrap_err();

    assert!(err.to_string().contains("no stored payload"), "{err}");
    Ok(())
}

#[test]
fn a_payload_without_a_body_is_an_error() -> R {
    let f = fixture();
    let run = run(&f, 1, "plan");
    f.store.insert_run(&run)?;
    planner_step(
        &f,
        &run,
        "plan",
        json!({ "front": { "answer": "planned" } }),
    )?;
    admitter_step(&f, &run, "admit1", "admitted")?;

    let err = admitted_plan(&f.store, &run).unwrap_err();

    assert!(err.to_string().contains("no stored plan body"), "{err}");
    Ok(())
}

#[test]
fn a_slice_entry_run_inherits_the_previous_runs_admission() -> R {
    let f = fixture();
    seeded(&f, 1, "plan", Some("plan"), Some(("admit1", "admitted")))?;
    seeded(&f, 2, "slice", None, None)?;

    let plan = admitted_plan(&f.store, &run(&f, 2, "slice"))?;

    assert_eq!(plan.as_deref(), Some("PLAN OF plan"));
    Ok(())
}

#[test]
fn repeated_continuations_reach_the_admitting_run() -> R {
    let f = fixture();
    seeded(&f, 1, "plan", Some("plan"), Some(("admit1", "admitted")))?;
    seeded(&f, 2, "slice", None, None)?;
    seeded(&f, 3, "rework1", None, None)?;

    let plan = admitted_plan(&f.store, &run(&f, 3, "rework1"))?;

    assert_eq!(plan.as_deref(), Some("PLAN OF plan"));
    Ok(())
}

#[test]
fn the_walk_stops_at_a_fresh_plan_run_without_admission() -> R {
    let f = fixture();
    seeded(&f, 1, "plan", Some("plan"), Some(("admit1", "admitted")))?;
    seeded(&f, 2, "plan", None, None)?;
    seeded(&f, 3, "slice", None, None)?;

    let plan = admitted_plan(&f.store, &run(&f, 3, "slice"))?;

    assert_eq!(plan, None);
    Ok(())
}

#[test]
fn the_walk_never_reads_future_runs() -> R {
    let f = fixture();
    seeded(&f, 1, "plan", Some("plan"), Some(("admit1", "revise")))?;
    seeded(&f, 2, "plan", Some("plan2"), Some(("admit2", "admitted")))?;

    let plan = admitted_plan(&f.store, &run(&f, 1, "plan"))?;

    assert_eq!(plan, None);
    assert_eq!(
        admitted_plan(&f.store, &run(&f, 2, "plan"))?.as_deref(),
        Some("PLAN OF plan2")
    );
    Ok(())
}

#[test]
fn another_needs_admission_is_never_read() -> R {
    let f = fixture();
    let other = RunRow {
        need: "need-2".into(),
        ..run(&f, 1, "plan")
    };
    f.store.insert_run(&other)?;
    f.store
        .record_step_payload(&StepPayloadRow {
            need: "need-2".into(),
            run: 1,
            node: "plan".into(),
            class: "planner".into(),
            task: "other-plan".into(),
            payload: json!({ "body": "OTHER PLAN" }),
        })
        .unwrap();
    f.store
        .insert_step(
            "need-2",
            &StepRow {
                run: 1,
                node: "plan".into(),
                task: "other-plan".into(),
                state: StepState::Done,
                verdict: Some("planned".into()),
            },
        )
        .unwrap();
    admitter_step(&f, &other, "admit1", "admitted")?;
    f.store.insert_run(&run(&f, 2, "slice"))?;

    let plan = admitted_plan(&f.store, &run(&f, 2, "slice"))?;

    // need-2's admission must not satisfy need-1's run 2.
    assert_ne!(plan.as_deref(), Some("OTHER PLAN"));
    assert_eq!(plan, None);
    Ok(())
}

#[test]
fn an_unknown_flow_kind_is_an_error() -> R {
    let f = fixture();
    let run = RunRow {
        kind: "mystery".into(),
        ..run(&f, 1, "plan")
    };

    let err = admitted_plan(&f.store, &run).unwrap_err();

    assert!(err.to_string().contains("unknown flow kind"), "{err}");
    Ok(())
}
