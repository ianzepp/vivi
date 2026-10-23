//! Tests for the repairer collector: overtime, lost retries, the publish
//! receipt deciding `built`, and a failed envelope.

use std::collections::BTreeMap;

use tempfile::{TempDir, tempdir};

use super::register;
use crate::error::VivariumError;
use crate::pump::config::PumpConfig;
use crate::pump::flow::advance::start::start;
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::handlers::Registry;
use crate::pump::flow::index::{StepJobRow, StepRow, StepState};
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::repair::{self, AttemptFacts};
use crate::pump::flow::tick::tick_with;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::{self, JobOutcome, JobSpec, JobStatus, JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, PumpStore};

type R = Result<(), VivariumError>;

const NEED: &str = "n1";
const BASE: &str = "1111111111111111111111111111111111111111";
const CAND: &str = "2222222222222222222222222222222222222222";
const BRANCH: &str = "vivi/repair/n1-a1";
const BUILT: &str = "+++\nanswer = \"built\"\n+++\nfixed";
const FAILED: &str = "+++\nanswer = \"failed\"\n+++\ngave up";

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
    _host: std::sync::Arc<FakeHost>,
    store: PumpStore,
    mail: FakeMail,
    jobs: Jobs,
    registry: Registry<Jobs, FakeMail>,
}

/// A repair run at `fix1` with one repairer job of that step.
fn rig() -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let mut mail = FakeMail::new("mind");
    MailWriter::open_record(&mut mail, NEED, "kind: repair\ntarget: vivi")?;
    start(&ctx, &store, &mut mail, NEED, Some("fix1"))?;
    let mut registry = Registry::new();
    register(&mut registry);
    let mut rig = Rig {
        _dir: dir,
        ctx,
        _host: host,
        store,
        mail,
        jobs: Jobs::default(),
        registry,
    };
    rig.fix_job("fix1", "fix-j1")?;
    Ok(rig)
}

impl Rig {
    fn step(&self, node: &str) -> Result<StepRow, VivariumError> {
        self.store
            .steps_of(NEED, 1)?
            .into_iter()
            .rev()
            .find(|row| row.node == node)
            .ok_or_else(|| VivariumError::Message(format!("no step {node}")))
    }

    fn fix_job(&mut self, node: &str, id: &str) -> R {
        let step = self.step(node)?;
        let mut spec = JobSpec::new(id, "repairer", "repairer", "s", "b");
        spec.task = Some(step.task.clone());
        spec.from_ref = Some(BASE.into());
        spec.publish_ref = Some(format!("refs/heads/{BRANCH}"));
        jobs::enqueue(&mut self.jobs, spec)?;
        self.store.insert_step_job(
            NEED,
            &StepJobRow {
                job: id.into(),
                run: 1,
                node: node.into(),
                task: step.task.clone(),
                attempt: 1,
            },
        )?;
        self.store
            .set_step_state(&step.task, StepState::Running, None)
    }

    fn end(&mut self, status: JobStatus, receipt: Option<String>) {
        self.end_job("fix-j1", status, receipt);
    }

    fn end_job(&mut self, id: &str, status: JobStatus, receipt: Option<String>) {
        let job = self.jobs.jobs.get_mut(id).unwrap();
        job.status = status;
        job.outcome = receipt.map(|receipt| JobOutcome {
            state: "finished".into(),
            detail: String::new(),
            receipt,
            at: 0,
        });
    }

    fn tick(&mut self) -> R {
        tick_with(
            &self.ctx,
            &mut self.jobs,
            &self.store,
            &mut self.mail,
            &self.registry,
        )
    }
}

fn receipt(publish: &str, message: &str) -> String {
    format!(
        "pool result j1: finished\npool p  slot s1  model m\nfrom main  default\n\
         agent exit 0 in 12s, job 20s, started from 1234567890\n{publish}\n\n\
         --- receipt\n{message}\n\n--- stderr tail\n\n\n--- runner log tail\nlog line"
    )
}

fn pushed() -> String {
    format!("runner publish: pushed refs/heads/{BRANCH} {CAND} {CAND}")
}

#[test]
fn a_timeout_answers_overtime_with_no_retry() -> R {
    let mut rig = rig()?;
    rig.end(JobStatus::Timeout, None);
    rig.tick()?;
    let fix = rig.step("fix1")?;
    assert_eq!(fix.state, StepState::Done);
    assert_eq!(fix.verdict.as_deref(), Some("overtime"));
    assert_eq!(rig.step("fix2")?.state, StepState::Pending);
    assert_eq!(rig.jobs.jobs.len(), 1, "no rerun job");
    Ok(())
}

