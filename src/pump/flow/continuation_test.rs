//! Tests for the shared conversational-pause lookups.

use std::collections::BTreeMap;

use tempfile::{TempDir, tempdir};

use super::{Origin, OriginSource, OriginStep, held_request, origin_of, step_request};
use crate::pump::flow::def::{FlowDef, FlowEdge, FlowKind, FlowNode, NodeClass};
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::scope::ScopeState;
use crate::pump::flow::tick::{OPERATOR_HOLD, SIDE_EFFECT_UNCLEAR};
use crate::pump::jobs::{JobSpec, JobStore, PoolJob};
use crate::pump::store::append_exception;
use crate::pump::store::recovery::pause::{PauseBoundaryRow, pause_state};
use crate::pump::store::{DB_FILENAME, PumpStore};

const NEED: &str = "n1";
const RUN: u32 = 3;

/// A store, the shipped build flow, and one live run of it.
struct Rig {
    dir: TempDir,
    store: PumpStore,
}

fn rig() -> Rig {
    let dir = tempdir().expect("tempdir");
    let store = PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open");
    store.insert_run(&run()).expect("insert run");
    Rig { dir, store }
}

fn run() -> RunRow {
    RunRow {
        need: NEED.into(),
        run: RUN,
        flow: "build".into(),
        flow_version: 2,
        kind: "build".into(),
        entry: "research".into(),
        state: "live".into(),
    }
}

/// A step of the rig's run.
fn step(node: &str, task: &str, state: StepState, verdict: Option<&str>) -> StepRow {
    StepRow {
        run: RUN,
        node: node.into(),
        task: task.into(),
        state,
        verdict: verdict.map(str::to_owned),
    }
}

/// Store `steps` and return the rig.
fn rigged(steps: &[StepRow]) -> Rig {
    let mut rig = rig();
    for row in steps {
        rig.store.insert_step(NEED, row).expect("insert step");
    }
    rig
}

fn origin_of_rig(rig: &Rig, gate: &StepRow) -> Origin {
    let def = load(FlowKind::Build).expect("build flow");
    origin_of(&rig.store, &def, NEED, gate).expect("origin lookup")
}

/// The rig's park gate, held open with no verdict.
fn park_gate(task: &str) -> StepRow {
    step("parked", task, StepState::Waiting, None)
}

/// A recorded pause on `origin`'s own coordinates.
fn record_pause(rig: &Rig, origin: &str, node: &str) {
    let boundary = PauseBoundaryRow {
        need: NEED.into(),
        run: RUN,
        node: node.into(),
        origin_task: origin.into(),
        gate: "answer-gate".into(),
        pause_generation: 1,
        input_generation: 7,
        role_request: format!("{origin}-request"),
        boundary_message: None,
        paused_at: 1_700,
        state: pause_state::OPEN.into(),
    };
    rig.store.record_pause(&boundary).expect("record pause");
}

/// A jobs map with `jobs` in it.
struct Jobs(BTreeMap<String, PoolJob>);

impl JobStore for Jobs {
    fn jobs(&self) -> &BTreeMap<String, PoolJob> {
        &self.0
    }

    fn jobs_mut(&mut self) -> &mut BTreeMap<String, PoolJob> {
        &mut self.0
    }
}

/// A jobs map holding one job.
fn jobs_of(job: PoolJob) -> Jobs {
    Jobs(BTreeMap::from([(job.id.clone(), job)]))
}

/// The original read-only admitter request of `task`.
fn read_only_request(task: &str) -> PoolJob {
    let mut spec = JobSpec::new(step_request(task), "admitter", "admitter", "admit", "brief");
    spec.read_only = true;
    spec.from_ref = Some("main".into());
    PoolJob::from(spec)
}

/// A minimal flow with one plan-routed park and three excluded node classes.
fn tiny_def() -> FlowDef {
    let node = |id: &str, class: NodeClass| FlowNode {
        id: id.into(),
        class,
        label: id.into(),
    };
    FlowDef {
        kind: FlowKind::Build,
        nodes: vec![
            node("plan", NodeClass::Planner),
            node("pq", NodeClass::Decision),
            node("parked", NodeClass::Park),
            node("admit1", NodeClass::Admitter),
            node("mile1", NodeClass::Milestone),
            node("lock1", NodeClass::Lock),
        ],
        edges: vec![
            FlowEdge {
                from: "plan".into(),
                to: "pq".into(),
                words: vec![],
            },
            FlowEdge {
                from: "pq".into(),
                to: "parked".into(),
                words: ["operator", "hold"].iter().map(|w| w.to_string()).collect(),
            },
        ],
        entries: vec!["plan".into()],
    }
}

