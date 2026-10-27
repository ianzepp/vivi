//! Companion tests: intercepting planner and admitter scope requests before
//! escalation — allowed original and corrected replies, refused requests,
//! mixed, unknown and mismatched targets, ordinary escalation, and a failed
//! publication that leaves the source unapplied for retry.

use std::collections::BTreeMap;
use std::fs;

use tempfile::{TempDir, tempdir};

use super::super::{RoleCollector, RunCx, StepCollector};
use super::FLOW_SCOPE_OPERATOR;
use crate::error::VivariumError;
use crate::letter::fixture::need_body_with;
use crate::letter::work::NeedFront;
use crate::mailspace::pump_note::note_of;
use crate::pump::config::{PumpConfig, PumpTarget};
use crate::pump::flow::advance::route::JoinInput;
use crate::pump::flow::def::{FlowDef, FlowKind, FlowNode};
use crate::pump::flow::fake_mail::{Call, FakeMail};
use crate::pump::flow::handlers::{Registry, shipped};
use crate::pump::flow::index::{PayloadIdentity, RunRow, StepJobRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::need_scope::{
    RulingDecision, ScopeIssue, ScopeRuling, decode, effective_target_scope,
};
use crate::pump::flow::tick::OPERATOR_HOLD;
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::jobs::{self, JobOutcome, JobSpec, JobStatus, JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, PumpStore};

type R = Result<(), VivariumError>;

const NEED: &str = "n1";
/// The configured target the requests name.
const TARGET: &str = "vivi";
/// A second configured target, for the mismatch case.
const OTHER: &str = "default";
/// The task handle of the asking plan step.
const TASK: &str = "task-plan";
/// The task handle of the asking admit step.
const ADMIT_TASK: &str = "task-admit";
/// The path a grant adds.
const ASKED: &str = "src/lib.rs";
/// The denied path.
const DENIED: &str = "src/secret/";
/// The need's delegated path.
const DELEGATED: &str = "src/pump/";
/// The canned `need show` body the fake host serves.
const THREAD: &str = "need thread: build the widget";

/// The pool job map of a test.
struct Jobs(BTreeMap<String, PoolJob>);

impl JobStore for Jobs {
    fn jobs(&self) -> &BTreeMap<String, PoolJob> {
        &self.0
    }

    fn jobs_mut(&mut self) -> &mut BTreeMap<String, PoolJob> {
        &mut self.0
    }
}

struct Rig {
    _dir: TempDir,
    _checkout: TempDir,
    ctx: Ctx,
    store: PumpStore,
    mail: FakeMail,
    jobs: Jobs,
    registry: Registry<Jobs, FakeMail>,
    def: FlowDef,
    run: RunRow,
}

/// A rig with a live build run, a running `plan` step and an `admit1` step,
/// a need whose letter delegates `src/pump/` (optionally naming a target),
/// and two configured targets, the first with a real checkout.
fn rig(need_target: Option<&str>) -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let checkout = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    fs::create_dir_all(checkout.path().join("src/secret"))
        .map_err(|e| VivariumError::Other(e.to_string()))?;
    fs::write(checkout.path().join("src/lib.rs"), "fn main() {}\n")
        .map_err(|e| VivariumError::Other(e.to_string()))?;
    let targets = BTreeMap::from([
        (
            TARGET.to_string(),
            PumpTarget {
                scope_allow: Some(vec![String::from("src/")]),
                scope_deny: vec![String::from("src/secret/")],
                integration_repo: Some(checkout.path().to_string_lossy().into_owned()),
                ..PumpTarget::default()
            },
        ),
        (OTHER.to_string(), PumpTarget::default()),
    ]);
    let (ctx, host) = test_ctx(
        dir.path(),
        PumpConfig {
            targets,
            ..PumpConfig::default()
        },
    );
    host.reply("need show", 0, THREAD);
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let mut mail = FakeMail::new("mind");
    let front = NeedFront {
        write_scope: vec![DELEGATED.to_string()],
        target: need_target.map(String::from),
        ..NeedFront::new(vec![String::from("done")])
    };
    let sent = mail.send_step("mind", "hand", "the need", &need_body_with(&front, ""), &[])?;
    if let Some(task) = mail.tasks.remove(&sent) {
        mail.tasks.insert(NEED.to_string(), task);
    }
    // The asking steps have their own open tasks: a routed advance closes
    // the step task in the mail.
    for (node, task) in [("plan", TASK), ("admit1", ADMIT_TASK)] {
        let sent = mail.send_step("mind", "mind", node, "step", &[])?;
        if let Some(mut task_mail) = mail.tasks.remove(&sent) {
            task_mail.subject = node.to_string();
            mail.tasks.insert(task.to_string(), task_mail);
        }
    }
    let run = RunRow {
        need: NEED.into(),
        run: 1,
        flow: "build".into(),
        flow_version: 2,
        kind: "build".into(),
        entry: "plan".into(),
        state: "live".into(),
    };
    store.insert_run(&run)?;
    store.insert_step(NEED, &step_row("plan", TASK))?;
    store.insert_step(NEED, &step_row("admit1", ADMIT_TASK))?;
    let registry = shipped::<Jobs, FakeMail>(&ctx);
    Ok(Rig {
        _dir: dir,
        _checkout: checkout,
        ctx,
        store,
        mail,
        jobs: Jobs(BTreeMap::new()),
        registry,
        def: load(FlowKind::Build)?,
        run,
    })
}

