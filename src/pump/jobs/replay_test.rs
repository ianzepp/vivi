//! Tests for the shared replay copy and the internal replay payload
//! metadata: copies keep their generation, replacements rise past the
//! superseded execution, and historical jobs without the keys read clean.

use std::collections::BTreeMap;

use serde_json::json;
use tempfile::{TempDir, tempdir};

use super::{
    copy_spec, dispatch_not_before, input_generation, replace_input, replacement_order,
    replacement_predecessor, stamp_dispatch_not_before, stamp_replacement,
};
use crate::error::VivariumError;
use crate::letter::fixture::record_body;
use crate::pump::config::PumpConfig;
use crate::pump::flow::advance::start::start;
use crate::pump::flow::collect_step::RunCx;
use crate::pump::flow::def::{FlowDef, FlowKind};
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::handlers::Registry;
use crate::pump::flow::index::{StepJobRow, StepRow};
use crate::pump::flow::load::load;
use crate::pump::flow::mail::MailWriter;
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::jobs::{self, JobSpec, JobStatus, JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, PumpStore};

type R = Result<(), VivariumError>;

const NEED: &str = "n1";

#[derive(Default)]
struct Jobs {
    jobs: BTreeMap<String, PoolJob>,
}

impl JobStore for Jobs {
    fn jobs(&self) -> &BTreeMap<String, PoolJob> {
        &self.jobs
    }

    fn jobs_mut(&mut self) -> &mut BTreeMap<String, PoolJob> {
        &mut self.jobs
    }
}

struct Rig {
    _dir: TempDir,
    ctx: Ctx,
    store: PumpStore,
    mail: FakeMail,
    jobs: Jobs,
    registry: Registry<Jobs, FakeMail>,
    def: FlowDef,
}

fn rig() -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let mut mail = FakeMail::new("mind");
    MailWriter::open_record(&mut mail, NEED, &record_body(NEED, "build"))?;
    start(&ctx, &store, &mut mail, NEED, Some("plan"))?;
    Ok(Rig {
        _dir: dir,
        ctx,
        store,
        mail,
        jobs: Jobs::default(),
        registry: Registry::new(),
        def: load(FlowKind::Build)?,
    })
}

impl Rig {
    fn plan_step(&self) -> Result<StepRow, VivariumError> {
        self.store
            .steps_of(NEED, 1)?
            .into_iter()
            .find(|row| row.node == "plan")
            .ok_or_else(|| VivariumError::Message("no plan step".into()))
    }
}

/// A pending job carrying exactly `payload`.
fn job_with_payload(id: &str, payload: serde_json::Value) -> PoolJob {
    let mut spec = JobSpec::new(id, "planner", "pump-planner", "subject", "body");
    spec.payload = payload;
    PoolJob::from(spec)
}

#[test]
fn copy_keeps_its_generation_until_new_input_replaces_it() {
    let job = job_with_payload("j1", json!({"input_generation": 4}));
    let mut spec = copy_spec(&job, "j1-a2");
    // The copy preserves the generation of the input it was built from.
    assert_eq!(input_generation(&spec.payload), 4);
    // New input replaces it only by an explicit stamp.
    replace_input(&mut spec.payload, 7);
    assert_eq!(input_generation(&spec.payload), 7);
    // The rest of the copy is untouched and the source still reads its own.
    assert_eq!(spec.kind, "planner");
    assert_eq!(input_generation(&job.payload), 4);
}

#[test]
fn replacement_order_rises_past_the_superseded_execution() {
    // The latest correction of j1 carries replacement order 3.
    let correction = job_with_payload("j1-c2", json!({"replacement_order": 3}));
    // The operator re-copies the earlier substantive request j1, whose own
    // order is older and lower: the stamp must rise past the superseded
    // execution, not past the copied one.
    let mut copy = job_with_payload(
        "j1-q1",
        json!({"replacement_order": 1, "input_generation": 2}),
    )
    .payload;
    stamp_replacement(&mut copy, &correction);
    assert_eq!(replacement_order(&copy), 4);
    assert_eq!(replacement_predecessor(&copy), Some("j1-c2"));
    // The copied generation survives the replacement stamp.
    assert_eq!(input_generation(&copy), 2);
    // Superseding a historical job with no metadata starts the order at 1.
    let historical = job_with_payload("j0", json!({}));
    let mut first = json!({});
    stamp_replacement(&mut first, &historical);
    assert_eq!(replacement_order(&first), 1);
    assert_eq!(replacement_predecessor(&first), Some("j0"));
}

#[test]
fn missing_or_mistyped_metadata_reads_as_default() {
    for payload in [
        json!({}),
        json!({"input_generation": "x"}),
        json!({"replacement_order": null, "replaces": 3}),
        json!([]),
    ] {
        assert_eq!(input_generation(&payload), 0);
        assert_eq!(replacement_order(&payload), 0);
        assert_eq!(replacement_predecessor(&payload), None);
        assert_eq!(dispatch_not_before(&payload), None);
    }
    // A stamp on a payload that is not an object starts fresh metadata.
    let mut payload = json!("text");
    stamp_dispatch_not_before(&mut payload, 500);
    assert_eq!(dispatch_not_before(&payload), Some(500));
    assert_eq!(replacement_order(&payload), 0);
}