fn held_of(rig: &Rig, def: &FlowDef, jobs: &Jobs, step: &StepRow) -> Option<String> {
    held_request(&rig.store, def, jobs, step).expect("held lookup")
}

#[test]
fn a_recorded_origin_wins_over_an_ambiguous_topology() {
    let rig = rigged(&[
        step("research", "t_r", StepState::Done, Some("READY")),
        step("plan", "t_p1", StepState::Done, Some("operator")),
        step("plan2", "t_p2", StepState::Done, Some("operator")),
        park_gate("t_pk"),
    ]);
    // Both planners routed the park; only plan2's pause is recorded, so the
    // record decides and the topology is never guessed at.
    record_pause(&rig, "t_p2", "plan2");
    let origin = origin_of_rig(&rig, &park_gate("t_pk"));
    assert_eq!(
        origin,
        Origin::Found(OriginStep {
            step: step("plan2", "t_p2", StepState::Done, Some("operator")),
            source: OriginSource::Recorded,
        }),
        "the recorded plan2 origin wins over the ambiguous routing"
    );
}

#[test]
fn a_recorded_nonplanner_origin_is_trusted_verbatim() {
    // A held admitter is its own origin: its own recorded pause names it.
    let rig = rigged(&[step(
        "admit1",
        "t_a",
        StepState::Waiting,
        Some(OPERATOR_HOLD),
    )]);
    let gate = step("admit1", "t_a", StepState::Waiting, Some(OPERATOR_HOLD));
    record_pause(&rig, "t_a", "admit1");
    let origin = origin_of_rig(&rig, &gate);
    assert_eq!(
        origin,
        Origin::Found(OriginStep {
            step: step("admit1", "t_a", StepState::Waiting, Some(OPERATOR_HOLD)),
            source: OriginSource::Recorded,
        }),
        "the gate's own record names the nonplanner origin"
    );
}

#[test]
fn a_legacy_park_resolves_the_one_routed_planner() {
    let rig = rigged(&[
        step("research", "t_r", StepState::Done, Some("READY")),
        step("plan", "t_p1", StepState::Done, Some("operator")),
        park_gate("t_pk"),
    ]);
    let origin = origin_of_rig(&rig, &park_gate("t_pk"));
    assert_eq!(
        origin,
        Origin::Found(OriginStep {
            step: step("plan", "t_p1", StepState::Done, Some("operator")),
            source: OriginSource::Inferred,
        }),
        "one completed step routed the gate on its own answer word"
    );
}

#[test]
fn ambiguous_and_missing_legacy_origins_stay_parked() {
    let ambiguous = rigged(&[
        step("plan", "t_p1", StepState::Done, Some("operator")),
        step("plan2", "t_p2", StepState::Done, Some("operator")),
        park_gate("t_pk"),
    ]);
    let diagnostic = match origin_of_rig(&ambiguous, &park_gate("t_pk")) {
        Origin::Parked(text) => text,
        other => panic!("expected a refusal, got {other:?}"),
    };
    assert!(
        diagnostic.contains("t_p1") && diagnostic.contains("t_p2"),
        "the diagnostic names both candidates: {diagnostic}"
    );

    let unrouted = rigged(&[
        step("plan", "t_p1", StepState::Done, Some("planned")),
        park_gate("t_pk"),
    ]);
    assert!(
        matches!(
            origin_of_rig(&unrouted, &park_gate("t_pk")),
            Origin::Parked(_)
        ),
        "a planner that never answered the park word is no origin"
    );
}

#[test]
fn a_held_admitter_and_auditor_are_servable() {
    let rig = rig();
    let def = load(FlowKind::Build).expect("build flow");
    let admitter = step("admit1", "t_a", StepState::Waiting, Some(OPERATOR_HOLD));
    rig.store.insert_step(NEED, &admitter).expect("insert");
    let auditor = step("audit1", "t_d", StepState::Waiting, Some(OPERATOR_HOLD));
    rig.store.insert_step(NEED, &auditor).expect("insert");
    let admission = read_only_request("t_a");
    let audit = read_only_request("t_d");
    let jobs = Jobs(BTreeMap::from([
        (admission.id.clone(), admission),
        (audit.id.clone(), audit),
    ]));
    assert_eq!(
        held_of(&rig, &def, &jobs, &admitter).as_deref(),
        Some("step-t_a-a1"),
        "the original admitter request, never a correction"
    );
    assert_eq!(
        held_of(&rig, &def, &jobs, &auditor).as_deref(),
        Some("step-t_d-a1")
    );
}