fn step_row(node: &str, task: &str) -> StepRow {
    StepRow {
        run: 1,
        node: node.into(),
        task: task.into(),
        state: StepState::Running,
        verdict: None,
    }
}

/// The pool receipt carrying `message` as the hand's reply.
fn receipt(message: &str) -> String {
    format!(
        "pool result j1: finished\npool p  slot s1  model m\nfrom main  default\n\
         push: none\nagent exit 0 in 12s, job 20s, started from 1234567890\n\n\
         --- receipt\n{message}\n\n--- stderr tail\n\n\n--- runner log tail\nlog line"
    )
}

/// A planner `operator` reply asking for `paths` at `target`.
fn planner_ask(paths: &[&str], target: &str) -> String {
    ask("operator", paths, target)
}

/// An admitter `escalate` reply asking for `paths` at `target`.
fn admitter_ask(paths: &[&str], target: &str) -> String {
    ask("escalate", paths, target)
}

fn ask(answer: &str, paths: &[&str], target: &str) -> String {
    let list = paths
        .iter()
        .map(|path| format!("\"{path}\""))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "+++\nanswer = \"{answer}\"\nreason = \"the work needs the file\"\n\
         scope_target = \"{target}\"\nscope_paths = [{list}]\n+++\nAsking."
    )
}

/// An admitter `escalate` reply with no structured request.
fn plain_escalate() -> String {
    "+++\nanswer = \"escalate\"\nreason = \"the plan disagrees\"\n+++\nEscalating.".to_string()
}

impl Rig {
    /// Queue `id` of `role` for `task` and mark it finished with `message`.
    fn finishes(&mut self, id: &str, role: &str, task: &str, message: &str) -> R {
        let mut spec = JobSpec::new(id, role, role, "s", "b");
        spec.task = Some(task.to_owned());
        jobs::enqueue(&mut self.jobs, spec)?;
        let job = self
            .jobs
            .0
            .get_mut(id)
            .ok_or_else(|| VivariumError::Message("no job".into()))?;
        job.status = JobStatus::Finished;
        job.outcome = Some(JobOutcome {
            state: "finished".into(),
            detail: String::new(),
            receipt: receipt(message),
            at: 0,
        });
        Ok(())
    }

    /// Collect `id` as attempt 1 of `node` through the real [`RoleCollector`].
    fn collect(&mut self, id: &str, node: &str, task: &str) -> R {
        let class = self
            .def
            .nodes
            .iter()
            .find(|n: &&FlowNode| n.id == node)
            .map(|n| n.class.clone())
            .ok_or_else(|| VivariumError::Message(format!("no {node} node")))?;
        if self.store.step_job(id)?.is_none() {
            self.store.insert_step_job(
                NEED,
                &StepJobRow {
                    job: id.into(),
                    run: 1,
                    node: node.into(),
                    task: task.into(),
                    attempt: 1,
                },
            )?;
        }
        let step = self
            .store
            .steps_of(NEED, 1)?
            .into_iter()
            .find(|s| s.node == node)
            .ok_or_else(|| VivariumError::Message("no step".into()))?;
        let job = self.jobs.0[id].clone();
        let mut cx = RunCx {
            ctx: &self.ctx,
            st: &mut self.jobs,
            store: &self.store,
            mail: &mut self.mail,
            registry: &self.registry,
            decide: &refuse_decision,
            def: self.def.clone(),
            run: self.run.clone(),
        };
        RoleCollector.collect(&mut cx, &job, &row_of(id, node, task), &step, &class)
    }