#[test]
fn retry_preserves_complete_job_spec() -> R {
    let mut rig = rig()?;
    let step = rig.plan_step()?;
    let mut spec = JobSpec::new(
        "plan-j1",
        "planner",
        "pump-planner",
        "plan the slice",
        "body",
    );
    spec.task = Some(step.task.clone());
    spec.target = Some("default".into());
    spec.from_ref = Some("refs/heads/from".into());
    spec.with = vec!["refs/heads/extra".into()];
    spec.extra_targets = vec!["side-repo".into()];
    spec.pool_candidates = vec!["alpha".into()];
    spec.pool_exclude = vec!["refused".into()];
    spec.local_only = true;
    spec.read_only = true;
    spec.branch = Some("vivi/repair/x".into());
    spec.allow_default_branch = true;
    spec.publish_ref = Some("refs/heads/vivi/repair/x".into());
    spec.publish_expect = Some("expect-sha".into());
    spec.publish_contains = Some("contain-sha".into());
    spec.delete_ref = Some("refs/heads/delete".into());
    spec.delete_expect = Some("delete-sha".into());
    spec.model = Some("model-q4".into());
    spec.command = Some("harness.sh".into());
    spec.builtin_role = Some("tugboat".into());
    spec.units = vec!["unit-1".into()];
    spec.source_jobs = vec!["src-1".into()];
    spec.budget = 600;
    spec.payload = json!({
        "input_generation": 3,
        "replacement_order": 5,
        "plan_file": "plan/one.md",
    });
    jobs::enqueue(&mut rig.jobs, spec)?;
    let job = rig.jobs.jobs["plan-j1"].clone();
    let row = StepJobRow {
        job: "plan-j1".into(),
        run: 1,
        node: "plan".into(),
        task: step.task.clone(),
        attempt: 1,
    };
    rig.store.insert_step_job(NEED, &row)?;
    let run = rig.store.runs_of(NEED)?.remove(0);
    let decide = |_: &_, _: &_| Err(VivariumError::Message("no decision".into()));
    let mut cx = RunCx {
        ctx: &rig.ctx,
        st: &mut rig.jobs,
        store: &rig.store,
        mail: &mut rig.mail,
        registry: &rig.registry,
        decide: &decide,
        def: rig.def.clone(),
        run,
    };
    // Repair-style fresh refs: the intentional edits must win over the copy.
    cx.rerun_with(&job, &row, &step, |spec| {
        spec.branch = Some("vivi/repair/x-t2".into());
        spec.publish_ref = Some("refs/heads/vivi/repair/x-t2".into());
    })?;
    let queued = &rig.jobs.jobs["plan-j1-a2"];
    // The intentional edits are retained.
    assert_eq!(queued.branch.as_deref(), Some("vivi/repair/x-t2"));
    assert_eq!(
        queued.publish_ref.as_deref(),
        Some("refs/heads/vivi/repair/x-t2")
    );
    // The copy queues fresh, and the ordinary work-attempt rules hold: the
    // same task and run continue at the next attempt.
    assert_eq!(queued.status, JobStatus::Pending);
    let stored = rig.store.step_job("plan-j1-a2")?.unwrap();
    assert_eq!(stored.attempt, row.attempt + 1);
    assert_eq!(queued.payload["attempt"], json!(stored.attempt));
    assert_eq!(queued.payload["task"], json!(stored.task));
    assert_eq!(queued.payload["run"], json!(stored.run));
    // Every other execution input survives: identity and checkout first.
    assert_eq!(queued.kind, "planner");
    assert_eq!(queued.role, "pump-planner");
    assert_eq!(queued.subject, "plan the slice");
    assert_eq!(queued.body, "body");
    assert_eq!(queued.task.as_deref(), Some(step.task.as_str()));
    assert_eq!(queued.target.as_deref(), Some("default"));
    assert_eq!(queued.from_ref.as_deref(), Some("refs/heads/from"));
    assert_eq!(queued.with, vec!["refs/heads/extra".to_owned()]);
    // Dispatch restrictions: extra targets and the pool bounds.
    assert_eq!(queued.extra_targets, vec!["side-repo".to_owned()]);
    assert_eq!(queued.pool_candidates, vec!["alpha".to_owned()]);
    assert_eq!(queued.pool_exclude, vec!["refused".to_owned()]);
    assert!(queued.local_only);
    assert!(queued.read_only);
    // Model, harness command, built-in charter and budget.
    assert_eq!(queued.model.as_deref(), Some("model-q4"));
    assert_eq!(queued.command.as_deref(), Some("harness.sh"));
    assert_eq!(queued.builtin_role.as_deref(), Some("tugboat"));
    assert_eq!(queued.budget, 600);
    // Publication ancestry and deletion expectations, untouched by the
    // fresh-ref edits.
    assert_eq!(queued.publish_expect.as_deref(), Some("expect-sha"));
    assert_eq!(queued.publish_contains.as_deref(), Some("contain-sha"));
    assert_eq!(queued.delete_ref.as_deref(), Some("refs/heads/delete"));
    assert_eq!(queued.delete_expect.as_deref(), Some("delete-sha"));
    // The default-branch permission survives too.
    assert!(queued.allow_default_branch);
    assert_eq!(queued.units, vec!["unit-1".to_owned()]);
    assert_eq!(queued.source_jobs, vec!["src-1".to_owned()]);
    // Replay metadata: the copy keeps its generation, rides past the
    // superseded execution, and keeps unrelated payload keys.
    assert_eq!(queued.payload["input_generation"], json!(3));
    assert_eq!(queued.payload["replacement_order"], json!(6));
    assert_eq!(queued.payload["replaces"], json!("plan-j1"));
    assert_eq!(queued.payload["plan_file"], json!("plan/one.md"));
    Ok(())
}
