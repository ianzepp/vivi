//! The repair audit phase: one auditor job per landed repair run.

use std::collections::BTreeMap;
use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use super::{job_id, register, run_key};
use crate::error::VivariumError;
use crate::letter::fixture::repair_record_body;
use crate::letter::records::{NoteWord, RecordFront, RecordKind};
use crate::letter::render;
use crate::mailspace::pump_note::note_body;
use crate::pump::config::PumpConfig;
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::handlers::Registry;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::repair::{self, AttemptFacts};
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::{JobOutcome, JobStatus, JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, NeedBranchRow, PumpStore};
use crate::routing::{RoleRouting, RoutedRole};

type R = Result<(), VivariumError>;

const NEED: &str = "n1";
const BASE: &str = "1111111111111111111111111111111111111111";
const COMMIT: &str = "2222222222222222222222222222222222222222";

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
    host: Arc<FakeHost>,
    store: PumpStore,
    mail: FakeMail,
    jobs: Jobs,
    registry: Registry<Jobs, FakeMail>,
}

fn with_auditor_route(ctx: Ctx) -> Ctx {
    let mut routing: RoleRouting = (*ctx.routing).clone();
    routing.roles.push(RoutedRole {
        name: String::from("auditor"),
        kind: Some(String::from("auditor")),
        status: String::from("active"),
        pool: Some(String::from("test-planner-pool")),
        read_only: Some(true),
        model: None,
        models: Vec::new(),
    });
    ctx.with_routing(routing)
}

fn rig(with_auditor: bool) -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let mut registry = Registry::new();
    register(&mut registry);
    Ok(Rig {
        _dir: dir,
        host,
        ctx: if with_auditor {
            with_auditor_route(ctx)
        } else {
            ctx
        },
        store,
        mail: FakeMail::new("mind"),
        jobs: Jobs::default(),
        registry,
    })
}

impl Rig {
    /// A done run of `kind`; a repair run gets a `fix1` step whose task carries
    /// attempt facts when `facts` is true.
    fn land(&mut self, kind: &str, facts: bool) -> R {
        self.store.insert_run(&RunRow {
            need: NEED.into(),
            run: 1,
            flow: kind.into(),
            flow_version: 2,
            kind: kind.into(),
            entry: "fix1".into(),
            state: "done".into(),
        })?;
        let task = self.mail.send_step("mind", "mind", "fix1", "fix", &[])?;
        self.store.insert_step(
            NEED,
            &StepRow {
                run: 1,
                node: "fix1".into(),
                task: task.clone(),
                state: StepState::Done,
                verdict: Some("built".into()),
            },
        )?;
        if facts {
            self.post_facts(&task, 1, COMMIT)?;
            self.step("merge1", StepState::Done, Some("merged"))?;
        }
        self.store.upsert_need_branch(&NeedBranchRow {
            need: NEED.into(),
            branch: format!("vivi/need/{NEED}"),
            base: Some(BASE.into()),
            status: "live".into(),
            run: 1,
            target: "vivi".into(),
            priors: Vec::new(),
        })
    }

    /// Post attempt facts for `attempt` with `commit` on `task`.
    fn post_facts(&mut self, task: &str, attempt: u32, commit: &str) -> R {
        let text = repair::facts_text(&AttemptFacts {
            branch: repair::branch(NEED, 1, attempt),
            commit: commit.into(),
            base: BASE.into(),
        });
        self.mail
            .reply(task, &note_body(NoteWord::RepairAttempt, None, &text)?)?;
        Ok(())
    }

    /// Add a step row of run 1.
    fn step(&mut self, node: &str, state: StepState, verdict: Option<&str>) -> R {
        let task = self.mail.send_step("mind", "mind", node, node, &[])?;
        self.store.insert_step(
            NEED,
            &StepRow {
                run: 1,
                node: node.into(),
                task,
                state,
                verdict: verdict.map(str::to_owned),
            },
        )
    }

    /// Add a `fix<attempt>` step carrying facts for `commit`; returns nothing.
    fn fix(&mut self, attempt: u32, commit: &str) -> R {
        let node = format!("fix{attempt}");
        let task = self.mail.send_step("mind", "mind", &node, &node, &[])?;
        self.store.insert_step(
            NEED,
            &StepRow {
                run: 1,
                node,
                task: task.clone(),
                state: StepState::Done,
                verdict: Some("built".into()),
            },
        )?;
        self.post_facts(&task, attempt, commit)
    }

    /// End the queued audit job with `status` and `receipt`.
    fn end_audit(&mut self, status: JobStatus, receipt: &str) {
        let job = self.jobs.jobs.get_mut(&job_id(NEED, 1)).expect("audit job");
        job.status = status;
        job.outcome = Some(JobOutcome {
            state: "finished".into(),
            detail: String::new(),
            receipt: receipt.into(),
            at: 1,
        });
    }

