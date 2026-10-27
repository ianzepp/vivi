//! Tests for the pause-origin lookup and for the eligibility of held steps.

use tempfile::{TempDir, tempdir};

use super::continuation::{
    EligibleHold, HoldBlock, OriginSource, PauseOrigin, ParkResolution, eligible_hold, hold_block,
    park_origin, park_steps, run_park,
};
use crate::error::VivariumError;
use crate::pump::envelope::correct;
use crate::pump::flow::def::{FlowDef, FlowEdge, FlowKind, FlowNode, NodeClass};
use crate::pump::flow::index::{RunRow, StepJobRow, StepRow, StepState, run_state};
use crate::pump::flow::load::load;
use crate::pump::flow::scope::ScopeState;
use crate::pump::flow::tick::{OPERATOR_HOLD, SIDE_EFFECT_UNCLEAR};
use crate::pump::jobs::{JobSpec, PoolJob};
use crate::pump::state::State;
use crate::pump::store::append_exception;
use crate::pump::store::recovery::pause::{PauseBoundaryRow, pause_state};
use crate::pump::store::{DB_FILENAME, PumpStore};

type R = Result<(), VivariumError>;

const NEED: &str = "n1";

fn open(dir: &TempDir) -> PumpStore {
    PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open")
}

fn build_run(run: u32) -> RunRow {
    RunRow {
        need: NEED.into(),
        run,
        flow: "build".into(),
        flow_version: 2,
        kind: "build".into(),
        entry: "plan".into(),
        state: run_state::LIVE.into(),
    }
}

fn step(
    run: u32,
    node: &str,
    task: &str,
    state: StepState,
    verdict: Option<&str>,
) -> StepRow {
    StepRow {
        run,
        node: node.into(),
        task: task.into(),
        state,
        verdict: verdict.map(str::to_string),
    }
}

fn held(node: &str, task: &str) -> StepRow {
    step(1, node, task, StepState::Waiting, Some(OPERATOR_HOLD))
}

/// The run's rows of a park at `plan2`: plan planned, admit1 asked for a
/// revision, plan2 answered `operator` and the park task waits.
fn plan2_park_steps() -> Vec<StepRow> {
    vec![
        step(1, "plan", "t-plan", StepState::Done, Some("planned")),
        step(1, "admit1", "t-admit1", StepState::Done, Some("revise")),
        step(1, "plan2", "t-plan2", StepState::Done, Some("operator")),
        step(1, "parked", "t-park", StepState::Waiting, None),
    ]
}

/// A park boundary of run 1, recorded for `origin` at `node` through `gate`.
fn boundary(node: &str, origin: &str, gate: &str) -> PauseBoundaryRow {
    PauseBoundaryRow {
        need: NEED.into(),
        run: 1,
        node: node.into(),
        origin_task: origin.into(),
        gate: gate.into(),
        pause_generation: 1,
        input_generation: 4,
        role_request: format!("step-{origin}-a1"),
        boundary_message: None,
        paused_at: 1_700,
        state: pause_state::OPEN.into(),
    }
}

/// A store with run 1 of NEED holding `steps`, all indexed.
fn rig(steps: &[StepRow]) -> (TempDir, PumpStore) {
    let dir = tempdir().expect("tempdir");
    let store = open(&dir);
    let run = build_run(1);
    store.insert_run(&run).expect("insert run");
    for row in steps {
        store.insert_run_step(&run, row).expect("insert step");
    }
    (dir, store)
}

fn expected_origin(node: &str, task: &str, gate: &str, source: OriginSource) -> ParkResolution {
    ParkResolution::Origin(PauseOrigin {
        need: NEED.into(),
        run: 1,
        node: node.into(),
        origin_task: task.into(),
        gate: gate.into(),
        source,
    })
}

/// A read-only role request for `task` in `role`, as the role handlers mint
/// it.
fn role_request(task: &str, role: &str) -> JobSpec {
    let mut spec = JobSpec::new(
        format!("step-{task}-a1"),
        role,
        "ada",
        format!("{role} (n1 r1)"),
        "brief",
    );
    spec.read_only = true;
    spec.from_ref = Some("main".into());
    spec
}

