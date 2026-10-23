//! Tests for residual and requirement-gap need filing.

use std::collections::BTreeMap;
use std::sync::Arc;

use super::file;
use crate::error::VivariumError;
use crate::letter::fixture::record_body;
use crate::pump::config::PumpConfig;
use crate::pump::envelope::review::{
    Auditor, AuditorAnswer, Finding, Requirement, RequirementStatus,
};
use crate::pump::flow::advance::start::start;
use crate::pump::flow::def::{FlowKind, NodeClass};
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::handlers::{self, Registry};
use crate::pump::flow::index::{StepJobRow, StepRow, StepState};
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::tick::tick_with;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::{self, JobOutcome, JobSpec, JobStatus, JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, PumpStore};

fn context(root: &std::path::Path) -> (Ctx, Arc<FakeHost>) {
    let (ctx, host) = test_ctx(root, PumpConfig::default());
    host.reply("need list", 0, "[]");
    host.reply("need send", 0, "created mind abc12345\nsent abc12345\n");
    (ctx, host)
}

fn requirement(id: &str, status: RequirementStatus) -> Requirement {
    Requirement {
        id: id.to_owned(),
        text: format!("requirement {id}"),
        status,
        evidence: Vec::new(),
    }
}

fn finding(id: &str) -> Finding {
    Finding {
        id: id.to_owned(),
        content: format!("finding {id}"),
        paths: vec![String::from("src/a.rs")],
        recheck_trigger: String::from("recheck a.rs"),
    }
}

fn front(answer: AuditorAnswer) -> Auditor {
    Auditor {
        answer,
        raise_rung: None,
        requirement: vec![
            requirement("R1", RequirementStatus::Satisfied),
            requirement("R2", RequirementStatus::Missing),
        ],
        finding: vec![finding("F1")],
    }
}

fn send_commands(host: &FakeHost) -> Vec<String> {
    host.commands()
        .into_iter()
        .filter(|c| c.contains("need send"))
        .collect()
}

#[test]
fn residual_files_finding_and_gap() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, host) = context(dir.path());
    let handles = file(&ctx, "n1", 1, "audit-task", &front(AuditorAnswer::Residual)).unwrap();
    assert_eq!(handles.len(), 2);
    let sends = send_commands(&host);
    assert_eq!(sends.len(), 2);
    assert!(sends.iter().any(|c| c.contains("audit requirement gap n1")));
    assert!(sends.iter().any(|c| c.contains("audit finding n1")));
    assert!(
        sends.iter().all(|c| c.contains(" --done-when ")),
        "{sends:?}"
    );
    assert!(
        sends
            .iter()
            .any(|c| c.contains("--done-when 'recheck a.rs'")),
        "{sends:?}"
    );
}

#[test]
fn a_blank_recheck_trigger_files_with_a_fixed_done_when() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, host) = context(dir.path());
    let mut residual = front(AuditorAnswer::Residual);
    residual.requirement.clear();
    residual.finding[0].recheck_trigger = String::from("  ");
    file(&ctx, "n1", 1, "audit-task", &residual).unwrap();
    let sends = send_commands(&host);
    assert_eq!(sends.len(), 1, "{sends:?}");
    assert!(
        sends[0].contains("--done-when 'the residual finding is resolved'"),
        "{sends:?}"
    );
}

#[test]
fn clean_pass_without_gaps_files_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, host) = context(dir.path());
    let mut clean = front(AuditorAnswer::CleanPass);
    clean.requirement = vec![requirement("R1", RequirementStatus::Satisfied)];
    assert!(
        file(&ctx, "n1", 1, "audit-task", &clean)
            .unwrap()
            .is_empty()
    );
    assert!(send_commands(&host).is_empty());
}

#[test]
fn block_range_files_no_findings() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, host) = context(dir.path());
    let mut blocked = front(AuditorAnswer::BlockRange);
    blocked.requirement.clear();
    assert!(
        file(&ctx, "n1", 1, "audit-task", &blocked)
            .unwrap()
            .is_empty()
    );
    assert!(send_commands(&host).is_empty());
}

#[test]
fn block_range_with_a_missing_requirement_files_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, host) = context(dir.path());
    let mut blocked = front(AuditorAnswer::BlockRange);
    blocked.finding.clear();
    assert!(
        file(&ctx, "n1", 1, "audit-task", &blocked)
            .unwrap()
            .is_empty()
    );
    assert!(send_commands(&host).is_empty());
}

#[test]
fn block_ship_with_a_gap_files_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, host) = context(dir.path());
    let handles = file(
        &ctx,
        "n1",
        1,
        "audit-task",
        &front(AuditorAnswer::BlockShip),
    )
    .unwrap();
    assert!(handles.is_empty());
    assert!(send_commands(&host).is_empty());
}

#[test]
fn clean_pass_with_a_gap_files_one_need() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, host) = context(dir.path());
    let handles = file(
        &ctx,
        "n1",
        1,
        "audit-task",
        &front(AuditorAnswer::CleanPass),
    )
    .unwrap();
    assert_eq!(handles.len(), 1);
    let sends = send_commands(&host);
    assert_eq!(sends.len(), 1);
    assert!(sends[0].contains("audit requirement gap n1"));
}