    fn step(&self, node: &str) -> Result<StepRow, VivariumError> {
        self.store
            .steps_of(NEED, 1)?
            .into_iter()
            .find(|s| s.node == node)
            .ok_or_else(|| VivariumError::Message(format!("no {node} step")))
    }

    fn exceptions(&self) -> String {
        fs::read_to_string(self.ctx.home.join("exceptions.txt")).unwrap_or_default()
    }

    /// The bodies posted to the need audience.
    fn posted(&self) -> Vec<String> {
        self.mail
            .calls
            .iter()
            .filter_map(|call| match call {
                Call::PostToNeed { body, .. } => Some(body.clone()),
                _ => None,
            })
            .collect()
    }

    /// The scope ruling of the only posted note.
    fn posted_ruling(&self) -> Result<ScopeRuling, VivariumError> {
        let posted = self.posted();
        let body = posted
            .first()
            .ok_or_else(|| VivariumError::Message("nothing posted".into()))?;
        let note = note_of(body).ok_or_else(|| VivariumError::Message("posted note".into()))?;
        decode(note.prose.trim())
    }

    /// How many continuation jobs the interception queued.
    fn continuation_count(&self) -> usize {
        self.jobs
            .0
            .keys()
            .filter(|id| id.ends_with("-cont"))
            .count()
    }
}

fn row_of(id: &str, node: &str, task: &str) -> StepJobRow {
    StepJobRow {
        job: id.into(),
        run: 1,
        node: node.into(),
        task: task.into(),
        attempt: 1,
    }
}

/// The decision callback of these tests: it refuses, so a stray routing
/// decision fails the test instead of passing silently.
fn refuse_decision(_: &FlowNode, _: &[JoinInput]) -> Result<String, VivariumError> {
    Err(VivariumError::Message("no decision".into()))
}

#[test]
fn an_allowed_planner_request_continues_the_role() -> R {
    let mut rig = rig(None)?;
    rig.finishes("plan-j1", "planner", TASK, &planner_ask(&[ASKED], TARGET))?;
    rig.collect("plan-j1", "plan", TASK)?;

    // The ruling is published as permission evidence.
    assert_eq!(rig.posted().len(), 1);
    let ruling = rig.posted_ruling()?;
    assert_eq!(ruling.decision, RulingDecision::Grant);
    assert_eq!(ruling.paths, vec![ASKED.to_string()]);
    let row = rig.store.scope_request("plan-j1")?.expect("journal");
    assert_eq!(row.decision, RulingDecision::Grant);
    assert!(row.published_at.is_some());

    // The role continues at the same attempt of the same task and run.
    let job = rig.jobs.0.get("plan-j1-cont").ok_or_else(|| {
        VivariumError::Message(format!(
            "no continuation job; jobs: {:?}",
            rig.jobs.0.keys()
        ))
    })?;
    assert_eq!(job.kind, "planner");
    let identity = PayloadIdentity::of(&job.payload).map_err(VivariumError::Parse)?;
    assert_eq!(identity.need, NEED);
    assert_eq!(identity.run, 1);
    assert_eq!(identity.task, TASK);
    assert_eq!(identity.attempt, Some(1));
    assert_eq!(rig.store.step_job("plan-j1-cont")?.expect("row").attempt, 1);
    assert_eq!(
        rig.store.step_job("plan-j1")?.expect("source row").attempt,
        1
    );
    assert_eq!(rig.continuation_count(), 1);

    // The step never advanced: no parked or cto route, the step is running.
    let plan = rig.step("plan")?;
    assert_eq!(plan.state, StepState::Running);
    assert_eq!(plan.verdict, None);
    assert!(
        rig.store
            .steps_of(NEED, 1)?
            .iter()
            .all(|s| s.node != "parked")
    );
    assert!(rig.store.steps_of(NEED, 1)?.iter().all(|s| s.node != "cto"));
    // No attempt, correction or exception.
    assert!(rig.store.step_job("plan-j1-a2")?.is_none());
    assert!(!rig.jobs.0.contains_key("plan-j1-c1"));
    assert!(rig.exceptions().is_empty(), "{}", rig.exceptions());
    Ok(())
}