/// Index `spec` as attempt 1 of step `task` at `node`.
fn index_attempt(store: &PumpStore, spec: &JobSpec, node: &str, task: &str) -> R {
    store.insert_step_job(
        NEED,
        &StepJobRow {
            job: spec.id.clone(),
            run: 1,
            node: node.into(),
            task: task.into(),
            attempt: 1,
        },
    )
}

#[test]
fn recorded_plan2_origin_names_the_exact_revision_step() -> R {
    let dir = tempdir()?;
    let store = open(&dir);
    let def = load(FlowKind::Build)?;
    let steps = plan2_park_steps();
    assert_eq!(park_steps(&def, &steps).len(), 1, "one live park row");
    let stored = store.record_pause(&boundary("plan2", "t-plan2", "pq2"))?;
    let run = build_run(1);
    assert_eq!(
        park_origin(&store, &def, &run, &steps)?,
        expected_origin(
            "plan2",
            "t-plan2",
            "pq2",
            OriginSource::Recorded(Box::new(stored))
        )
    );
    Ok(())
}

#[test]
fn recorded_origin_beats_an_ambiguous_legacy_index() -> R {
    let dir = tempdir()?;
    let store = open(&dir);
    let def = load(FlowKind::Build)?;
    // Both planner steps match the park words; only plan2 carries a record.
    let mut steps = plan2_park_steps();
    steps[0] = step(1, "plan", "t-plan", StepState::Done, Some("operator"));
    store.record_pause(&boundary("plan2", "t-plan2", "pq2"))?;
    let run = build_run(1);
    let resolved = park_origin(&store, &def, &run, &steps)?;
    let ParkResolution::Origin(origin) = resolved else {
        panic!("the record must resolve the park");
    };
    assert_eq!(origin.node, "plan2");
    assert_eq!(origin.origin_task, "t-plan2");
    assert_eq!(origin.record().map(|row| row.gate.as_str()), Some("pq2"));
    Ok(())
}

#[test]
fn recorded_nonplanner_origin_names_the_research_step() -> R {
    let dir = tempdir()?;
    let store = open(&dir);
    let def = research_def();
    let steps = vec![
        step(1, "res", "t-res", StepState::Done, Some("NOT READY")),
        step(1, "park", "t-park", StepState::Waiting, None),
    ];
    let stored = store.record_pause(&boundary("res", "t-res", "g"))?;
    let run = RunRow {
        kind: "research".into(),
        flow: "research".into(),
        ..build_run(1)
    };
    assert_eq!(
        park_origin(&store, &def, &run, &steps)?,
        expected_origin(
            "res",
            "t-res",
            "g",
            OriginSource::Recorded(Box::new(stored))
        )
    );
    Ok(())
}

#[test]
fn unambiguous_legacy_recovery_names_the_only_park_word_step() -> R {
    let dir = tempdir()?;
    let store = open(&dir);
    let def = load(FlowKind::Build)?;
    // A historical park: plan answered `operator`, nothing was recorded, and
    // a retired boundary from an earlier park of the same step stays a
    // receipt, not the live park's record.
    let steps = vec![
        step(1, "plan", "t-plan", StepState::Done, Some("operator")),
        step(1, "parked", "t-park", StepState::Waiting, None),
    ];
    store.record_pause(&boundary("plan", "t-plan", "pq"))?;
    store.retire_pause(NEED, 1, "plan", "t-plan", 1)?;
    let run = build_run(1);
    assert_eq!(
        park_origin(&store, &def, &run, &steps)?,
        expected_origin("plan", "t-plan", "pq", OriginSource::Legacy)
    );
    Ok(())
}

