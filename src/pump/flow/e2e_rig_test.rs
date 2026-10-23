//! End to end: the shared rig for the end to end flow tests.
//!
//! A [`Rig`] is a real mailspace project, a pump context under a caller
//! config, the shipped step handlers, and an in-memory job table the test
//! drives as the pool would: it ticks the pump, finds the job a step opened,
//! finishes it with a scripted receipt, and ticks again. The tests of the
//! research, release and repair flows build on it.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::error::VivariumError;
use crate::pump::config::PumpConfig;
use crate::pump::fixture::{Project, project, send_need};
use crate::pump::flow::advance::start::start;
use crate::pump::flow::handlers::{Registry, shipped};
use crate::pump::flow::index::{RunRow, StepRow};
use crate::pump::flow::mail::{MailReader, MailWriter, MailspacePort, NeedStatus};
use crate::pump::flow::tick::tick_with;
use crate::pump::host::Ctx;
use crate::pump::jobs::{JobOutcome, JobStatus, JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, PumpStore};
use crate::routing::RoutedRole;

type R = Result<(), VivariumError>;

/// The pump's mailspace identity: the default `[pump] reply_from`.
const PUMP: &str = "mind";
/// Ticks `tick_until` may take before it gives up.
const MAX_TICKS: usize = 12;
/// The pool the rig's routed roles run on (the fixture's own pool).
const POOL: &str = "test-planner-pool";

/// The jobs the pump enqueued, in memory.
#[derive(Default)]
pub(crate) struct Jobs {
    pub jobs: BTreeMap<String, PoolJob>,
}

impl JobStore for Jobs {
    fn jobs(&self) -> &BTreeMap<String, PoolJob> {
        &self.jobs
    }

    fn jobs_mut(&mut self) -> &mut BTreeMap<String, PoolJob> {
        &mut self.jobs
    }
}

/// One need with an open pump record and a started run, over a real
/// mailspace, driven by the real tick.
pub(crate) struct Rig {
    pub project: Project,
    pub ctx: Ctx,
    pub store: PumpStore,
    pub port: MailspacePort,
    pub jobs: Jobs,
    pub registry: Registry<Jobs, MailspacePort>,
    pub need: String,
}

fn routed(name: &str, kind: &str) -> RoutedRole {
    RoutedRole {
        name: name.into(),
        kind: Some(kind.into()),
        status: String::from("active"),
        pool: Some(POOL.into()),
        read_only: None,
        model: None,
        models: Vec::new(),
    }
}

/// A rig under `cfg`: a need sent to the pump identity, its pump record
/// opened with `record_body` (for example `kind: repair\ntarget:
/// default\ntested: <sha>`), and a run started at `entry`. The routing holds
/// a `researcher` and a `repairer` role besides the fixture's planner.
///
/// # Errors
/// Returns a [`VivariumError`] when the record or the run cannot be opened.
pub(crate) fn rig(cfg: PumpConfig, record_body: &str, entry: &str) -> Result<Rig, VivariumError> {
    let project = project();
    let need = project.need("a need for the end to end rig");
    let mut ctx = project.pump_ctx();
    ctx.cfg = Arc::new(cfg);
    assert_eq!(ctx.cfg.reply_from, PUMP, "the rig speaks as the pump");
    let mut routing = (*ctx.routing).clone();
    routing.roles.push(routed("researcher", "researcher"));
    routing.roles.push(routed("repairer", "repairer"));
    ctx = ctx.with_routing(routing);
    let store = PumpStore::open(&ctx.home.join(DB_FILENAME))?;
    let mut port = MailspacePort {
        mailspace: project.mailspace.clone(),
        pump_identity: PUMP.into(),
    };
    MailWriter::open_record(&mut port, &need, record_body)?;
    start(&ctx, &store, &mut port, &need, Some(entry))?;
    let registry = shipped::<Jobs, MailspacePort>(&ctx);
    Ok(Rig {
        project,
        ctx,
        store,
        port,
        jobs: Jobs::default(),
        registry,
        need,
    })
}

impl Rig {
    /// One tick of the pump.
    ///
    /// # Errors
    /// Returns the tick's error.
    pub(crate) fn tick(&mut self) -> R {
        tick_with(
            &self.ctx,
            &mut self.jobs,
            &self.store,
            &mut self.port,
            &self.registry,
        )
    }

    /// Tick until `pred` holds, at most 12 ticks; the check runs before the
    /// first tick too.
    ///
    /// # Errors
    /// Returns a tick error, or an error naming `label` when `pred` never
    /// held.
    pub(crate) fn tick_until(&mut self, label: &str, pred: impl Fn(&Rig) -> bool) -> R {
        for _ in 0..MAX_TICKS {
            if pred(self) {
                return Ok(());
            }
            self.tick()?;
        }
        if pred(self) {
            return Ok(());
        }
        Err(VivariumError::Message(format!(
            "never reached `{label}` in {MAX_TICKS} ticks: runs {:?}, steps {:?}, exceptions {:?}",
            self.runs()?,
            self.store.steps_of(&self.need, self.latest_run()?),
            self.exceptions()
        )))
    }

    fn latest_run(&self) -> Result<u32, VivariumError> {
        Ok(self.runs()?.iter().map(|run| run.run).max().unwrap_or(1))
    }

    /// The need's runs, oldest first.
    ///
    /// # Errors
    /// Returns a store error.
    pub(crate) fn runs(&self) -> Result<Vec<RunRow>, VivariumError> {
        self.store.runs_of(&self.need)
    }

