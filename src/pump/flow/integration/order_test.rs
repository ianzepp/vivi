//! W2-IN-1: four verified needs on one target reach main in rank order.
//!
//! The jobs are built by the real `MergeMainStart` handler and carry the
//! payload the tick writes; the order is the dispatcher's: repair kind first,
//! then need priority, then age, with one merge-main in flight per target.

use std::collections::BTreeMap;

use tempfile::tempdir;

use super::start::MergeMainStart;
use crate::error::VivariumError;
use crate::pump::config::PumpConfig;
use crate::pump::flow::def::FlowKind;
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::flow::rank::{flow_key, single_flight};
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::{self, JobStatus, JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, NeedBranchRow, PumpStore};
use crate::routing::RoutedRole;

type R = Result<(), VivariumError>;

const MAIN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

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

fn with_merge_route(ctx: Ctx) -> Ctx {
    let mut routing = (*ctx.routing).clone();
    routing.roles.push(RoutedRole {
        name: String::from("merge"),
        kind: Some(String::from("merge")),
        status: String::from("active"),
        pool: Some(String::from("test-planner-pool")),
        read_only: Some(false),
        model: None,
        models: Vec::new(),
    });
    ctx.with_routing(routing)
}

/// One verified need: (handle, run kind, priority, queued_at).
const NEEDS: [(&str, &str, i64, u64); 4] = [
    ("p5", "build", 5, 100),
    ("p10", "build", 10, 200),
    ("rep", "repair", 1, 300),
    ("p5b", "build", 5, 400),
];

fn tip_of(index: usize) -> String {
    format!("{:040x}", index + 1)
}

fn queue(
    ctx: &Ctx,
    host: &FakeHost,
    store: &PumpStore,
    jobs: &mut Jobs,
) -> Result<(), VivariumError> {
    let def = load(FlowKind::Build)?;
    let mail = FakeMail::new("mind");
    for (index, (need, kind, _, queued_at)) in NEEDS.iter().enumerate() {
        host.reply(
            &format!("ls-remote origin refs/heads/vivi/need/{need}"),
            0,
            &format!("{}\trefs/heads/vivi/need/{need}\n", tip_of(index)),
        );
        store.upsert_need_branch(&NeedBranchRow {
            need: (*need).into(),
            branch: format!("vivi/need/{need}"),
            base: Some(format!("{:040x}", index + 101)),
            status: "live".into(),
            run: 1,
            target: "default".into(),
            priors: Vec::new(),
        })?;
        let run = RunRow {
            need: (*need).into(),
            run: 1,
            flow: "build".into(),
            flow_version: 2,
            kind: (*kind).into(),
            entry: "plan".into(),
            state: "live".into(),
        };
        let step = StepRow {
            run: 1,
            node: "merge".into(),
            task: format!("t-{need}"),
            state: StepState::Pending,
            verdict: None,
        };
        let cx = StartCx {
            ctx,
            store,
            mail: &mail,
            run: &run,
            def: &def,
        };
        let StartAction::Job(boxed) = MergeMainStart.start(&cx, &step)? else {
            return Err(VivariumError::Message("not a job".into()));
        };
        let mut spec = *boxed;
        let mut payload = match std::mem::take(&mut spec.payload) {
            serde_json::Value::Object(map) => map,
            _ => serde_json::Map::new(),
        };
        for (key, value) in [
            ("need", serde_json::json!(need)),
            ("run", serde_json::json!(1)),
            ("node", serde_json::json!("merge")),
            ("task", serde_json::json!(step.task)),
            ("attempt", serde_json::json!(1)),
            ("kind", serde_json::json!(kind)),
            ("class", serde_json::json!("merge-main")),
        ] {
            payload.insert(key.to_owned(), value);
        }
        spec.payload = serde_json::Value::Object(payload);
        let id = spec.id.clone();
        jobs::enqueue(jobs, spec)?;
        jobs.jobs_mut()
            .get_mut(&id)
            .ok_or_else(|| VivariumError::Message(format!("no job {id}")))?
            .queued_at = *queued_at;
    }
    Ok(())
}

fn pending_sorted(jobs: &Jobs, priorities: &BTreeMap<String, i64>) -> Vec<PoolJob> {
    let mut pending: Vec<PoolJob> = jobs
        .jobs
        .values()
        .filter(|job| job.status == JobStatus::Pending)
        .cloned()
        .collect();
    pending.sort_by_key(|job| flow_key(job, priorities));
    pending
}

fn need_of(job: &PoolJob) -> String {
    job.payload["need"].as_str().unwrap_or_default().to_owned()
}

#[test]
fn four_verified_needs_merge_in_rank_order_one_at_a_time() -> R {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    let ctx = with_merge_route(ctx);
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    host.reply(
        "ls-remote origin main",
        0,
        &format!("{MAIN}\trefs/heads/main\n"),
    );
    let mut jobs = Jobs::default();
    queue(&ctx, &host, &store, &mut jobs)?;
    assert_eq!(jobs.jobs.len(), 4);
    let priorities: BTreeMap<String, i64> = NEEDS
        .iter()
        .map(|(need, _, priority, _)| ((*need).to_owned(), *priority))
        .collect();

    let mut order = Vec::new();
    while !pending_sorted(&jobs, &priorities).is_empty() {
        let pending = pending_sorted(&jobs, &priorities);
        let kept = single_flight(pending.clone(), &jobs.jobs);
        assert_eq!(kept.len(), 1, "one merge-main per target at a time");
        let first = kept[0].id.clone();
        assert_eq!(first, pending[0].id);
        order.push(need_of(&kept[0]));

        let job = jobs
            .jobs
            .get_mut(&first)
            .ok_or_else(|| VivariumError::Message(format!("no job {first}")))?;
        job.status = JobStatus::Dispatched;
        assert!(
            single_flight(pending_sorted(&jobs, &priorities), &jobs.jobs).is_empty(),
            "another merge-main of the target is Dispatched"
        );
        let job = jobs
            .jobs
            .get_mut(&first)
            .ok_or_else(|| VivariumError::Message(format!("no job {first}")))?;
        job.status = JobStatus::Finished;
    }
    assert_eq!(order, ["rep", "p10", "p5", "p5b"]);
    Ok(())
}