#[test]
fn an_allowed_corrected_reply_keys_to_the_base() -> R {
    let mut rig = rig(None)?;
    // The broken original, then its envelope fix carrying the valid request.
    rig.finishes("plan-j1", "planner", TASK, "no envelope here")?;
    rig.finishes(
        "plan-j1-c1",
        "planner",
        TASK,
        &planner_ask(&[ASKED], TARGET),
    )?;
    rig.store
        .insert_step_job(NEED, &row_of("plan-j1", "plan", TASK))?;
    rig.collect("plan-j1-c1", "plan", TASK)?;

    // The journal and continuation key to the correction-stripped base.
    let row = rig.store.scope_request("plan-j1")?.expect("journal");
    assert_eq!(row.decision, RulingDecision::Grant);
    assert!(row.published_at.is_some());
    let job = rig.jobs.0.get("plan-j1-cont").expect("continuation job");
    let identity = PayloadIdentity::of(&job.payload).map_err(VivariumError::Parse)?;
    assert_eq!(identity.attempt, Some(1));
    assert_eq!(rig.store.step_job("plan-j1-cont")?.expect("row").attempt, 1);
    assert_eq!(rig.jobs.0.len(), 3, "original, correction, continuation");
    let plan = rig.step("plan")?;
    assert_eq!(plan.state, StepState::Running);
    Ok(())
}

#[test]
fn a_denied_planner_request_parks_under_an_operator_ruling() -> R {
    let mut rig = rig(None)?;
    rig.finishes("plan-j1", "planner", TASK, &planner_ask(&[DENIED], TARGET))?;
    rig.collect("plan-j1", "plan", TASK)?;

    // The operator-required ruling is published and grants nothing.
    assert_eq!(rig.posted().len(), 1);
    let ruling = rig.posted_ruling()?;
    assert_eq!(ruling.decision, RulingDecision::OperatorRequired);
    assert_eq!(ruling.issue, Some(ScopeIssue::Denied));
    let row = rig.store.scope_request("plan-j1")?.expect("journal");
    assert_eq!(row.decision, RulingDecision::OperatorRequired);
    assert_eq!(row.issue, Some(ScopeIssue::Denied));
    assert!(row.published_at.is_some());
    assert_eq!(
        effective_target_scope(&rig.mail, NEED, TARGET, "mind")?,
        vec![DELEGATED.to_string()]
    );
    // The planner keeps its ordinary operator route: parked, never the cto.
    assert!(rig.step("parked").is_ok());
    assert!(rig.store.steps_of(NEED, 1)?.iter().all(|s| s.node != "cto"));
    // No continuation and no policy exception for a planner.
    assert_eq!(rig.continuation_count(), 0);
    assert!(
        !rig.exceptions().contains(FLOW_SCOPE_OPERATOR),
        "{}",
        rig.exceptions()
    );
    Ok(())
}

#[test]
fn a_denied_admitter_request_holds_for_the_operator() -> R {
    let mut rig = rig(None)?;
    rig.finishes(
        "adm-j1",
        "admitter",
        ADMIT_TASK,
        &admitter_ask(&[DENIED], TARGET),
    )?;
    rig.collect("adm-j1", "admit1", ADMIT_TASK)?;

    // The step waits under the exact exception code, never the cto route.
    let admit = rig.step("admit1")?;
    assert_eq!(admit.state, StepState::Waiting);
    assert_eq!(admit.verdict.as_deref(), Some(OPERATOR_HOLD));
    let log = rig.exceptions();
    assert_eq!(log.matches(FLOW_SCOPE_OPERATOR).count(), 1, "{log}");
    assert!(rig.store.steps_of(NEED, 1)?.iter().all(|s| s.node != "cto"));
    // The ruling is published, grants nothing, and no role continues.
    assert_eq!(rig.posted().len(), 1);
    let row = rig.store.scope_request("adm-j1")?.expect("journal");
    assert_eq!(row.decision, RulingDecision::OperatorRequired);
    assert_eq!(row.issue, Some(ScopeIssue::Denied));
    assert!(row.published_at.is_some());
    assert_eq!(rig.continuation_count(), 0);
    assert_eq!(
        effective_target_scope(&rig.mail, NEED, TARGET, "mind")?,
        vec![DELEGATED.to_string()]
    );
    Ok(())
}