#[test]
fn run_park_resolves_a_stored_run() -> R {
    let steps = vec![
        step(1, "plan", "t-plan", StepState::Done, Some("operator")),
        step(1, "parked", "t-park", StepState::Waiting, None),
    ];
    let (_dir, store) = rig(&steps);
    let run = store.run_row(NEED, 1)?;
    assert_eq!(
        run_park(&store, &run)?,
        expected_origin("plan", "t-plan", "pq", OriginSource::Legacy)
    );
    Ok(())
}

#[test]
fn ambiguous_legacy_refusal_leaves_the_park_with_a_diagnostic() -> R {
    let dir = tempdir()?;
    let store = open(&dir);
    let def = load(FlowKind::Build)?;
    // Both planner steps carry a park verdict and neither has a record: the
    // newest planner is never assumed.
    let mut steps = plan2_park_steps();
    steps[0] = step(1, "plan", "t-plan", StepState::Done, Some("operator"));
    let run = build_run(1);
    let ParkResolution::Parked(text) = park_origin(&store, &def, &run, &steps)? else {
        panic!("an ambiguous legacy park must stay parked");
    };
    assert!(text.contains("r1 parks at `parked`"), "{text}");
    assert!(text.contains("plan (t-plan) and plan2 (t-plan2)"), "{text}");
    assert!(text.contains("unresolved"), "{text}");
    Ok(())
}

#[test]
fn missing_legacy_origin_leaves_the_park_with_a_diagnostic() -> R {
    let dir = tempdir()?;
    let store = open(&dir);
    let def = load(FlowKind::Build)?;
    // The park row waits, but no completed step of the run took a park word.
    let steps = vec![
        step(1, "plan", "t-plan", StepState::Done, Some("planned")),
        step(1, "parked", "t-park", StepState::Waiting, None),
    ];
    let run = build_run(1);
    let ParkResolution::Parked(text) = park_origin(&store, &def, &run, &steps)? else {
        panic!("a park with no origin evidence must stay parked");
    };
    assert!(text.contains("no completed step"), "{text}");
    Ok(())
}

#[test]
fn a_run_without_a_park_row_is_not_parked() -> R {
    let dir = tempdir()?;
    let store = open(&dir);
    let def = load(FlowKind::Build)?;
    let steps = vec![
        step(1, "plan", "t-plan", StepState::Done, Some("planned")),
        step(1, "admit1", "t-admit1", StepState::Pending, None),
    ];
    assert!(park_steps(&def, &steps).is_empty());
    assert_eq!(
        park_origin(&store, &def, &build_run(1), &steps)?,
        ParkResolution::NotParked
    );
    Ok(())
}

#[test]
fn held_admitter_is_eligible_with_its_original_request() -> R {
    let steps = vec![held("admit1", "t-admit1")];
    let (_dir, store) = rig(&steps);
    let def = load(FlowKind::Build)?;
    let mut st = State::default();
    let spec = role_request("t-admit1", "admitter");
    // The envelope-fix correction of the same step is never the request.
    let mut fix = JobSpec::new(
        format!("{}-c1", spec.id),
        correct::KIND,
        "ada",
        "envelope fix: admit1",
        "body",
    );
    fix.read_only = true;
    st.jobs.insert(spec.id.clone(), PoolJob::from(spec.clone()));
    st.jobs.insert(fix.id.clone(), PoolJob::from(fix));
    index_attempt(&store, &spec, "admit1", "t-admit1")?;
    let row = StepJobRow {
        job: format!("{}-c1", spec.id),
        run: 1,
        node: "admit1".into(),
        task: "t-admit1".into(),
        attempt: 1,
    };
    store.insert_step_job(NEED, &row)?;
    assert_eq!(hold_block(&store, &st, &def, &steps[0])?, None);
    assert_eq!(
        eligible_hold(&store, &st, "t-admit1")?,
        Some(EligibleHold {
            need: NEED.into(),
            run: 1,
            node: "admit1".into(),
            task: "t-admit1".into(),
            request: PoolJob::from(spec),
        })
    );
    Ok(())
}

