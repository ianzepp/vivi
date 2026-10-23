//! The research-post phase: a landed research run answers on its want once.

use std::collections::BTreeMap;

use tempfile::{TempDir, tempdir};

use super::register;
use crate::error::VivariumError;
use crate::pump::config::PumpConfig;
use crate::pump::flow::fake_mail::{Call, FakeMail};
use crate::pump::flow::handlers::Registry;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::mail::MailWriter;
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::jobs::{JobOutcome, JobSpec, JobStatus, JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, PumpStore};

type R = Result<(), VivariumError>;

const STEP_TASK: &str = "step-task";

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
    need: String,
    want: String,
}

/// A rig whose need body is `body` (`{want}` stands for the want's handle).
fn rig(body: &str) -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let mut mail = FakeMail::new("mind");
    let want = mail.send_step("mind", "mind", "the want", "wanted", &[])?;
    let need = mail.send_step(
        "mind",
        "mind",
        "the need",
        &body.replace("{want}", &want),
        &[],
    )?;
    let mut registry = Registry::new();
    register(&mut registry);
    Ok(Rig {
        _dir: dir,
        ctx,
        store,
        mail,
        jobs: Jobs::default(),
        registry,
        need,
        want,
    })
}

const ENVELOPE: &str = "+++\nanswer = \"READY\"\n+++\n\n## Verdict\nPlan it.";

impl Rig {
    /// A `done` run of `kind` with a research step and one finished job.
    fn land(&mut self, kind: &str, receipt: &str) -> R {
        self.store.insert_run(&RunRow {
            need: self.need.clone(),
            run: 1,
            flow: kind.into(),
            flow_version: 1,
            kind: kind.into(),
            entry: "research".into(),
            state: "done".into(),
        })?;
        self.store.insert_step(
            &self.need,
            &StepRow {
                run: 1,
                node: "research".into(),
                task: STEP_TASK.into(),
                state: StepState::Done,
                verdict: None,
            },
        )?;
        self.finish("job-1", 10, receipt);
        Ok(())
    }

    fn finish(&mut self, id: &str, at: u64, receipt: &str) {
        let mut job = PoolJob::from(JobSpec::new(id, "role", "researcher", "s", "b"));
        job.task = Some(STEP_TASK.into());
        job.status = JobStatus::Finished;
        job.outcome = Some(JobOutcome {
            state: "finished".into(),
            detail: String::new(),
            receipt: receipt.into(),
            at,
        });
        self.jobs.jobs.insert(id.into(), job);
    }

    fn tick(&mut self) -> R {
        for phase in self.registry.phases() {
            phase.run(&self.ctx, &mut self.jobs, &self.store, &mut self.mail)?;
        }
        Ok(())
    }

    fn replies(&self) -> Vec<(String, String)> {
        self.mail
            .calls
            .iter()
            .filter_map(|call| match call {
                Call::Reply { handle, body } => Some((handle.clone(), body.clone())),
                _ => None,
            })
            .collect()
    }

    fn marked(&self) -> Result<bool, VivariumError> {
        let key = format!("research-post:{}:r1", self.need);
        Ok(self.store.applied_action_ids()?.contains(&key))
    }
}

#[test]
fn a_done_research_run_posts_one_reply_on_its_want() -> R {
    let mut rig = rig("research it\nfor: <{want}>")?;
    rig.land("research", ENVELOPE)?;
    rig.tick()?;
    let replies = rig.replies();
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].0, rig.want);
    assert_eq!(
        replies[0].1,
        format!(
            "pump: research analysis for need {} r1 (answer READY)\n\n## Verdict\nPlan it.",
            rig.need
        )
    );
    assert!(rig.marked()?);
    Ok(())
}

#[test]
fn a_second_run_of_the_phase_posts_nothing() -> R {
    let mut rig = rig("for: {want}")?;
    rig.land("research", ENVELOPE)?;
    rig.tick()?;
    rig.tick()?;
    assert_eq!(rig.replies().len(), 1);
    Ok(())
}

#[test]
fn a_build_run_posts_nothing() -> R {
    let mut rig = rig("for: {want}")?;
    rig.land("build", ENVELOPE)?;
    rig.tick()?;
    assert!(rig.replies().is_empty());
    assert!(!rig.marked()?);
    Ok(())
}

#[test]
fn a_need_with_no_for_line_posts_nothing_and_marks_the_key() -> R {
    let mut rig = rig("no pointer here")?;
    rig.land("research", ENVELOPE)?;
    rig.tick()?;
    assert!(rig.replies().is_empty());
    assert!(rig.marked()?);
    Ok(())
}

#[test]
fn the_newest_parsable_reply_wins_and_a_broken_one_is_skipped() -> R {
    let mut rig = rig("for: {want}")?;
    rig.land("research", ENVELOPE)?;
    rig.finish("job-2", 20, "not an envelope");
    rig.tick()?;
    let replies = rig.replies();
    assert_eq!(replies.len(), 1);
    assert!(replies[0].1.contains("Plan it."));
    Ok(())
}

#[test]
fn a_failed_reply_is_retried_on_the_next_tick() -> R {
    let mut rig = rig("for: {want}")?;
    rig.land("research", ENVELOPE)?;
    rig.mail.fail_at = Some(rig.mail.calls.len());
    assert!(rig.tick().is_err());
    assert!(!rig.marked()?);
    rig.tick()?;
    assert_eq!(rig.replies().len(), 1);
    assert!(rig.marked()?);
    Ok(())
}

#[test]
fn a_bad_run_does_not_stop_the_runs_after_it() -> R {
    let mut rig = rig("for: {want}")?;
    rig.land("research", ENVELOPE)?;
    // "a-missing" sorts before the real need and has no task in the fake mail.
    rig.store.insert_run(&RunRow {
        need: "a-missing".into(),
        run: 1,
        flow: "research".into(),
        flow_version: 1,
        kind: "research".into(),
        entry: "research".into(),
        state: "done".into(),
    })?;
    assert!(rig.tick().is_err());
    let replies = rig.replies();
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].0, rig.want);
    assert!(rig.marked()?);
    let applied = rig.store.applied_action_ids()?;
    assert!(!applied.contains("research-post:a-missing:r1"));
    Ok(())
}