#[test]
fn the_original_request_survives_a_correction_chain() {
    let rig = rig();
    let def = load(FlowKind::Build).expect("build flow");
    let held = step("admit1", "t_a", StepState::Waiting, Some(OPERATOR_HOLD));
    rig.store.insert_step(NEED, &held).expect("insert");
    let original = read_only_request("t_a");
    let mut correction = original.clone();
    correction.id = format!("{}-c1", step_request("t_a"));
    let mut retried = original.clone();
    retried.id = format!("{}-tr1", step_request("t_a"));
    let jobs = Jobs(BTreeMap::from([
        (original.id.clone(), original),
        (correction.id.clone(), correction.clone()),
        (retried.id.clone(), retried),
    ]));
    assert_eq!(
        held_of(&rig, &def, &jobs, &held).as_deref(),
        Some("step-t_a-a1"),
        "the request a continuation serves is the original dispatch"
    );
    // A step whose only job is a correction has no original request to
    // continue.
    let correction_id = correction.id.clone();
    let orphan = Jobs(BTreeMap::from([(correction_id, correction)]));
    assert_eq!(
        held_of(&rig, &def, &orphan, &held),
        None,
        "no original request, nothing to continue"
    );
    assert_eq!(step_request("t_a"), "step-t_a-a1");
}

#[test]
fn every_excluded_wait_is_unservable() {
    let rig = rig();
    let def = tiny_def();

    // A command job has no conversation to continue.
    let mut command = read_only_request("t_cmd");
    command.command = Some("true".into());
    let jobs = jobs_of(command);
    let held = step("admit1", "t_cmd", StepState::Waiting, Some(OPERATOR_HOLD));
    assert_eq!(held_of(&rig, &def, &jobs, &held), None, "command job");

    // An effectful request must never be re-sent by a continuation.
    let mut effectful = read_only_request("t_eff");
    effectful.read_only = false;
    effectful.publish_ref = Some("refs/heads/need".into());
    let jobs = jobs_of(effectful);
    let held = step("admit1", "t_eff", StepState::Waiting, Some(OPERATOR_HOLD));
    assert_eq!(held_of(&rig, &def, &jobs, &held), None, "effectful job");

    // Neither is a write request with no publication named.
    let mut write = read_only_request("t_write");
    write.read_only = false;
    let jobs = jobs_of(write);
    let held = step("admit1", "t_write", StepState::Waiting, Some(OPERATOR_HOLD));
    assert_eq!(held_of(&rig, &def, &jobs, &held), None, "not read-only");

    // A scope wait, a milestone hold and a lock are pump bookkeeping.
    rig.store
        .insert_slice_scope("t_scope", RUN, &["src/a.rs".into()])
        .expect("slice scope");
    rig.store
        .set_scope_state("t_scope", ScopeState::Wait)
        .expect("scope state");
    let jobs = Jobs(BTreeMap::new());
    let scoped = step("admit1", "t_scope", StepState::Waiting, Some(OPERATOR_HOLD));
    assert_eq!(held_of(&rig, &def, &jobs, &scoped), None, "scope wait");
    for (node, task) in [("mile1", "t_mile"), ("lock1", "t_lock")] {
        let held = step(node, task, StepState::Waiting, Some(OPERATOR_HOLD));
        assert_eq!(held_of(&rig, &def, &jobs, &held), None, "{node}");
    }

    // An uncertain side effect waits for evidence, not for an answer.
    append_exception(
        rig.dir.path(),
        7,
        SIDE_EFFECT_UNCLEAR,
        "t_unclear",
        "git could not settle it",
    );
    let held = step(
        "admit1",
        "t_unclear",
        StepState::Waiting,
        Some(OPERATOR_HOLD),
    );
    assert_eq!(held_of(&rig, &def, &jobs, &held), None, "unclear effect");

    // No job at all, and the park node itself, are not servable either.
    let jobless = step("admit1", "t_none", StepState::Waiting, Some(OPERATOR_HOLD));
    assert_eq!(held_of(&rig, &def, &jobs, &jobless), None, "no job");
    let parked = step("parked", "t_pk", StepState::Waiting, Some(OPERATOR_HOLD));
    assert_eq!(held_of(&rig, &def, &jobs, &parked), None, "park node");
}