    /// Land a repair run, queue its audit and end it with `receipt`.
    fn audited(&mut self, receipt: &str) -> R {
        self.land("repair", true)?;
        self.tick()?;
        self.end_audit(JobStatus::Finished, receipt);
        self.host.reply("need send", 0, "created mind n9\n");
        Ok(())
    }

    fn count(&self, fragment: &str) -> usize {
        self.host
            .commands()
            .iter()
            .filter(|command| command.contains(fragment))
            .count()
    }

    fn alarms(&self) -> usize {
        let raised =
            std::fs::read_to_string(self.ctx.home.join("exceptions.txt")).unwrap_or_default();
        raised.matches(super::REPAIR_AUDIT_FAILED).count()
    }

    fn gave_up(&self) -> usize {
        let raised =
            std::fs::read_to_string(self.ctx.home.join("exceptions.txt")).unwrap_or_default();
        raised.matches(super::REPAIR_AUDIT_GAVE_UP).count()
    }

    fn tick(&mut self) -> R {
        for phase in self.registry.phases() {
            phase.run(&self.ctx, &mut self.jobs, &self.store, &mut self.mail)?;
        }
        Ok(())
    }
}

#[test]
fn a_done_repair_run_with_fix_facts_enqueues_one_auditor_job() -> R {
    let mut rig = rig(true)?;
    rig.land("repair", true)?;
    rig.tick()?;
    assert_eq!(rig.jobs.jobs.len(), 1);
    let job = &rig.jobs.jobs[&job_id(NEED, 1)];
    assert_eq!(job.kind, "auditor");
    assert_eq!(job.role, "auditor");
    assert_eq!(job.target.as_deref(), Some("vivi"));
    assert_eq!(job.from_ref.as_deref(), Some(COMMIT));
    assert!(job.read_only);
    assert_eq!(job.budget, rig.ctx.cfg.dispatch.budget);
    assert_eq!(job.payload["need"], NEED);
    assert_eq!(job.payload["run"], 1);
    assert_eq!(job.payload["commit"], COMMIT);
    assert_eq!(job.payload["base"], BASE);
    assert!(job.body.contains("Repair audit mode"), "{}", job.body);
    assert!(
        job.body.contains(&format!("{BASE}..{COMMIT}")),
        "{}",
        job.body
    );
    Ok(())
}

#[test]
fn an_open_record_adds_the_tested_tip_and_the_failing_tail() -> R {
    let mut rig = rig(true)?;
    rig.mail.open_record(
        NEED,
        &repair_record_body(NEED, "other", Some("abc123"), "the failing tail"),
    )?;
    rig.land("repair", true)?;
    rig.tick()?;
    let job = &rig.jobs.jobs[&job_id(NEED, 1)];
    assert_eq!(job.target.as_deref(), Some("other"));
    assert!(job.body.contains("Tested tip: abc123"), "{}", job.body);
    assert!(job.body.contains("the failing tail"), "{}", job.body);
    Ok(())
}

#[test]
fn a_second_phase_run_enqueues_nothing_more() -> R {
    let mut rig = rig(true)?;
    rig.land("repair", true)?;
    rig.tick()?;
    rig.tick()?;
    assert_eq!(rig.jobs.jobs.len(), 1);
    Ok(())
}

#[test]
fn a_done_build_run_enqueues_nothing() -> R {
    let mut rig = rig(true)?;
    rig.land("build", true)?;
    rig.tick()?;
    assert!(rig.jobs.jobs.is_empty());
    Ok(())
}

#[test]
fn a_run_whose_key_is_applied_enqueues_nothing() -> R {
    let mut rig = rig(true)?;
    rig.land("repair", true)?;
    rig.store.mark_action_applied(&run_key(NEED, 1), 1)?;
    rig.tick()?;
    assert!(rig.jobs.jobs.is_empty());
    Ok(())
}

#[test]
fn a_run_that_published_nothing_is_marked_and_audits_nothing() -> R {
    let mut rig = rig(true)?;
    rig.land("repair", false)?;
    rig.tick()?;
    assert!(rig.jobs.jobs.is_empty());
    assert!(rig.store.applied_action_ids()?.contains(&run_key(NEED, 1)));
    Ok(())
}

const COMMIT2: &str = "3333333333333333333333333333333333333333";

#[test]
fn the_merged_attempt_is_audited_even_when_attempt_one_also_published() -> R {
    let mut rig = rig(true)?;
    rig.land("repair", true)?;
    rig.store.set_step_state(
        &rig.store.steps_of(NEED, 1)?[1].task,
        StepState::Done,
        Some("conflict"),
    )?;
    rig.fix(2, COMMIT2)?;
    rig.step("merge2", StepState::Done, Some("merged"))?;
    rig.tick()?;
    let job = &rig.jobs.jobs[&job_id(NEED, 1)];
    assert_eq!(job.payload["commit"], COMMIT2);
    assert_eq!(job.from_ref.as_deref(), Some(COMMIT2));
    Ok(())
}