#[test]
fn a_mixed_request_grants_no_path() -> R {
    let mut rig = rig(None)?;
    rig.finishes(
        "plan-j1",
        "planner",
        TASK,
        &planner_ask(&[ASKED, DENIED], TARGET),
    )?;
    rig.collect("plan-j1", "plan", TASK)?;

    // One forbidden member refuses the whole set: the operator-required
    // ruling names both paths and grants neither.
    let row = rig.store.scope_request("plan-j1")?.expect("journal");
    assert_eq!(row.decision, RulingDecision::OperatorRequired);
    assert_eq!(row.issue, Some(ScopeIssue::Denied));
    let ruling = rig.posted_ruling()?;
    assert_eq!(ruling.decision, RulingDecision::OperatorRequired);
    assert!(ruling.paths.contains(&ASKED.to_string()));
    assert!(ruling.paths.contains(&DENIED.to_string()));
    assert_eq!(
        effective_target_scope(&rig.mail, NEED, TARGET, "mind")?,
        vec![DELEGATED.to_string()]
    );
    assert_eq!(rig.continuation_count(), 0);
    assert!(rig.step("parked").is_ok());
    Ok(())
}

#[test]
fn an_unknown_or_mismatched_target_needs_the_operator() -> R {
    for (target, issue, need_target) in [
        ("ghost", ScopeIssue::UnknownTarget, None),
        (OTHER, ScopeIssue::TargetMismatch, Some(TARGET)),
    ] {
        let mut rig = rig(need_target)?;
        rig.finishes("plan-j1", "planner", TASK, &planner_ask(&[ASKED], target))?;
        rig.collect("plan-j1", "plan", TASK)?;

        let row = rig.store.scope_request("plan-j1")?.expect("journal");
        assert_eq!(row.decision, RulingDecision::OperatorRequired, "{issue:?}");
        assert_eq!(row.issue, Some(issue), "{target}");
        assert_eq!(row.target, target);
        assert_eq!(rig.posted().len(), 1, "{target}");
        assert_eq!(
            effective_target_scope(&rig.mail, NEED, TARGET, "mind")?,
            vec![DELEGATED.to_string()],
            "{target}"
        );
        assert_eq!(rig.continuation_count(), 0, "{target}");
        assert!(rig.step("parked").is_ok(), "{target}");
    }
    Ok(())
}

#[test]
fn an_ordinary_escalation_still_routes_to_the_cto() -> R {
    let mut rig = rig(None)?;
    rig.finishes("adm-j1", "admitter", ADMIT_TASK, &plain_escalate())?;
    rig.collect("adm-j1", "admit1", ADMIT_TASK)?;

    // No structured request: the ordinary route decides, no ruling is
    // journalled or posted, and nothing waits on the operator.
    let cto = rig
        .store
        .steps_of(NEED, 1)?
        .into_iter()
        .find(|s| s.node == "cto");
    assert!(cto.is_some(), "no cto step");
    assert!(rig.posted().is_empty());
    assert!(rig.store.scope_request("adm-j1")?.is_none());
    assert_eq!(rig.continuation_count(), 0);
    assert!(rig.exceptions().is_empty(), "{}", rig.exceptions());
    Ok(())
}

#[test]
fn scope_ruling_failure_does_not_dispatch() -> R {
    let mut rig = rig(None)?;
    rig.finishes("plan-j1", "planner", TASK, &planner_ask(&[ASKED], TARGET))?;
    // The ruling post fails: the collection errors, the tick holds the job.
    rig.mail.fail_at = Some(rig.mail.calls.len());
    assert!(rig.collect("plan-j1", "plan", TASK).is_err());

    // No continuation, no scope mutation, and the source is unapplied.
    assert_eq!(rig.continuation_count(), 0);
    assert_eq!(rig.jobs.0.len(), 1, "only the source job exists");
    assert!(rig.posted().is_empty());
    assert_eq!(
        effective_target_scope(&rig.mail, NEED, TARGET, "mind")?,
        vec![DELEGATED.to_string()]
    );
    let row = rig.store.scope_request("plan-j1")?.expect("prepared");
    assert!(row.published_at.is_none());
    assert_eq!(rig.jobs.0["plan-j1"].applied_at, None);
    let plan = rig.step("plan")?;
    assert_eq!(plan.state, StepState::Running);

    // The retry publishes once and continues the role.
    rig.mail.fail_at = None;
    rig.collect("plan-j1", "plan", TASK)?;
    assert_eq!(rig.posted().len(), 1);
    let row = rig.store.scope_request("plan-j1")?.expect("journal");
    assert!(row.published_at.is_some());
    assert_eq!(rig.jobs.0.get("plan-j1-cont").map(|_| 1), Some(1));
    Ok(())
}