#[test]
fn residual_with_a_gap_and_a_finding_files_two_needs() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, host) = context(dir.path());
    let handles = file(&ctx, "n1", 1, "audit-task", &front(AuditorAnswer::Residual)).unwrap();
    assert_eq!(handles.len(), 2);
    assert_eq!(send_commands(&host).len(), 2);
}

#[test]
fn repeat_with_same_keys_files_nothing_new() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, host) = context(dir.path());
    let auditor = front(AuditorAnswer::Residual);
    file(&ctx, "n1", 1, "audit-task", &auditor).unwrap();
    let rows: Vec<String> = send_commands(&host)
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let after = c.split("--subject ").nth(1).unwrap();
            let subject = after
                .split(" --body-file")
                .next()
                .unwrap()
                .trim_matches('\'');
            format!("{{\"handle\":\"h{i}\",\"date\":\"2026-10-23\",\"subject\":{subject:?}}}")
        })
        .collect();
    host.reply("need list", 0, &format!("[{}]", rows.join(",")));
    let before = send_commands(&host).len();
    let handles = file(&ctx, "n1", 1, "audit-task", &auditor).unwrap();
    assert_eq!(handles, vec![String::from("h0"), String::from("h1")]);
    assert_eq!(send_commands(&host).len(), before);
}

// ---- the build auditor collector ------------------------------------------------

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

const RESIDUAL: &str = "+++\nanswer = \"residual\"\n\n[[finding]]\nid = \"F1\"\n\
                        content = 'a defect'\npaths = [\"src/a.rs\"]\n\
                        recheck_trigger = 'recheck a.rs'\n+++\nthe audit";

#[test]
fn the_shipped_registry_files_residual_needs_from_a_build_audit() -> Result<(), VivariumError> {
    let dir = tempfile::tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, host) = context(dir.path());
    let registry: Registry<Jobs, FakeMail> = handlers::shipped(&ctx);
    assert!(
        registry
            .collector_for(FlowKind::Build, &NodeClass::Auditor)
            .is_some()
    );
    assert!(
        registry
            .collector_for(FlowKind::Research, &NodeClass::Auditor)
            .is_some()
    );

    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let mut mail = FakeMail::new("mind");
    MailWriter::open_record(&mut mail, "n1", &record_body("n1", "build"))?;
    start(&ctx, &store, &mut mail, "n1", Some("plan"))?;
    let task = mail.send_step(
        "mind",
        "mind",
        "audit",
        &crate::letter::fixture::step_body("n1", 1, "audit1", ""),
        &[],
    )?;
    store.insert_step(
        "n1",
        &StepRow {
            run: 1,
            node: "audit1".into(),
            task: task.clone(),
            state: StepState::Running,
            verdict: None,
        },
    )?;
    let mut jobs = Jobs::default();
    let mut spec = JobSpec::new("audit-j1", "auditor", "auditor", "s", "b");
    spec.task = Some(task.clone());
    jobs::enqueue(&mut jobs, spec)?;
    store.insert_step_job(
        "n1",
        &StepJobRow {
            job: "audit-j1".into(),
            run: 1,
            node: "audit1".into(),
            task: task.clone(),
            attempt: 1,
        },
    )?;
    let job = jobs.jobs.get_mut("audit-j1").expect("job");
    job.status = JobStatus::Finished;
    job.outcome = Some(JobOutcome {
        state: "finished".into(),
        detail: String::new(),
        receipt: format!(
            "pool result audit-j1: finished\npool p  slot s1  model m\nfrom main  default\n\
             push: \nagent exit 0 in 12s, job 20s, started from 1234567890\n\n\
             --- receipt\n{RESIDUAL}\n\n--- stderr tail\n\n\n--- runner log tail\nlog line"
        ),
        at: 0,
    });

    tick_with(&ctx, &mut jobs, &store, &mut mail, &registry)?;

    let exceptions = std::fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap_or_default();
    assert_eq!(send_commands(&host).len(), 1, "exceptions: {exceptions}");
    let audit = store
        .steps_of("n1", 1)?
        .into_iter()
        .find(|row| row.node == "audit1")
        .expect("audit step");
    assert_eq!(audit.state, StepState::Done);
    assert_eq!(audit.verdict.as_deref(), Some("residual"));
    Ok(())
}

#[test]
fn need_handle_comes_from_the_created_line_then_the_sent_line() {
    use super::parse_need_handle;
    assert_eq!(
        parse_need_handle("created mind 1a2b3c4d\n").as_deref(),
        Some("1a2b3c4d")
    );
    assert_eq!(
        parse_need_handle("sent 5e6f7a8b\n").as_deref(),
        Some("5e6f7a8b")
    );
    assert_eq!(
        parse_need_handle("sent 5e6f7a8b\ncreated mind 1a2b3c4d\n").as_deref(),
        Some("1a2b3c4d")
    );
    assert_eq!(parse_need_handle(""), None);
    assert_eq!(parse_need_handle("garbled output\nnothing here"), None);
    assert_eq!(parse_need_handle("created mind"), None);
}