#[test]
fn held_auditor_is_eligible() -> R {
    let steps = vec![
        step(1, "build1", "t-build1", StepState::Done, Some("built")),
        held("audit1", "t-audit1"),
    ];
    let (_dir, store) = rig(&steps);
    let def = load(FlowKind::Build)?;
    let mut st = State::default();
    let spec = role_request("t-audit1", "auditor");
    st.jobs.insert(spec.id.clone(), PoolJob::from(spec.clone()));
    index_attempt(&store, &spec, "audit1", "t-audit1")?;
    let hold = eligible_hold(&store, &st, "t-audit1")?.expect("eligible");
    assert_eq!(hold.node, "audit1");
    assert_eq!(hold.request.id, "step-t-audit1-a1");
    assert_eq!(hold.request.read_only, true);
    assert_eq!(hold_block(&store, &st, &def, &steps[1])?, None);
    Ok(())
}

#[test]
fn command_and_effectful_requests_stay_with_the_operator() -> R {
    let dir = tempdir()?;
    let store = open(&dir);
    let def = load(FlowKind::Build)?;
    let step_row = held("admit1", "t-admit1");

    let mut command = role_request("t-admit1", "admitter");
    command.command = Some("make test".into());
    let mut st = State::default();
    st.jobs.insert(command.id.clone(), PoolJob::from(command));
    assert_eq!(
        hold_block(&store, &st, &def, &step_row)?,
        Some(HoldBlock::Command)
    );

    let mut effectful = role_request("t-admit1", "admitter");
    effectful.read_only = false;
    let mut st = State::default();
    st.jobs.insert(effectful.id.clone(), PoolJob::from(effectful));
    assert_eq!(
        hold_block(&store, &st, &def, &step_row)?,
        Some(HoldBlock::Effectful)
    );
    assert_eq!(eligible_hold(&store, &st, "t-admit1")?, None);
    Ok(())
}

#[test]
fn a_step_without_any_job_is_unbacked() -> R {
    let dir = tempdir()?;
    let store = open(&dir);
    let def = load(FlowKind::Build)?;
    let st = State::default();
    assert_eq!(
        hold_block(&store, &st, &def, &held("admit1", "t-admit1"))?,
        Some(HoldBlock::Unbacked)
    );
    Ok(())
}

#[test]
fn scope_waits_stay_with_the_slicer() -> R {
    let dir = tempdir()?;
    let store = open(&dir);
    let def = load(FlowKind::Build)?;
    let mut st = State::default();
    let spec = role_request("t-admit1", "admitter");
    st.jobs.insert(spec.id.clone(), PoolJob::from(spec));
    store.insert_slice_scope("t-admit1", 1, &["src/".to_string()])?;
    store.set_scope_state("t-admit1", ScopeState::Wait)?;
    assert_eq!(
        hold_block(&store, &st, &def, &held("admit1", "t-admit1"))?,
        Some(HoldBlock::ScopeWait)
    );
    store.set_scope_state("t-admit1", ScopeState::Rework)?;
    assert_eq!(
        hold_block(&store, &st, &def, &held("admit1", "t-admit1"))?,
        Some(HoldBlock::ScopeWait)
    );
    Ok(())
}

#[test]
fn milestones_and_locks_never_continue() -> R {
    let dir = tempdir()?;
    let store = open(&dir);
    let st = State::default();
    let def = FlowDef {
        kind: FlowKind::Build,
        nodes: vec![
            FlowNode {
                id: "gate".into(),
                class: NodeClass::Milestone,
                label: "Gate".into(),
            },
            FlowNode {
                id: "lock".into(),
                class: NodeClass::Lock,
                label: "Locked".into(),
            },
        ],
        edges: vec![],
        entries: vec![],
    };
    assert_eq!(
        hold_block(&store, &st, &def, &held("gate", "t-gate"))?,
        Some(HoldBlock::Milestone)
    );
    assert_eq!(
        hold_block(&store, &st, &def, &held("lock", "t-lock"))?,
        Some(HoldBlock::Lock)
    );
    Ok(())
}