#[test]
fn a_lost_job_runs_a_second_attempt() -> R {
    let mut rig = rig()?;
    rig.end(JobStatus::Lost, None);
    rig.tick()?;
    assert_eq!(rig.step("fix1")?.state, StepState::Running);
    assert_eq!(rig.jobs.jobs.len(), 2);
    assert_eq!(rig.store.step_job("fix-j1-a2")?.unwrap().attempt, 2);
    Ok(())
}

#[test]
fn a_built_job_with_a_pushed_branch_posts_the_attempt_facts_and_routes_to_the_rung() -> R {
    let mut rig = rig()?;
    rig.end(JobStatus::Finished, Some(receipt(&pushed(), BUILT)));
    rig.tick()?;
    let fix = rig.step("fix1")?;
    assert_eq!(fix.state, StepState::Done);
    assert_eq!(fix.verdict.as_deref(), Some("built"));
    assert_eq!(rig.step("rung1")?.state, StepState::Pending);
    let replies = &rig.mail.tasks[&fix.task].replies;
    let facts = replies.iter().find_map(|reply| repair::parse_facts(reply));
    assert_eq!(
        facts,
        Some(AttemptFacts {
            branch: BRANCH.into(),
            commit: CAND.into(),
            base: BASE.into(),
        }),
        "{replies:?}"
    );
    Ok(())
}

#[test]
fn a_built_job_with_nothing_published_answers_failed() -> R {
    let mut rig = rig()?;
    rig.end(JobStatus::Finished, Some(receipt("", BUILT)));
    rig.tick()?;
    let fix = rig.step("fix1")?;
    assert_eq!(fix.verdict.as_deref(), Some("failed"));
    assert_eq!(rig.step("fix2")?.state, StepState::Pending);
    assert!(rig.mail.tasks[&fix.task].replies.is_empty());
    Ok(())
}

#[test]
fn a_publish_of_another_ref_answers_failed() -> R {
    let mut rig = rig()?;
    let other = format!("runner publish: pushed refs/heads/vivi/repair/other {CAND} {CAND}");
    rig.end(JobStatus::Finished, Some(receipt(&other, BUILT)));
    rig.tick()?;
    assert_eq!(rig.step("fix1")?.verdict.as_deref(), Some("failed"));
    Ok(())
}

#[test]
fn a_failed_envelope_routes_to_the_second_attempt() -> R {
    let mut rig = rig()?;
    rig.end(JobStatus::Finished, Some(receipt("", FAILED)));
    rig.tick()?;
    assert_eq!(rig.step("fix1")?.verdict.as_deref(), Some("failed"));
    assert_eq!(rig.step("fix2")?.state, StepState::Pending);
    Ok(())
}

fn no_changes() -> String {
    "push: no_changes".into()
}

#[test]
fn an_empty_attempt_answers_unchanged_and_routes_to_the_second_attempt() -> R {
    let mut rig = rig()?;
    rig.end(JobStatus::Finished, Some(receipt(&no_changes(), BUILT)));
    rig.tick()?;
    let fix = rig.step("fix1")?;
    assert_eq!(fix.state, StepState::Done);
    assert_eq!(fix.verdict.as_deref(), Some("unchanged"));
    assert_eq!(rig.step("fix2")?.state, StepState::Pending);
    let payload = rig.store.step_payload(&fix.task)?.unwrap().payload;
    let reason = payload["reason"].as_str().unwrap();
    assert!(reason.contains("no_changes"), "{reason}");
    assert!(reason.contains(BASE), "{reason}");
    assert!(reason.contains(BRANCH), "{reason}");
    assert_eq!(payload["job"], "fix-j1");
    Ok(())
}

#[test]
fn an_empty_second_attempt_routes_to_unrepaired() -> R {
    let mut rig = rig()?;
    rig.end(JobStatus::Finished, Some(receipt("", FAILED)));
    rig.tick()?;
    rig.fix_job("fix2", "fix-j2")?;
    rig.end_job(
        "fix-j2",
        JobStatus::Finished,
        Some(receipt(&no_changes(), BUILT)),
    );
    rig.tick()?;
    assert_eq!(rig.step("fix2")?.verdict.as_deref(), Some("unchanged"));
    assert_eq!(rig.step("unrepaired")?.node, "unrepaired");
    Ok(())
}

#[test]
fn a_refused_push_answers_failed_naming_it() -> R {
    let mut rig = rig()?;
    let refused =
        format!("push: push_refused\nrunner publish: failed refs/heads/{BRANCH} {CAND} {CAND}");
    rig.end(JobStatus::Finished, Some(receipt(&refused, BUILT)));
    rig.tick()?;
    let fix = rig.step("fix1")?;
    assert_eq!(fix.verdict.as_deref(), Some("failed"));
    let payload = rig.store.step_payload(&fix.task)?.unwrap().payload;
    let reason = payload["reason"].as_str().unwrap();
    assert!(reason.contains("push_refused"), "{reason}");
    assert!(reason.contains(BRANCH), "{reason}");
    Ok(())
}
