//! Test harness: a private Temporal dev server per test (the SDK's
//! `WorkflowEnvironment::start_local` with the installed `temporal` CLI),
//! one worker, a stand-in world and an in-memory record.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use temporalio_client::{Client, WorkflowGetResultOptions, WorkflowStartOptions};
use temporalio_sdk::testing::{
    DevServerLogLevel, EphemeralExe, LocalServer, LocalWorkflowEnvironmentOptions,
    WorkflowEnvironment,
};
use temporalio_sdk::{Runtime, Worker};
use vivi_temporal::activities::Stand;
use vivi_temporal::fault::NeedScript;
use vivi_temporal::lander::LanderWorkflow;
use vivi_temporal::need::NeedWorkflow;
use vivi_temporal::record::MemoryRecord;
use vivi_temporal::types::{AdmissionPolicy, LanderInput, NeedInput, NeedOutcome, Routing, Tuning};
use vivi_temporal::world::{Event, World, WorldTiming};
use vivi_temporal::{need_start_options, worker_options};

pub const QUEUE: &str = "spike";
pub const LANDER: &str = "lander";
pub const TARGET: &str = "stand";

pub fn tuning() -> Tuning {
    Tuning {
        max_work_rounds: 3,
        max_infra_attempts: 3,
        max_provider_waits: 5,
        provider_default_wait_ms: 200,
        implement_cap_ms: 2_500,
        heartbeat_ms: 1_000,
        step_cap_ms: 5_000,
        infra_retry_ms: 100,
    }
}

pub fn timing() -> WorldTiming {
    WorldTiming {
        step: Duration::from_millis(50),
        steps_per_round: 3,
        retry_after: Duration::from_millis(300),
        dark_hold: Duration::from_millis(4_000),
    }
}

pub struct Harness {
    env: WorkflowEnvironment<LocalServer>,
    pub client: Client,
    pub world: Arc<World>,
    pub record: Arc<MemoryRecord>,
}

impl Harness {
    pub async fn start(needs: Vec<(&str, NeedScript)>) -> Harness {
        let options = LocalWorkflowEnvironmentOptions::builder()
            .server_executable(EphemeralExe::ExistingPath(temporal_cli()))
            .startup_timeout(Duration::from_secs(30))
            .log_level(DevServerLogLevel::Error)
            .build();
        let env = WorkflowEnvironment::start_local(options)
            .await
            .expect("start temporal dev server");
        let client = env.client().clone();
        Harness {
            env,
            client,
            world: Arc::new(World::new(timing(), needs)),
            record: Arc::new(MemoryRecord::default()),
        }
    }

    /// Run `body` while a worker serves the queue; drop the worker after.
    pub async fn with_worker<F: Future>(&self, body: F) -> F::Output {
        let runtime = Runtime::from_current_tokio(Default::default()).expect("runtime");
        let stand = Stand {
            world: self.world.clone(),
            record: self.record.clone(),
        };
        let options = worker_options(QUEUE, stand).expect("worker options");
        let mut worker = Worker::new(&runtime, self.client.clone(), options).expect("worker");
        tokio::select! {
            run = worker.run() => panic!("worker stopped early: {run:?}"),
            out = body => out,
        }
    }

    pub async fn start_lander(&self, batch_window_ms: u64) {
        self.start_lander_with(batch_window_ms, AdmissionPolicy::default())
            .await;
    }

    pub async fn start_lander_with(&self, batch_window_ms: u64, admission: AdmissionPolicy) {
        let input = LanderInput {
            batch_window_ms,
            max_batch: 8,
            step_cap_ms: 5_000,
            gate_cap_ms: 5_000,
            target: TARGET.to_string(),
            routing: Routing::single(QUEUE),
            admission,
        };
        let opts = WorkflowStartOptions::new(QUEUE, LANDER).build();
        self.client
            .start_workflow(LanderWorkflow::run, input, opts)
            .await
            .expect("start lander");
    }

    /// Start a need and wait for its outcome.
    pub async fn run_need(&self, handle: &str, tuning: Tuning) -> NeedOutcome {
        let input = need_input(handle, tuning);
        let handle = self
            .client
            .start_workflow(NeedWorkflow::run, input, need_start_options(QUEUE, handle))
            .await
            .expect("start need");
        handle
            .get_result(WorkflowGetResultOptions::default())
            .await
            .expect("need result")
    }

    pub fn events(&self) -> Vec<Event> {
        self.world.events().expect("events")
    }

    pub async fn shutdown(self) {
        self.env.shutdown().await.expect("shut down dev server");
    }
}

pub fn need_input(handle: &str, tuning: Tuning) -> NeedInput {
    NeedInput {
        handle: handle.to_string(),
        lander_id: LANDER.to_string(),
        tuning,
        routing: Routing::single(QUEUE),
        target: TARGET.to_string(),
        brief: format!("stand-in need {handle}"),
        slices: Vec::new(),
        branch_prefix: "vivi/".to_string(),
    }
}

/// `TEMPORAL_CLI` when set, else the first `temporal` on `PATH`.
fn temporal_cli() -> String {
    if let Ok(path) = std::env::var("TEMPORAL_CLI") {
        return path;
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|dir| dir.join("temporal"))
        .find(|exe| exe.is_file())
        .map(|exe| exe.display().to_string())
        .expect("the temporal CLI on PATH, or TEMPORAL_CLI")
}