#[test]
fn a_run_landed_through_green_queues_nothing_and_settles_its_key() -> R {
    let mut rig = rig(true)?;
    rig.land("repair", false)?;
    rig.fix(1, COMMIT)?;
    rig.fix(2, COMMIT2)?;
    rig.step("merge1", StepState::Done, Some("conflict"))?;
    rig.step("merge2", StepState::Done, Some("failed"))?;
    rig.step("unrepaired", StepState::Done, Some("green"))?;
    rig.tick()?;
    assert!(rig.jobs.jobs.is_empty());
    assert!(rig.store.applied_action_ids()?.contains(&run_key(NEED, 1)));
    assert_eq!(rig.alarms(), 0);
    Ok(())
}

#[test]
fn a_merged_attempt_without_facts_raises_once_and_settles() -> R {
    let mut rig = rig(true)?;
    rig.land("repair", false)?;
    rig.step("merge1", StepState::Done, Some("merged"))?;
    rig.tick()?;
    rig.tick()?;
    assert!(rig.jobs.jobs.is_empty());
    assert_eq!(rig.alarms(), 1);
    assert!(rig.store.applied_action_ids()?.contains(&run_key(NEED, 1)));
    Ok(())
}

#[test]
fn no_auditor_role_raises_once_and_marks_the_key() -> R {
    let mut rig = rig(false)?;
    rig.land("repair", true)?;
    rig.tick()?;
    rig.tick()?;
    assert!(rig.jobs.jobs.is_empty());
    assert!(rig.store.applied_action_ids()?.contains(&run_key(NEED, 1)));
    let raised = std::fs::read_to_string(rig.ctx.home.join("exceptions.txt")).unwrap_or_default();
    assert_eq!(raised.matches(super::REPAIR_AUDIT_FAILED).count(), 1);
    Ok(())
}

#[test]
fn an_unreadable_open_record_names_the_range_only() -> R {
    let mut rig = rig(true)?;
    let front = RecordFront {
        need: NEED.into(),
        kind: Some(RecordKind::Repair),
        target: None,
        tested: None,
    };
    let body = render(&front, "no target here").unwrap();
    rig.mail.open_record(NEED, &body)?;
    rig.land("repair", true)?;
    rig.tick()?;
    let job = &rig.jobs.jobs[&job_id(NEED, 1)];
    assert_eq!(job.target.as_deref(), Some("vivi"));
    assert!(!job.body.contains("Failing output"), "{}", job.body);
    Ok(())
}

const TWO_FINDINGS: &str = "+++\nanswer = \"residual\"\n\n\
    [[finding]]\nid = \"F1\"\ncontent = 'Counted twice.'\npaths = [\"src/a.rs\"]\n\
    recheck_trigger = 'Any change.'\n\n\
    [[finding]]\nid = \"weakened-test-1\"\ncontent = 'A test was ignored.'\n\
    recheck_trigger = 'Any change.'\n+++\nreport\n";

#[test]
fn findings_file_one_need_each_and_a_weakened_test_gets_top_priority() -> R {
    let mut rig = rig(true)?;
    rig.audited(TWO_FINDINGS)?;
    rig.tick()?;
    assert_eq!(rig.count("need send"), 2);
    assert_eq!(rig.count("pump send --need n9"), 2);
    assert_eq!(rig.count("pump priority set n9 100"), 1);
    assert_eq!(rig.count("pump priority"), 1);
    let sent = rig.host.commands();
    let first = sent
        .iter()
        .find(|c| c.contains("need send"))
        .expect("need send");
    assert!(first.contains("repair audit n1 r1: F1"), "{first}");
    assert!(first.contains("src/a.rs"), "{first}");
    assert!(first.contains(&format!("{BASE}..{COMMIT}")), "{first}");
    assert!(first.contains("--done-when 'F1 is resolved'"), "{first}");
    assert!(rig.store.applied_action_ids()?.contains(&run_key(NEED, 1)));
    assert!(
        rig.store
            .applied_action_ids()?
            .contains(&format!("{}:F1", run_key(NEED, 1)))
    );
    Ok(())
}

#[test]
fn a_second_phase_run_files_nothing_more() -> R {
    let mut rig = rig(true)?;
    rig.audited(TWO_FINDINGS)?;
    rig.tick()?;
    rig.tick()?;
    assert_eq!(rig.count("need send"), 2);
    assert_eq!(rig.count("pump send"), 2);
    Ok(())
}

