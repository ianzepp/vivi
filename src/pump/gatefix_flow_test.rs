//! A gate fix need, end to end, under pump records: the pump sends the need
//! from its own identity, opens its record, the auditor's report mints fix
//! units, a revision is planned and lands, and the units dispatch. Also the
//! recovery of a need an older pump created without a record.

use std::collections::BTreeMap;
use std::fs;
use std::sync::Arc;

use crate::mailspace::{Mailspace, PumpState, SendRequest};
use crate::pump::audit::{collect_completed, current, start_audit};
use crate::pump::config::{DispatchConfig, GateConfig, Kind, Lane};
use crate::pump::fixture::{Project, project};
use crate::pump::host::Ctx;
use crate::pump::jobs::{JobOutcome, JobStatus};
use crate::pump::state::{State, Status};
use crate::routing::{RoleRouting, RoutedPool, RoutedRole};

const TIP: &str = "5b7486c716abcdef0123456789abcdef01234567";

/// The pump's own need: sent from the pump identity to the operator, the way
/// a gate fix need is.
fn gate_fix_need(mailspace: &Mailspace) -> String {
    mailspace
        .send(SendRequest {
            from: "mind".into(),
            to: vec!["operator".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "pump: gate fix for g5b7486c7 (1 new, 0 residual)".into(),
            body: "The integration branch went red.".into(),
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

fn role(name: &str, kind: &str, pool: &str) -> RoutedRole {
    RoutedRole {
        name: name.into(),
        kind: Some(kind.into()),
        status: "active".into(),
        pool: Some(pool.into()),
        read_only: None,
        model: None,
        models: Vec::new(),
    }
}

fn pool() -> RoutedPool {
    RoutedPool {
        slots: 1,
        provider: "local".into(),
        fallback: None,
        caps: BTreeMap::new(),
        models: Vec::new(),
        has_bands: false,
    }
}

/// A pump context over the project with an auditor, a planner and one hand
/// pool wired.
fn pump(project: &Project) -> Ctx {
    let mut ctx = project.pump_ctx().with_routing(RoleRouting {
        roles: vec![
            role("auditor", "auditor", "apool"),
            role("pump-planner", "planner", "here"),
        ],
        pools: BTreeMap::from([(String::from("here"), pool())]),
    });
    let mut cfg = (*ctx.cfg).clone();
    cfg.gate = GateConfig {
        pools: vec![String::from("apool")],
        model: String::from("gatemodel"),
        budget: 600,
        ..GateConfig::default()
    };
    cfg.dispatch = DispatchConfig {
        lanes: vec![Lane {
            model: String::from("m/a"),
            cap: 2,
        }],
        last_resort: String::from("m/last"),
        kinds: BTreeMap::from([(
            String::from("plain"),
            Kind {
                pools: vec![String::from("fly4")],
                ..Kind::default()
            },
        )]),
        default_kind: String::from("plain"),
        ..DispatchConfig::default()
    };
    ctx.cfg = Arc::new(cfg);
    ctx
}

fn finish(st: &mut State, job_id: &str, receipt: &str) {
    let job = st.jobs.get_mut(job_id).unwrap();
    job.status = JobStatus::Finished;
    job.task = Some(String::from("task0001"));
    job.outcome = Some(JobOutcome {
        state: String::from("finished"),
        detail: String::new(),
        receipt: receipt.into(),
        at: 0,
    });
}

fn unit_json(id: &str, brief: &str) -> String {
    format!(
        r#"{{"id":"{id}","title":"{id}","brief":"{brief}","write_scope":["src/{id}.rs"],"proof":[]}}"#
    )
}

#[test]
fn a_gate_fix_need_runs_from_red_gate_to_dispatched_units_under_records() {
    let project = project();
    let host = project.host.clone();
    let ctx = pump(&project);
    let need = gate_fix_need(&project.mailspace);
    host.reply("need send", 0, &format!("created operator {need}\n"));
    host.reply("task send", 0, "created 1234abcd");
    host.reply(
        "pool status",
        0,
        "slot apool bridge slot-1 free\nslot fly4 bridge slot-1 free\n",
    );
    host.reply(
        "pool dispatch",
        0,
        "dispatched 1234abcd bridge slot-1 m/a unit/x-a1",
    );
    let mut st = State::default();

    // Gate red: the pump sends the need and opens its record in one step.
    start_audit(
        &ctx,
        &mut st,
        TIP,
        "test result: FAILED",
        &[String::from("gate::a_fails")],
        &[],
        None,
    );
    let record = project.mailspace.pump_open_record(&need).unwrap();
    let record = record.expect("the gate fix need has an open pump record");
    assert_eq!(record.status, PumpState::Open);
    assert_eq!(st.red["gate::a_fails"], need);
    assert!(crate::pump::gatefix::own_needs(&st).contains(&need));
    assert!(
        host.commands()
            .iter()
            .any(|c| c.contains("need send") && c.contains("gate fix for g5b7486c7"))
    );

    // The auditor reports two root causes.
    let job_id = current(&st).unwrap().job_id.unwrap();
    let report = format!(
        "agent exit 0 in 1s\nAUDIT-BEGIN\n[{},{}]\nAUDIT-END\n",
        r#"{"root_cause":"one","brief":"fix one","write_scope":["src/one.rs"]}"#,
        r#"{"root_cause":"two","brief":"fix two","write_scope":["src/two.rs"]}"#,
    );
    finish(&mut st, &job_id, &report);
    collect_completed(&ctx, &mut st);
    assert_eq!(st.needs[&need].units.len(), 2);
    assert_eq!(st.needs[&need].revision, 1);

    // One unit cannot run as written: a revision is requested and planned.
    let (first, second) = (format!("{need}-1"), format!("{need}-2"));
    st.units.get_mut(&first).unwrap().status = Status::NeedsReplan;
    crate::pump::amend::request(&ctx, &mut st, &need, "proof validation failed: no proof");
    assert!(st.needs[&need].amendment_pending);
    let queued_at = st.board.revision_planning[&need];
    let revision_job =
        crate::pump::planner_jobs::job_id("revision", &format!("{need}.revision"), queued_at);
    let plan = format!(
        r#"{{"need":"{need}","revision":2,"verdict":"units","reason":"fixed proof","units":[{},{}]}}"#,
        unit_json(&first, "fix one properly"),
        unit_json(&second, "fix two"),
    );
    finish(
        &mut st,
        &revision_job,
        &format!(
            "agent_rc 0 agent_s 1\nhand's final message:\nPUMP-PLAN-BEGIN\n{plan}\nPUMP-PLAN-END\n"
        ),
    );
    crate::pump::planner_jobs::apply_completions(&ctx, &mut st);
    crate::pump::plan::ingest(&ctx, &mut st);
    assert_eq!(st.needs[&need].revision, 2);
    assert!(!st.needs[&need].amendment_pending);
    assert_eq!(st.units[&first].status, Status::Pending);
    assert_eq!(st.units[&first].brief, "fix one properly");

    // The record is still the need's one open record.
    assert_eq!(project.mailspace.pump_history(&need).unwrap().len(), 1);
    drop(fs::remove_file(ctx.home.join("exceptions.txt")));
}

#[test]
fn a_revision_plan_that_cannot_be_applied_is_logged_and_raised() {
    let project = project();
    let host = project.host.clone();
    let ctx = pump(&project);
    let need = gate_fix_need(&project.mailspace);
    let mut st = State::default();
    let units = format!("{need}-1");
    st.units.insert(
        units.clone(),
        crate::pump::state::Unit {
            need: need.clone(),
            status: Status::NeedsReplan,
            revision: 1,
            ..crate::pump::state::Unit::default()
        },
    );
    st.needs.insert(
        need.clone(),
        crate::pump::state::NeedRecord {
            verdict: String::from("units"),
            units: vec![units.clone()],
            revision: 1,
            amendment_pending: true,
            ..crate::pump::state::NeedRecord::default()
        },
    );
    // The answer names a proof command the pump refuses.
    let plan = format!(
        r#"{{"need":"{need}","revision":2,"units":[{{"id":"{units}","brief":"b","write_scope":["src/a.rs"],"proof":["cargo test --lib a b"]}}]}}"#
    );
    fs::create_dir_all(ctx.home.join("plans")).unwrap();
    fs::write(ctx.home.join(format!("plans/{need}.r2.json")), plan).unwrap();

    crate::pump::plan::ingest(&ctx, &mut st);

    let exceptions = fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap();
    assert!(
        exceptions.contains(&format!("bad-plan {need}")),
        "{exceptions}"
    );
    assert!(
        host.logs()
            .iter()
            .any(|l| l.contains(&format!("revision r2 of {need} rejected")))
    );
    assert!(st.needs[&need].amendment_pending, "the need still waits");
}

#[test]
fn adopt_opens_the_record_of_a_gate_fix_need_an_older_pump_left_without_one() {
    let project = project();
    let need = gate_fix_need(&project.mailspace);
    // State as the production pump holds it: the need is minted from an audit
    // report, and nothing ever opened a record for it.
    let mut st = State::default();
    st.needs.insert(
        need.clone(),
        crate::pump::state::NeedRecord {
            verdict: String::from("units"),
            reason: String::from("audit report"),
            units: vec![format!("{need}-1")],
            revision: 1,
            amendment_pending: true,
            ..crate::pump::state::NeedRecord::default()
        },
    );
    assert!(project.mailspace.pump_open_record(&need).unwrap().is_none());

    let report = crate::pump::adopt::adopt(&project.mailspace, &st, false).unwrap();

    assert_eq!(report.opened.len(), 1, "{report:?}");
    assert!(report.skipped.is_empty(), "{report:?}");
    assert!(project.mailspace.pump_open_record(&need).unwrap().is_some());
    // `vivi pump send --need` reaches the same state for one need.
    assert!(
        project
            .mailspace
            .pump_send(None, &need, None)
            .unwrap_err()
            .to_string()
            .contains("already has an open pump record")
    );
}