    /// The newest row of `node` in the newest run.
    ///
    /// # Errors
    /// Returns a store error.
    pub(crate) fn row(&self, node: &str) -> Result<Option<StepRow>, VivariumError> {
        Ok(self
            .store
            .steps_of(&self.need, self.latest_run()?)?
            .into_iter()
            .rev()
            .find(|row| row.node == node))
    }

    /// The newest pending or dispatched job whose payload `node` is `node`.
    #[must_use]
    pub(crate) fn open_job(&self, node: &str) -> Option<PoolJob> {
        self.jobs
            .jobs
            .values()
            .filter(|job| matches!(job.status, JobStatus::Pending | JobStatus::Dispatched))
            .filter(|job| job.payload.get("node").and_then(|n| n.as_str()) == Some(node))
            .max_by_key(|job| job.queued_at)
            .cloned()
    }

    /// End job `id` as the pool would: `status` and an outcome carrying
    /// `receipt`.
    ///
    /// # Errors
    /// Returns an error when no job has that id.
    pub(crate) fn finish(&mut self, id: &str, status: JobStatus, receipt: &str) -> R {
        let known: Vec<String> = self.jobs.jobs.keys().cloned().collect();
        let job = self
            .jobs
            .jobs
            .get_mut(id)
            .ok_or_else(|| VivariumError::Message(format!("no job {id} among {known:?}")))?;
        job.status = status;
        job.outcome = Some(JobOutcome {
            state: match status {
                JobStatus::Timeout => "timeout".into(),
                JobStatus::Lost => "lost".into(),
                _ => "finished".into(),
            },
            detail: String::new(),
            receipt: receipt.into(),
            at: 0,
        });
        Ok(())
    }

    /// Open a second need on this rig: a new need with its own pump record
    /// (`record_body`) and a run started at `entry`. Returns its handle. The
    /// rig's own `need`, `row` and `runs` keep pointing at the first need.
    ///
    /// # Errors
    /// Returns a [`VivariumError`] when the record or the run cannot be
    /// opened.
    pub(crate) fn add_need(
        &mut self,
        subject: &str,
        record_body: &str,
        entry: &str,
    ) -> Result<String, VivariumError> {
        let need = send_need(&self.port.mailspace, subject, PUMP);
        MailWriter::open_record(&mut self.port, &need, record_body)?;
        start(&self.ctx, &self.store, &mut self.port, &need, Some(entry))?;
        Ok(need)
    }

    /// The pump's `exceptions.txt`; empty when it was never written.
    #[must_use]
    pub(crate) fn exceptions(&self) -> String {
        match std::fs::read_to_string(self.ctx.home.join("exceptions.txt")) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => format!("exceptions.txt unreadable: {e}"),
        }
    }

    /// Script the `ls-remote origin <reference>` reply: `sha` at `reference`.
    pub(crate) fn git(&self, reference: &str, sha: &str) {
        self.project.host.reply(
            &format!("ls-remote origin {reference}"),
            0,
            &format!("{sha}\t{reference}\n"),
        );
    }
}

/// A pool result for an agent job that ended with `exit`, its last message
/// `message` (the envelope, as the worker wrote it).
#[must_use]
pub(crate) fn agent_receipt(exit: i32, message: &str) -> String {
    push_receipt(exit, "", "", message)
}

/// A pool result whose summary carries a `push:` line and a `runner publish:`
/// line (either may be empty, and then it is left out or blank).
#[must_use]
pub(crate) fn push_receipt(
    exit: i32,
    push_line: &str,
    runner_publish_line: &str,
    message: &str,
) -> String {
    let publish = if runner_publish_line.is_empty() {
        String::new()
    } else {
        format!("{runner_publish_line}\n")
    };
    format!(
        "pool result j1: finished\npool p  slot s1  model m\nfrom main  default\n\
         push: {push_line}\nagent exit {exit} in 12s, job 20s, started from 1234567890\n\
         {publish}\n--- receipt\n{message}\n\n--- stderr tail\n\n\n--- runner log tail\nlog line"
    )
}

/// A pool result of a job whose agent exited 127 because `tool` is missing.
#[must_use]
pub(crate) fn env_receipt(tool: &str) -> String {
    format!(
        "pool result j1: finished\npool p  slot s1  model m\nfrom main  default\n\
         push: \nagent exit 127 in 1s, job 2s, started from 1234567890\n\n\
         --- receipt\n\n\n--- stderr tail\nbash: {tool}: command not found\n\n\
         --- runner log tail\nlog line"
    )
}

#[test]
fn a_ready_research_envelope_lands_the_run_and_closes_the_need() -> R {
    let mut rig = rig(PumpConfig::default(), "kind: research", "research")?;
    rig.tick_until("research job", |rig| rig.open_job("research").is_some())?;
    let job = rig
        .open_job("research")
        .ok_or_else(|| VivariumError::Message("no research job".into()))?;
    let envelope = "+++\nanswer = \"READY\"\n+++\n\n## Verdict\nPlan it.";
    rig.finish(&job.id, JobStatus::Finished, &agent_receipt(0, envelope))?;
    rig.tick_until(
        "run done",
        |rig| matches!(rig.runs(), Ok(runs) if runs.iter().any(|run| run.state == "done")),
    )?;
    assert_eq!(rig.port.need_state(&rig.need)?, NeedStatus::Done);
    // The landing tick re-reads the kind after the record closed and raises
    // `flow-kind-changed` once (a tick defect, not the rig's); nothing else.
    let exceptions = rig.exceptions();
    assert!(
        exceptions
            .lines()
            .all(|line| line.contains("flow-kind-changed")),
        "{exceptions}"
    );
    Ok(())
}