#[test]
fn a_clean_pass_files_nothing_and_marks_the_key() -> R {
    let mut rig = rig(true)?;
    rig.audited("+++\nanswer = \"clean_pass\"\n+++\nall good\n")?;
    rig.tick()?;
    assert_eq!(rig.count("need send"), 0);
    assert!(rig.store.applied_action_ids()?.contains(&run_key(NEED, 1)));
    Ok(())
}

#[test]
fn a_lost_job_raises_one_alarm_and_files_nothing() -> R {
    let mut rig = rig(true)?;
    rig.land("repair", true)?;
    rig.tick()?;
    rig.end_audit(JobStatus::Lost, "");
    rig.tick()?;
    rig.tick()?;
    assert_eq!(rig.count("need send"), 0);
    assert_eq!(rig.alarms(), 1);
    assert!(rig.store.applied_action_ids()?.contains(&run_key(NEED, 1)));
    Ok(())
}

#[test]
fn an_unparseable_reply_raises_one_alarm_and_marks_the_key() -> R {
    let mut rig = rig(true)?;
    rig.audited("no envelope here")?;
    rig.tick()?;
    assert_eq!(rig.count("need send"), 0);
    assert_eq!(rig.alarms(), 1);
    assert!(rig.store.applied_action_ids()?.contains(&run_key(NEED, 1)));
    Ok(())
}

#[test]
fn a_failed_need_send_alarms_once_keeps_the_run_open_and_retries() -> R {
    let mut rig = rig(true)?;
    rig.audited(TWO_FINDINGS)?;
    rig.host.reply("need send", 1, "boom\n");
    rig.tick()?;
    rig.tick()?;
    assert_eq!(rig.alarms(), 2, "once per finding");
    assert!(!rig.store.applied_action_ids()?.contains(&run_key(NEED, 1)));
    rig.host.reply("need send", 0, "created mind n9\n");
    rig.tick()?;
    assert!(rig.store.applied_action_ids()?.contains(&run_key(NEED, 1)));
    assert_eq!(rig.count("pump send --need n9"), 2);
    Ok(())
}

#[test]
fn a_failed_pump_send_retries_without_filing_the_need_twice() -> R {
    let mut rig = rig(true)?;
    rig.audited(TWO_FINDINGS)?;
    rig.host.reply("pump send", 1, "no\n");
    rig.tick()?;
    let filed = rig.count("need send");
    assert_eq!(filed, 2);
    rig.host.reply("pump send", 0, "ok\n");
    rig.tick()?;
    assert_eq!(rig.count("need send"), filed);
    assert!(rig.store.applied_action_ids()?.contains(&run_key(NEED, 1)));
    Ok(())
}

#[test]
fn a_bad_run_does_not_stop_the_next_runs_audit() -> R {
    let mut rig = rig(true)?;
    let bad = "n0";
    rig.store.insert_run(&RunRow {
        need: bad.into(),
        run: 1,
        flow: "repair".into(),
        flow_version: 2,
        kind: "repair".into(),
        entry: "fix1".into(),
        state: "done".into(),
    })?;
    for (node, task, verdict) in [
        ("fix1", "ghost-fix", "built"),
        ("merge1", "ghost-merge", "merged"),
    ] {
        rig.store.insert_step(
            bad,
            &StepRow {
                run: 1,
                node: node.into(),
                task: task.into(),
                state: StepState::Done,
                verdict: Some(verdict.into()),
            },
        )?;
    }
    rig.land("repair", true)?;
    assert!(rig.tick().is_err());
    assert!(rig.jobs.jobs.contains_key(&job_id(NEED, 1)));
    assert!(!rig.jobs.jobs.contains_key(&job_id(bad, 1)));
    assert!(!rig.store.applied_action_ids()?.contains(&run_key(bad, 1)));
    Ok(())
}

#[test]
fn a_finding_that_fails_three_ticks_is_given_up_and_the_run_settles() -> R {
    let mut rig = rig(true)?;
    rig.audited(TWO_FINDINGS)?;
    rig.host.reply("need send", 1, "boom\n");
    rig.tick()?;
    rig.tick()?;
    assert!(!rig.store.applied_action_ids()?.contains(&run_key(NEED, 1)));
    assert_eq!(rig.gave_up(), 0);
    rig.tick()?;
    let applied = rig.store.applied_action_ids()?;
    assert!(applied.contains(&format!("{}:F1", run_key(NEED, 1))));
    assert!(applied.contains(&run_key(NEED, 1)));
    assert_eq!(rig.gave_up(), 2, "one line per finding");
    let sends = rig.count("need send");
    rig.tick()?;
    assert_eq!(rig.count("need send"), sends);
    assert_eq!(rig.gave_up(), 2);
    Ok(())
}
