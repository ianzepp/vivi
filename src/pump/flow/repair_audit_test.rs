//! The repair audit phase: one auditor job per landed repair run.

use std::collections::BTreeMap;

use tempfile::{TempDir, tempdir};

use super::{job_id, register, run_key};
use crate::error::VivariumError;
use crate::mailspace::marked_line;
use crate::pump::config::PumpConfig;
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::handlers::Registry;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::repair::{self, AttemptFacts};
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::jobs::{JobStore, PoolJob};
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
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let mut registry = Registry::new();
    register(&mut registry);
    Ok(Rig {
        _dir: dir,
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
            flow_version: 1,
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
            let text = repair::facts_text(&AttemptFacts {
                branch: repair::branch(NEED, 1),
                commit: COMMIT.into(),
                base: BASE.into(),
            });
            self.mail
                .reply(&task, &marked_line(repair::ATTEMPT_MARKER, &text))?;
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
        "kind: repair\ntarget: other\ntested: abc123\nthe failing tail",
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
    rig.mail.open_record(NEED, "kind: repair\nno target here")?;
    rig.land("repair", true)?;
    rig.tick()?;
    let job = &rig.jobs.jobs[&job_id(NEED, 1)];
    assert_eq!(job.target.as_deref(), Some("vivi"));
    assert!(!job.body.contains("Failing output"), "{}", job.body);
    Ok(())
}
