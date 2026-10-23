//! End to end: one need through the shipped build flow's planning loop, on a
//! real mailspace, driven by the real tick.
//!
//! The flow is `plan` (planner) then `admit1` (admitter, asks for a revision),
//! then `plan2` (planner, answers `delivered`), then the `done` terminal. It
//! has no join and no raise check, so the tick's `decide` is never reached.
//! The tick resolves flows by kind from the shipped registry, so the stub is
//! not a flow of its own: it is the step handlers (which start every planner
//! and admitter step as a job) and the loop below (which finishes each job
//! with the scripted envelope, as a pool worker would).

use std::collections::BTreeMap;

use super::tick_with;
use crate::error::VivariumError;
use crate::mailspace::{Mailspace, SendRequest};
use crate::pump::fixture::{Project, project};
use crate::pump::flow::def::{FlowKind, NodeClass};
use crate::pump::flow::handlers::Registry;
use crate::pump::flow::index::{RunRow, StepRow};
use crate::pump::flow::mail::read::run_count;
use crate::pump::flow::mail::{MailReader, MailWriter, MailspacePort, NeedStatus, TaskStatus};
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler, role_job};
use crate::pump::flow::run::{SENTINEL_NODE, start};
use crate::pump::host::Ctx;
use crate::pump::jobs::{JobOutcome, JobStatus, JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, PumpStore};

type R = Result<(), VivariumError>;

/// The pump's mailspace identity: the default `[pump] reply_from`, distinct
/// from the default `[pump] from` (`operator`).
const PUMP: &str = "mind";
/// Ticks the loop may take; the flow needs three job rounds.
const MAX_TICKS: usize = 12;

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

/// Starts every planner and admitter step as a pool job.
struct JobStep;

impl StepHandler for JobStep {
    fn start(&self, cx: &StartCx<'_>, step: &StepRow) -> Result<StartAction, VivariumError> {
        Ok(StartAction::Job(Box::new(role_job(
            cx.ctx,
            cx.run,
            step,
            "planner",
            "planner",
            &format!("stub brief for {}", step.node),
        ))))
    }
}

fn handlers() -> Registry<Jobs, MailspacePort> {
    let mut registry = Registry::new();
    registry.handler(FlowKind::Build, NodeClass::Planner, Box::new(JobStep));
    registry.handler(FlowKind::Build, NodeClass::Admitter, Box::new(JobStep));
    registry
}

/// The scripted reply of the worker that holds `node`'s step.
fn reply_for(node: &str) -> &'static str {
    match node {
        "plan" => "+++\nanswer = \"planned\"\n+++\nplan",
        "admit1" => {
            "+++\nanswer = \"revise\"\nrequired_changes = ['tighten the scope']\n+++\nrevise"
        }
        "plan2" => "+++\nanswer = \"delivered\"\nreason = 'nothing left to build'\n+++\nplan 2",
        other => panic!("the stub flow never starts node {other}"),
    }
}

fn receipt(message: &str) -> String {
    format!(
        "pool result j1: finished\npool p  slot s1  model m\nfrom main  default\n\
         push: \nagent exit 0 in 12s, job 20s, started from 1234567890\n\n\
         --- receipt\n{message}\n\n--- stderr tail\n\n\n--- runner log tail\nlog line"
    )
}

struct Rig {
    _project: Project,
    ctx: Ctx,
    store: PumpStore,
    port: MailspacePort,
    jobs: Jobs,
    need: String,
}

fn rig() -> Result<Rig, VivariumError> {
    let p = project();
    let need = p.need("plan something");
    rig_for(p, need)
}

/// A rig over `need`, run under the default config (`from` is not
/// `reply_from`).
fn rig_for(p: Project, need: String) -> Result<Rig, VivariumError> {
    let ctx = p.pump_ctx();
    assert_ne!(ctx.cfg.from, ctx.cfg.reply_from);
    assert_eq!(ctx.cfg.reply_from, PUMP);
    let store = PumpStore::open(&ctx.home.join(DB_FILENAME))?;
    let mut port = MailspacePort {
        mailspace: p.mailspace.clone(),
        pump_identity: PUMP.into(),
    };
    MailWriter::open_record(&mut port, &need, "kind: build")?;
    Ok(Rig {
        _project: p,
        ctx,
        store,
        port,
        jobs: Jobs::default(),
        need,
    })
}

impl Rig {
    fn tick(&mut self) -> R {
        tick_with(
            &self.ctx,
            &mut self.jobs,
            &self.store,
            &mut self.port,
            &handlers(),
        )
    }

    /// Finish every job still in flight with its node's scripted reply.
    fn finish_jobs(&mut self) -> Result<usize, VivariumError> {
        let open: Vec<String> = self
            .jobs
            .jobs
            .values()
            .filter(|job| !job.status.is_terminal())
            .map(|job| job.id.clone())
            .collect();
        for id in &open {
            let row = self
                .store
                .step_job(id)?
                .ok_or_else(|| VivariumError::Message(format!("job {id} has no step row")))?;
            let job = self
                .jobs
                .jobs
                .get_mut(id)
                .ok_or_else(|| VivariumError::Message(format!("job {id} vanished")))?;
            job.status = JobStatus::Finished;
            job.outcome = Some(JobOutcome {
                state: "finished".into(),
                detail: String::new(),
                receipt: receipt(reply_for(&row.node)),
                at: 0,
            });
        }
        Ok(open.len())
    }

