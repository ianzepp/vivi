//! The deployable worker: one process per host, one Temporal worker per
//! queue that host serves (`[hosts.<name>].queues`), each limited to the
//! configured number of concurrent activities. The control queue also runs
//! the workflows. Plus the two starters: the Lander and a need.

use std::sync::Arc;
use std::time::Duration;

use temporalio_client::{
    Client, ClientOptions, Connection, ConnectionOptions, WorkflowGetResultOptions,
    WorkflowIdReusePolicy, WorkflowStartOptions,
};
use temporalio_sdk::runtime::worker_tuner::{FixedSizeSlotSupplier, TunerHolder};
use temporalio_sdk::{Runtime, Worker, WorkerOptions};
use thiserror::Error;

use crate::config::{Config, RecordKind, lander_id};
use crate::lander::LanderWorkflow;
use crate::need::NeedWorkflow;
use crate::real::Real;
use crate::record::{LogRecord, McpRecord, MemoryRecord, NeedRecord};
use crate::slice::SliceWorkflow;
use crate::types::{NeedInput, NeedOutcome, SliceSpec};
use crate::{error::ConfigError, need_start_options};

/// Workflow tasks are short; this bounds them on the control queue.
const WORKFLOW_SLOTS: usize = 20;

#[derive(Debug, Error)]
pub enum WorkerError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("temporal: {0}")]
    Temporal(String),
}

fn temporal(e: impl std::fmt::Display) -> WorkerError {
    WorkerError::Temporal(e.to_string())
}

pub async fn connect(config: &Config, identity: &str) -> Result<Client, WorkerError> {
    let url: temporalio_client::Url = config.server.address.parse().map_err(temporal)?;
    let conn = ConnectionOptions::new(url)
        .identity(identity.to_string())
        .connect_timeout(Duration::from_secs(15))
        .build();
    let connection = Connection::connect(conn).await.map_err(temporal)?;
    let options = ClientOptions::new(config.server.namespace.clone()).build();
    Client::new(connection, options).map_err(temporal)
}

pub fn record_for(config: &Config) -> Arc<dyn NeedRecord> {
    match config.record.kind {
        RecordKind::Memory => Arc::new(MemoryRecord::default()),
        RecordKind::Log => Arc::new(LogRecord),
        RecordKind::Mcp => Arc::new(McpRecord {
            config: config.record.clone(),
        }),
    }
}

fn options_for(
    config: &Arc<Config>,
    host: &str,
    queue: &str,
    slots: usize,
    record: &Arc<dyn NeedRecord>,
) -> Result<WorkerOptions, WorkerError> {
    let tuner = TunerHolder::builder()
        .workflow_task_slot_supplier(FixedSizeSlotSupplier::new(WORKFLOW_SLOTS))
        .activity_task_slot_supplier(FixedSizeSlotSupplier::new(slots))
        .local_activity_task_slot_supplier(FixedSizeSlotSupplier::new(1))
        .nexus_task_slot_supplier(FixedSizeSlotSupplier::new(1))
        .build();
    let real = Real {
        host: host.to_string(),
        config: config.clone(),
        record: record.clone(),
    };
    let builder = WorkerOptions::new(queue)
        .tuner(tuner)
        .client_identity_override(format!("{host}/{queue}"));
    if queue != config.routing.control {
        return Ok(builder.register_activities(real).build());
    }
    Ok(builder
        .register_workflow::<NeedWorkflow>()
        .map_err(temporal)?
        .register_workflow::<SliceWorkflow>()
        .map_err(temporal)?
        .register_workflow::<LanderWorkflow>()
        .map_err(temporal)?
        .register_activities(real)
        .build())
}

/// Poll every queue `host` serves until one worker stops or Ctrl-C.
pub async fn run_host(config: Config, host: &str) -> Result<(), WorkerError> {
    let config = Arc::new(config);
    let queues = config.queues_for(host)?;
    let client = connect(&config, host).await?;
    let runtime = Runtime::from_current_tokio(Default::default()).map_err(temporal)?;
    let record = record_for(&config);
    let mut workers = Vec::new();
    for (queue, slots) in &queues {
        let options = options_for(&config, host, queue, *slots, &record)?;
        workers.push(Worker::new(&runtime, client.clone(), options).map_err(temporal)?);
        println!("{host}: polling {queue} with {slots} activity slots");
    }
    let stops: Vec<_> = workers.iter().map(Worker::shutdown_handle).collect();
    let runs = workers.iter_mut().map(|w| Box::pin(w.run()));
    let all = futures::future::select_all(runs);
    tokio::select! {
        (ended, i, _) = all => {
            for stop in &stops { stop(); }
            ended.map_err(|e| temporal(format!("worker {} stopped: {e}", queues[i].0)))
        }
        _ = tokio::signal::ctrl_c() => {
            println!("{host}: stopping");
            for stop in &stops { stop(); }
            Ok(())
        }
    }
}

/// Start the target's Lander unless it already runs.
pub async fn start_lander(config: &Config, target: &str) -> Result<String, WorkerError> {
    let client = connect(config, "starter").await?;
    let input = config.lander_input(target)?;
    let id = lander_id(target);
    let opts = WorkflowStartOptions::new(config.routing.control.clone(), id.clone())
        .id_reuse_policy(WorkflowIdReusePolicy::AllowDuplicate)
        .build();
    match client
        .start_workflow(LanderWorkflow::run, input, opts)
        .await
    {
        Ok(_) => Ok(format!("started {id}")),
        Err(temporalio_client::errors::WorkflowStartError::AlreadyStarted { .. }) => {
            Ok(format!("{id} already runs"))
        }
        Err(e) => Err(temporal(e)),
    }
}

/// What to start a need with.
pub struct NeedRequest {
    pub handle: String,
    pub target: String,
    pub brief: String,
    pub slices: Vec<SliceSpec>,
}

/// Start a need and wait for its outcome.
pub async fn run_need(config: &Config, request: NeedRequest) -> Result<NeedOutcome, WorkerError> {
    let target = config.target(&request.target)?;
    let client = connect(config, "starter").await?;
    let input = NeedInput {
        handle: request.handle.clone(),
        lander_id: lander_id(&request.target),
        tuning: config.tuning.clone(),
        routing: config.routing(),
        target: request.target.clone(),
        brief: request.brief,
        slices: request.slices,
        branch_prefix: target.branch_prefix.clone(),
    };
    let opts = need_start_options(&config.routing.control, &request.handle);
    let handle = client
        .start_workflow(NeedWorkflow::run, input, opts)
        .await
        .map_err(temporal)?;
    println!("started {}", crate::need_workflow_id(&request.handle));
    handle
        .get_result(WorkflowGetResultOptions::default())
        .await
        .map_err(temporal)
}