#[test]
fn uncertain_effect_holds_stay_with_the_operator_until_superseded() -> R {
    let dir = tempdir()?;
    let store = open(&dir);
    let def = load(FlowKind::Build)?;
    let mut st = State::default();
    let spec = role_request("t-admit1", "admitter");
    st.jobs.insert(spec.id.clone(), PoolJob::from(spec));

    // Another unit's exception never counts.
    append_exception(dir.path(), 1, SIDE_EFFECT_UNCLEAR, "t-other", "unclear");
    assert_eq!(
        hold_block(&store, &st, &def, &held("admit1", "t-admit1"))?,
        None
    );

    // The newest exception of the step decides.
    append_exception(dir.path(), 2, SIDE_EFFECT_UNCLEAR, "t-admit1", "unclear");
    assert_eq!(
        hold_block(&store, &st, &def, &held("admit1", "t-admit1"))?,
        Some(HoldBlock::UncertainEffect)
    );
    append_exception(dir.path(), 3, "flow-step-error-held", "t-admit1", "held");
    assert_eq!(
        hold_block(&store, &st, &def, &held("admit1", "t-admit1"))?,
        None
    );
    Ok(())
}

#[test]
fn non_role_and_non_hold_waits_never_continue() -> R {
    let dir = tempdir()?;
    let store = open(&dir);
    let def = load(FlowKind::Build)?;
    let st = State::default();
    // A rung row held for the operator is no role step.
    assert_eq!(
        hold_block(&store, &st, &def, &held("test1", "t-test1"))?,
        Some(HoldBlock::NotRole)
    );
    // A waiting row that is not an operator hold.
    assert_eq!(
        hold_block(
            &store,
            &st,
            &def,
            &step(1, "parked", "t-park", StepState::Waiting, None)
        )?,
        Some(HoldBlock::NotOperatorHold)
    );
    // A step that is not waiting at all.
    assert_eq!(
        hold_block(
            &store,
            &st,
            &def,
            &step(1, "admit1", "t-admit1", StepState::Running, None)
        )?,
        Some(HoldBlock::NotWaiting)
    );
    Ok(())
}

#[test]
fn the_block_words_name_every_reason() {
    let words = [
        (HoldBlock::NotWaiting, "not-waiting"),
        (HoldBlock::NotOperatorHold, "not-operator-hold"),
        (HoldBlock::NotRole, "not-role"),
        (HoldBlock::Milestone, "milestone"),
        (HoldBlock::Lock, "lock"),
        (HoldBlock::ScopeWait, "scope-wait"),
        (HoldBlock::UncertainEffect, "uncertain-effect"),
        (HoldBlock::Unbacked, "unbacked"),
        (HoldBlock::Command, "command"),
        (HoldBlock::Effectful, "effectful"),
    ];
    for (block, word) in words {
        assert_eq!(block.word(), word);
    }
}

#[test]
fn an_unknown_task_is_no_continuation_target() -> R {
    let dir = tempdir()?;
    let store = open(&dir);
    let st = State::default();
    assert_eq!(eligible_hold(&store, &st, "t-absent")?, None);
    Ok(())
}

/// A research flow whose park is fed by the researcher step through one
/// decision gate.
fn research_def() -> FlowDef {
    FlowDef {
        kind: FlowKind::Research,
        nodes: vec![
            FlowNode {
                id: "res".into(),
                class: NodeClass::Researcher,
                label: "Research".into(),
            },
            FlowNode {
                id: "g".into(),
                class: NodeClass::Decision,
                label: "analysis".into(),
            },
            FlowNode {
                id: "park".into(),
                class: NodeClass::Park,
                label: "Parked".into(),
            },
        ],
        edges: vec![
            FlowEdge {
                from: "res".into(),
                to: "g".into(),
                words: vec![],
            },
            FlowEdge {
                from: "g".into(),
                to: "park".into(),
                words: ["NOT READY", "hold"].iter().map(|w| w.to_string()).collect(),
            },
        ],
        entries: vec!["res".into()],
    }
}