    fn exceptions(&self) -> String {
        std::fs::read_to_string(self.ctx.home.join("exceptions.txt")).unwrap_or_default()
    }
}

/// The `From` of every `done` state-event letter held for `item`.
fn done_event_senders(mailspace: &Mailspace, item: &str) -> Vec<String> {
    let storage = mailspace.storage().unwrap();
    let message_id = storage.resolve_message_token(item).unwrap();
    let view = storage.message_by_id(&message_id).unwrap().unwrap();
    let mut senders = Vec::new();
    for child in storage.reply_child_contents(&view.content_id).unwrap() {
        let row = storage.message_by_content_id(&child).unwrap().unwrap();
        let bytes = storage.read_listed_blob(&row.blob_relpath).unwrap();
        let parsed = mail_parser::MessageParser::default().parse(&bytes).unwrap();
        let header = |name: &str| {
            parsed
                .headers()
                .iter()
                .find(|h| h.name().eq_ignore_ascii_case(name))
                .and_then(|h| match h.value() {
                    mail_parser::HeaderValue::Text(text) => Some(text.trim().to_string()),
                    _ => None,
                })
        };
        if header("X-Vivi-Event").as_deref() == Some("done") {
            senders.push(
                parsed
                    .from()
                    .and_then(|from| from.first())
                    .and_then(|addr| addr.address())
                    .and_then(|address| address.split('@').next())
                    .unwrap_or_default()
                    .to_string(),
            );
        }
    }
    senders
}

#[test]
fn one_need_runs_the_planning_loop_to_a_landed_close() -> R {
    landed_close(rig()?)
}

/// A need authored elsewhere and delegated to a third role: the pump's
/// landing close is authorized by its `reply_from` identity alone, writes a
/// `done` event letter that travels, and the need is done.
#[test]
fn a_delegated_need_lands_with_a_travelling_done_letter() -> R {
    let p = project();
    let need = send_need_from(&p, "elsewhere", "delegated");
    p.mailspace.assign_need("elsewhere", &need, "operator")?;
    landed_close(rig_for(p, need)?)
}

/// Send a need from `from` to the pump identity.
fn send_need_from(p: &Project, from: &str, subject: &str) -> String {
    p.mailspace
        .send(SendRequest {
            from: from.into(),
            to: vec![PUMP.into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.into(),
            body: "done_when: it works".into(),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle
}

fn landed_close(mut rig: Rig) -> R {
    let run: RunRow = start(&rig.ctx, &rig.store, &mut rig.port, &rig.need, Some("plan"))?;
    assert_eq!(run.kind, "build");
    assert_eq!(rig.port.need_state(&rig.need)?, NeedStatus::Open);

    let mut finished = 0;
    let mut landed = false;
    for _ in 0..MAX_TICKS {
        rig.tick()?;
        if !rig.store.runs_in_state("done")?.is_empty() {
            landed = true;
            break;
        }
        assert_eq!(
            rig.port.need_state(&rig.need)?,
            NeedStatus::Open,
            "the need closed before the terminal node"
        );
        finished += rig.finish_jobs()?;
    }
    assert!(
        landed,
        "the run never reached its terminal node: steps {:?}, exceptions {}, log {:?}",
        rig.store.steps_of(&rig.need, 1)?,
        rig.exceptions(),
        std::fs::read_to_string(rig.ctx.home.join("pump.log")).unwrap_or_default()
    );
    assert_eq!(finished, 3, "one job each for plan, admit1 and plan2");

    // The need is done, by a `done` event letter the pump identity wrote.
    assert_eq!(rig.port.need_state(&rig.need)?, NeedStatus::Done);
    assert_eq!(
        rig.port
            .mailspace
            .item_event_state(&rig.need)?
            .unwrap()
            .state,
        "done"
    );
    assert_eq!(done_event_senders(&rig.port.mailspace, &rig.need), [PUMP]);
    // The pump record is closed.
    assert!(MailReader::open_record(&rig.port, &rig.need)?.is_none());

    // Every step task, the sentinel included, is closed with its verdict.
    let rows = rig.store.steps_of(&rig.need, 1)?;
    let verdicts: BTreeMap<&str, &str> = [
        ("plan", "planned"),
        ("admit1", "revise"),
        ("plan2", "delivered"),
        (SENTINEL_NODE, "delivered"),
    ]
    .into_iter()
    .collect();
    assert_eq!(rows.len(), verdicts.len(), "{rows:?}");
    for row in &rows {
        let want = verdicts[row.node.as_str()];
        let state = rig.port.task_state(&row.task)?;
        assert_eq!(state.status, TaskStatus::Done, "{}", row.node);
        assert_eq!(state.verdict.as_deref(), Some(want), "{}", row.node);
        assert_eq!(state.closer.as_deref(), Some(PUMP), "{}", row.node);
        assert_eq!(row.verdict.as_deref(), Some(want), "{}", row.node);
    }

    // One run, no live or finishing leftovers, and no alarm raised.
    assert_eq!(run_count(&rig.port.mailspace, &rig.need), 1);
    assert!(rig.store.live_runs()?.is_empty());
    assert!(rig.store.finishing_runs()?.is_empty());
    assert_eq!(rig.exceptions(), "");
    Ok(())
}
