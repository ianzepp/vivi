//! Tests for [`crate::pump::audit`]: audit state round-trips through `extra`
//! and the auditor brief carries failures, rules, and any prior report.

use std::collections::BTreeMap;
use std::fs;
use std::sync::Arc;

use tempfile::tempdir;

use crate::pump::audit::{
    AuditRun, Cause, apply_report, audit_brief, current, parse_report, prior_reports, start_audit,
    store,
};
use crate::pump::config::{GateConfig, PumpConfig};
use crate::pump::gatefix::fix_need;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::{JobOutcome, JobSpec, JobStatus, PoolJob, dispatch_pending};
use crate::pump::state::{State, Status, Unit};
use crate::routing::{RoleRouting, RoutedRole};

/// A 40-hex integration tip; its first eight characters name the fix need.
const TIP: &str = "0123456789abcdef0123456789abcdef01234567";

/// Pump config with one gate pool, the gate model and a gate budget.
fn audit_cfg() -> PumpConfig {
    PumpConfig {
        gate: GateConfig {
            pools: vec![String::from("apool")],
            model: String::from("gatemodel"),
            budget: 600,
            ..GateConfig::default()
        },
        ..PumpConfig::default()
    }
}

/// Routing with one auditor role that supplies its own pool.
fn auditor_routing() -> RoleRouting {
    RoleRouting {
        roles: vec![RoutedRole {
            name: String::from("auditor"),
            kind: Some(String::from("auditor")),
            status: String::from("active"),
            pool: Some(String::from("apool")),
            read_only: None,
            model: None,
            models: Vec::new(),
        }],
        pools: BTreeMap::new(),
    }
}

/// The vivi replies an audit start needs: a free slot, a task handle and a
/// dispatch receipt.
fn ready(host: &FakeHost) {
    host.reply(
        "pool status --porcelain",
        0,
        "slot apool bridge slot-1 free",
    );
    host.reply("task send", 0, "created 1234abcd");
    host.reply("pool dispatch", 0, "dispatched 1234abcd to apool");
}

/// The `pool dispatch` command line an audit start ran.
fn dispatch_command(host: &FakeHost) -> String {
    host.commands()
        .into_iter()
        .find(|cmd| cmd.contains("pool dispatch"))
        .expect("no pool dispatch command ran")
}

/// A context with [`audit_cfg`] and its fake host.
fn setup(routing: Option<RoleRouting>) -> (tempfile::TempDir, Ctx, std::sync::Arc<FakeHost>) {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), audit_cfg());
    let ctx = match routing {
        Some(routing) => ctx.with_routing(routing),
        None => ctx,
    };
    (dir, ctx, host)
}

#[test]
fn audit_run_round_trips_through_state_extra() {
    let dir = tempdir().unwrap();
    let (_ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let mut st = State::default();
    assert!(current(&st).is_none());

    let run = AuditRun {
        task: Some(String::from("task-1")),
        job_id: Some(String::from("audit-1")),
        need: String::from("pumpfix-g01234567"),
        tip: String::from("abc1234"),
        pool: Some(String::from("here")),
        at: 1_000_000,
        fails: 2,
        retry_at: Some(1_000_060),
        new: 1,
        residual: 2,
        failures: vec![String::from("gate::a"), String::from("gate::b")],
        attempts: 2,
        exhausted: false,
        last_error: Some(String::from("timeout")),
    };
    store(&mut st, Some(run.clone()));
    assert_eq!(current(&st), Some(run));

    store(&mut st, None);
    assert!(current(&st).is_none());
}

#[test]
fn audit_brief_carries_failings_rules_and_prior() {
    let dir = tempdir().unwrap();
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let new = vec![String::from("gate::a_fails"), String::from("gate::b_fails")];
    let residual = vec![(String::from("gate::old_fails"), String::from("pumpfix-g1"))];
    let prior = "cause: off-by-one in x";
    let brief = audit_brief(
        &ctx,
        "abc1234",
        "test result: FAILED",
        &new,
        &residual,
        Some(prior),
    );

    for name in ["gate::a_fails", "gate::b_fails", "gate::old_fails"] {
        assert!(brief.contains(name), "brief is missing {name}");
    }
    assert!(brief.contains("abc1234"));
    assert!(brief.contains("no suspect lists, no bisecting, no culprit commit"));
    assert!(brief.contains("It must never weaken, skip or delete tests."));
    assert!(brief.contains("Still failing after earlier fixes:"));
    assert!(brief.contains("Gate output (tail):"));
    assert!(brief.contains("test result: FAILED"));
    assert!(brief.contains("AUDIT-BEGIN"));
    assert!(brief.contains("AUDIT-END"));
    assert!(brief.contains("done_when:"));
    assert!(brief.contains("cause: off-by-one in x"));

    let plain = audit_brief(&ctx, "abc1234", "test result: FAILED", &new, &[], None);
    assert!(!plain.contains("cause: off-by-one in x"));
    assert!(!plain.contains("Still failing after earlier fixes:"));
    assert!(!plain.contains("Prior audit report"));
    assert!(plain.contains("gate::a_fails"));
    assert!(plain.contains("AUDIT-BEGIN"));
}

#[test]
fn parse_report_reads_the_last_marker_block() {
    let earlier = r#"[{"root_cause":"old cause","brief":"old brief","write_scope":["old.rs"]}]"#;
    let latest = r#"[
        {
            "root_cause": "root cause two",
            "brief": "concrete fix for the failing behavior",
            "tests": ["pump::audit_test::first", "pump::audit_test::second"],
            "write_scope": ["src/pump/audit.rs"],
            "proof": ["cargo test --lib pump::audit"]
        }
    ]"#;
    let text = format!(
        "preamble\nAUDIT-BEGIN\n{earlier}\nAUDIT-END\nextra prose\n\
         AUDIT-BEGIN\n{latest}\nAUDIT-END\ntrailer"
    );

    let causes = parse_report(&text).expect("the final report block should parse");
    assert_eq!(causes.len(), 1);
    assert_eq!(causes[0].root_cause, "root cause two");
    assert_eq!(causes[0].brief, "concrete fix for the failing behavior");
    assert_eq!(
        causes[0].tests,
        ["pump::audit_test::first", "pump::audit_test::second"]
    );
    assert_eq!(causes[0].write_scope, ["src/pump/audit.rs"]);
    assert_eq!(causes[0].proof, ["cargo test --lib pump::audit"]);
}

#[test]
fn parse_report_rejects_missing_markers_bad_json_and_empty_scopes() {
    assert!(parse_report("plain reply without markers").is_none());
    assert!(parse_report("AUDIT-BEGIN\nnot json\nAUDIT-END").is_none());
    assert!(parse_report("AUDIT-BEGIN\n[]\nAUDIT-END").is_none());
    let empty_scope = r#"[{"root_cause":"broken","brief":"fix it","write_scope":[]}]"#;
    let empty_scope_report = format!("AUDIT-BEGIN\n{empty_scope}\nAUDIT-END");
    assert!(parse_report(&empty_scope_report).is_none());
}

#[test]
fn apply_report_mints_one_unit_per_cause() {
    let (_dir, ctx, host) = setup(None);
    let mut st = State::default();
    let causes = vec![
        Cause {
            root_cause: String::from("first root cause"),
            brief: String::from("fix the first cause"),
            tests: vec![String::from("pump::first")],
            write_scope: vec![String::from("src/first.rs")],
            proof: vec![String::from("cargo test --lib pump::first")],
        },
        Cause {
            root_cause: String::from("second root cause"),
            brief: String::from("fix the second cause"),
            tests: vec![String::from("pump::second")],
            write_scope: vec![String::from("src/second.rs")],
            proof: vec![String::from("cargo test --lib pump::second")],
        },
    ];
    let raw = "AUDIT-BEGIN\nraw report\nAUDIT-END\n";

    assert_eq!(apply_report(&ctx, &mut st, "need-audit", raw, &causes), 2);
    assert_eq!(st.order, ["need-audit-1", "need-audit-2"]);
    for uid in &st.order {
        assert_eq!(st.units[uid].status, Status::Pending);
        assert_eq!(st.units[uid].need, "need-audit");
    }
    assert_eq!(st.units["need-audit-1"].title, "first root cause");
    assert_eq!(st.units["need-audit-1"].brief, "fix the first cause");
    assert_eq!(st.units["need-audit-1"].write_scope, ["src/first.rs"]);
    assert_eq!(
        st.units["need-audit-1"].proof,
        ["cargo test --lib pump::first"]
    );
    assert_eq!(st.units["need-audit-1"].tools, "");

    let record = &st.needs["need-audit"];
    assert_eq!(record.verdict, "units");
    assert_eq!(record.reason, "audit report");
    assert_eq!(record.units, ["need-audit-1", "need-audit-2"]);
    assert_eq!(record.revision, 1);
    assert_eq!(
        fs::read_to_string(ctx.home.join("audit/need-audit.report")).unwrap(),
        raw
    );
    assert!(
        host.logs()
            .iter()
            .any(|line| line.contains("audit report for need-audit: 2 fix units"))
    );
}

#[test]
fn prior_reports_reads_distinct_residual_owners_and_caps_the_context() {
    let (_dir, ctx, _host) = setup(None);
    let audit_dir = ctx.home.join("audit");
    fs::create_dir_all(&audit_dir).unwrap();
    fs::write(audit_dir.join("owner-a.report"), "first cause\n").unwrap();
    fs::write(audit_dir.join("owner-b.report"), "x".repeat(5000)).unwrap();
    let residual = vec![
        (String::from("test::a"), String::from("owner-a")),
        (String::from("test::b"), String::from("owner-a")),
        (String::from("test::c"), String::from("owner-b")),
    ];

    let prior = prior_reports(&ctx, &residual).expect("prior reports should be read");

    assert!(prior.contains("prior report for owner-a:\nfirst cause"));
    assert!(prior.contains("prior report for owner-b:"));
    assert_eq!(prior.chars().count(), 4000);
    assert_eq!(prior.matches("prior report for owner-a:").count(), 1);
}

#[test]
fn a_completed_audit_mints_fix_units_and_applies_once() {
    let (_dir, ctx, host) = setup(None);
    ready(&host);
    let mut st = State::default();
    start_audit(
        &ctx,
        &mut st,
        TIP,
        "test result: FAILED",
        &[String::from("gate::a_fails")],
        &[],
        None,
    );
    let job_id = current(&st).unwrap().job_id.unwrap();
    let job = st.jobs.get_mut(&job_id).unwrap();
    job.status = JobStatus::Finished;
    job.task = Some(String::from("audit-task"));
    job.outcome = Some(JobOutcome {
        state: String::from("finished"),
        detail: String::new(),
        receipt: String::from(
            "agent exit 0 in 1s\nAUDIT-BEGIN\n[{\"root_cause\":\"broken\",\"brief\":\"fix it\",\"write_scope\":[\"src/a.rs\"]}]\nAUDIT-END\n",
        ),
        at: ctx.now(),
    });

    crate::pump::audit::collect_completed(&ctx, &mut st);
    assert!(current(&st).is_none());
    assert_eq!(st.units["pumpfix-g01234567-1"].title, "broken");
    assert_eq!(st.jobs[&job_id].applied_at, Some(ctx.now()));
    crate::pump::audit::collect_completed(&ctx, &mut st);
    assert_eq!(st.order, ["pumpfix-g01234567-1"]);
}

#[test]
fn a_non_default_targets_terminal_audit_job_is_collected_and_its_units_minted() {
    let (_dir, mut ctx, host) = setup(None);
    let mut cfg = (*ctx.cfg).clone();
    cfg.targets.insert(
        String::from("gradus"),
        crate::pump::config::PumpTarget {
            path: String::from("gradus"),
            branch: String::from("main"),
            ..crate::pump::config::PumpTarget::default()
        },
    );
    ctx.cfg = Arc::new(cfg);
    ready(&host);
    let gradus = ctx.for_target("gradus").expect("gradus target resolves");
    let mut st = State::default();
    crate::pump::state::with_target_state(&mut st, "gradus", |target_st| {
        start_audit(
            &gradus,
            target_st,
            TIP,
            "test result: FAILED",
            &[String::from("gate::a_fails")],
            &[],
            None,
        );
    });
    let job_id = format!("audit-gradus-{}-1", fix_need(&ctx, TIP));
    assert!(st.jobs.contains_key(&job_id), "{:?}", st.jobs.keys());
    // The pool finished the job; its receipt is waiting for collection.
    let job = st.jobs.get_mut(&job_id).unwrap();
    job.status = JobStatus::Finished;
    job.task = Some(String::from("audit-task"));
    job.outcome = Some(JobOutcome {
        state: String::from("finished"),
        detail: String::new(),
        receipt: String::from(
            "agent exit 0 in 1s\nAUDIT-BEGIN\n[{\"root_cause\":\"broken\",\"brief\":\"fix it\",\"write_scope\":[\"src/a.rs\"]}]\nAUDIT-END\n",
        ),
        at: ctx.now(),
    });
    assert!(st.target_states["gradus"].extra.contains_key("audit"));

    crate::pump::audit::collect_completed(&ctx, &mut st);

    let uid = format!("{}-1", fix_need(&ctx, TIP));
    assert_eq!(st.units[&uid].title, "broken");
    assert_eq!(st.units[&uid].target, "gradus");
    assert_eq!(st.jobs[&job_id].applied_at, Some(ctx.now()));
    assert!(!st.target_states["gradus"].extra.contains_key("audit"));
    // Applied exactly once: a second collection mints nothing more.
    crate::pump::audit::collect_completed(&ctx, &mut st);
    assert_eq!(st.order, [uid]);
}

#[test]
fn an_audit_timeout_backs_off_then_fails_over_to_another_pool() {
    let (_dir, mut ctx, host) = setup(None);
    let mut cfg = (*ctx.cfg).clone();
    cfg.gate.pools = vec![String::from("g1"), String::from("g2")];
    ctx.cfg = Arc::new(cfg);
    host.reply(
        "pool status --porcelain",
        0,
        "slot g1 bridge slot-1 free\nslot g2 bridge slot-2 free\n",
    );
    host.reply("task send", 0, "created 5678abcd");
    host.reply("pool dispatch", 0, "dispatched 5678abcd to g2");
    let mut st = State::default();
    start_audit(
        &ctx,
        &mut st,
        TIP,
        "test result: FAILED",
        &[String::from("gate::a_fails")],
        &[],
        None,
    );
    let first_id = current(&st).unwrap().job_id.unwrap();
    let first = st.jobs.get_mut(&first_id).unwrap();
    first.pool = Some(String::from("g1"));
    first.status = JobStatus::Timeout;
    first.outcome = Some(JobOutcome {
        state: String::from("timeout"),
        detail: String::from("budget exceeded"),
        receipt: String::from("agent exit 124 after 600s"),
        at: ctx.now(),
    });

    crate::pump::audit::collect_completed(&ctx, &mut st);
    assert_eq!(current(&st).unwrap().retry_at, Some(ctx.now() + 60));
    let commands = host.commands().len();
    host.advance(59);
    crate::pump::audit::retry_due(&ctx, &mut st);
    assert_eq!(host.commands().len(), commands);
    host.advance(1);
    crate::pump::audit::retry_due(&ctx, &mut st);
    let run = current(&st).unwrap();
    let retry_id = run.job_id.clone().unwrap();
    assert_ne!(retry_id, first_id);
    let retry = &st.jobs[&retry_id];
    assert_eq!(retry.pool_candidates, ["g2"]);
    assert_eq!(retry.pool_exclude, ["g1"]);
    dispatch_pending(&ctx, &mut st).unwrap();
    assert!(dispatch_command(&host).contains("--pool g2"));
}

#[test]
fn a_provider_error_retries_then_a_valid_audit_creates_fix_units() {
    let (_dir, mut ctx, host) = setup(None);
    let mut cfg = (*ctx.cfg).clone();
    cfg.gate.pools = vec![String::from("g1"), String::from("g2")];
    ctx.cfg = Arc::new(cfg);
    host.reply(
        "pool status --porcelain",
        0,
        "slot g1 bridge slot-1 free\nslot g2 bridge slot-2 free\n",
    );
    host.reply("task send", 0, "created 1111aaaa");
    host.reply("pool dispatch", 0, "dispatched 1111aaaa to g1");
    host.reply("task send", 0, "created 2222bbbb");
    host.reply("pool dispatch", 0, "dispatched 2222bbbb to g2");
    let mut st = State::default();
    start_audit(
        &ctx,
        &mut st,
        TIP,
        "test result: FAILED",
        &[String::from("gate::a_fails")],
        &[],
        None,
    );
    let first_id = current(&st).unwrap().job_id.unwrap();
    let first = st.jobs.get_mut(&first_id).unwrap();
    first.pool = Some(String::from("g1"));
    first.status = JobStatus::Finished;
    first.outcome = Some(JobOutcome {
        state: String::from("finished"),
        detail: String::from("agent failed"),
        receipt: String::from(
            "pool result j1: finished\nagent exit 1 in 0s\n\n--- stderr tail\nConnection error.\n",
        ),
        at: ctx.now(),
    });

    crate::pump::audit::collect_completed(&ctx, &mut st);
    let run = current(&st).unwrap();
    assert_eq!(run.fails, 1);
    assert_eq!(run.retry_at, Some(ctx.now() + 60));
    assert!(!run.exhausted);
    let exception = fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap();
    assert!(exception.contains("environment"), "{exception}");
    assert!(exception.contains("exit=1"), "{exception}");
    assert!(exception.contains("Connection error."), "{exception}");
    assert!(host.logs().iter().any(|line| {
        line.contains("audit retry 1")
            && line.contains("exit=1")
            && line.contains("Connection error.")
    }));

    host.advance(60);
    crate::pump::audit::retry_due(&ctx, &mut st);
    let retry_id = current(&st).unwrap().job_id.clone().unwrap();
    assert_ne!(retry_id, first_id);
    assert_eq!(st.jobs[&retry_id].pool_candidates, ["g2"]);
    dispatch_pending(&ctx, &mut st).unwrap();
    let retry = st.jobs.get_mut(&retry_id).unwrap();
    retry.status = JobStatus::Finished;
    retry.outcome = Some(JobOutcome {
        state: String::from("finished"),
        detail: String::new(),
        receipt: String::from(
            "agent exit 0 in 1s\nAUDIT-BEGIN\n[{\"root_cause\":\"broken\",\"brief\":\"fix it\",\"write_scope\":[\"src/a.rs\"]}]\nAUDIT-END\n",
        ),
        at: ctx.now(),
    });
    crate::pump::audit::collect_completed(&ctx, &mut st);

    assert!(current(&st).is_none());
    assert_eq!(st.units["pumpfix-g01234567-1"].status, Status::Pending);
    assert!(
        host.logs()
            .iter()
            .any(|line| line.contains("audit retry 1"))
    );
}

#[test]
fn an_unavailable_auditor_slot_defers_without_spending_a_failed_attempt() {
    let (_dir, ctx, host) = setup(None);
    let mut st = State::default();
    start_audit(
        &ctx,
        &mut st,
        TIP,
        "tail",
        &[String::from("gate::a_fails")],
        &[],
        None,
    );
    let run = current(&st).unwrap();
    assert_eq!(run.fails, 0);
    assert_eq!(run.attempts, 0);
    assert_eq!(run.retry_at, Some(ctx.now() + 60));
    assert!(run.job_id.is_none());
    assert!(st.jobs.is_empty());
    host.advance(60);
    crate::pump::audit::retry_due(&ctx, &mut st);
    let run = current(&st).unwrap();
    assert_eq!(run.fails, 0);
    assert_eq!(run.attempts, 0);
    assert_eq!(run.retry_at, Some(ctx.now() + 60));
}

#[test]
fn an_audit_stops_after_three_failures_and_does_not_restart_the_same_red_result() {
    let (_dir, ctx, host) = setup(None);
    ready(&host);
    let mut st = State::default();
    let failure = vec![String::from("gate::a_fails")];
    start_audit(&ctx, &mut st, TIP, "tail", &failure, &[], None);

    for attempt in 1..=crate::pump::audit::MAX_AUDIT_ATTEMPTS {
        let job_id = current(&st).unwrap().job_id.clone().unwrap();
        let job = st.jobs.get_mut(&job_id).unwrap();
        job.status = JobStatus::Finished;
        job.outcome = Some(JobOutcome {
            state: String::from("finished"),
            detail: String::from("agent failed"),
            receipt: String::from("agent exit 1 in 1s\n--- receipt\nno audit report\n"),
            at: ctx.now(),
        });
        crate::pump::audit::collect_completed(&ctx, &mut st);
        let run = current(&st).unwrap();
        assert_eq!(run.fails, attempt);
        assert_eq!(
            run.exhausted,
            attempt == crate::pump::audit::MAX_AUDIT_ATTEMPTS
        );
        if attempt < crate::pump::audit::MAX_AUDIT_ATTEMPTS {
            let wait = if attempt == 1 { 60 } else { 120 };
            host.advance(wait);
            crate::pump::audit::retry_due(&ctx, &mut st);
        }
    }

    let run = current(&st).unwrap();
    assert_eq!(run.attempts, crate::pump::audit::MAX_AUDIT_ATTEMPTS);
    assert_eq!(run.retry_at, None);
    let jobs = st.jobs.len();
    start_audit(&ctx, &mut st, TIP, "tail", &failure, &[], None);
    assert_eq!(st.jobs.len(), jobs);
    let changed = vec![String::from("gate::b_fails")];
    start_audit(&ctx, &mut st, TIP, "tail", &changed, &[], None);
    let restarted = current(&st).unwrap();
    assert!(!restarted.exhausted);
    assert_eq!(restarted.fails, 0);
    assert_eq!(restarted.attempts, 4);
    assert!(restarted.job_id.unwrap().ends_with("-4"));
    let exception = fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap();
    assert!(exception.contains("gatefix-planner-failed"), "{exception}");
    assert!(exception.contains("vivi pump replan"), "{exception}");
}

#[test]
fn status_explains_exhausted_red_work_and_operator_recovery() {
    let (_dir, ctx, _host) = setup(None);
    let mut st = State::default();
    let run = AuditRun {
        job_id: Some(String::from("audit-job")),
        need: String::from("pumpfix-g01234567"),
        tip: TIP.to_owned(),
        fails: 1,
        attempts: 2,
        last_error: Some(String::from("provider: Connection error.")),
        ..AuditRun::default()
    };
    st.red
        .insert(run.need.clone(), String::from("pumpfix-g01234567"));
    let mut job = PoolJob::from(JobSpec::new(
        "audit-job",
        "audit",
        "auditor",
        "audit subject",
        "audit body",
    ));
    job.status = JobStatus::Dispatched;
    st.jobs.insert(String::from("audit-job"), job);
    store(&mut st, Some(run));

    let lines = crate::pump::audit::status_lines(&ctx, &st);
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("gatefix-active"), "{lines:?}");
    assert!(lines[0].contains("audit=in-flight"), "{lines:?}");
    assert!(lines[0].contains("jobs=2 failures=1/3"), "{lines:?}");
    let mut run = current(&st).unwrap();
    run.job_id = None;
    run.fails = 3;
    run.attempts = 3;
    run.exhausted = true;
    store(&mut st, Some(run));
    let lines = crate::pump::audit::status_lines(&ctx, &st);
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("gatefix-no-live-work"), "{lines:?}");
    assert!(lines[0].contains("audit=failed"), "{lines:?}");
    assert!(lines[0].contains("failures=3/3"), "{lines:?}");
    assert!(lines[0].contains("Connection error."), "{lines:?}");
    assert!(
        lines[0].contains("vivi pump replan pumpfix-g01234567"),
        "{lines:?}"
    );
    crate::pump::run::write_status(&ctx, &st, &[]);
    let status = fs::read_to_string(ctx.home.join("status.txt")).unwrap();
    assert!(status.contains("gatefix-no-live-work"), "{status}");
    assert!(
        status.contains("vivi pump replan pumpfix-g01234567"),
        "{status}"
    );
    assert!(
        !status.contains("red owned by pumpfix-g01234567"),
        "{status}"
    );
}

#[test]
fn start_audit_dispatches_to_the_auditor_role() {
    let (_dir, ctx, host) = setup(Some(auditor_routing()));
    ready(&host);
    let mut st = State::default();
    let new = vec![String::from("gate::a_fails")];
    start_audit(&ctx, &mut st, TIP, "test result: FAILED", &new, &[], None);
    dispatch_pending(&ctx, &mut st).unwrap();

    let dispatch = dispatch_command(&host);
    assert!(dispatch.contains("--pool apool"), "{dispatch}");
    assert!(dispatch.contains(&format!("--from {TIP}")), "{dispatch}");
    assert!(!dispatch.contains("--model"), "{dispatch}");
    assert!(!dispatch.contains("--read-only"), "{dispatch}");

    let run = current(&st).expect("no audit run stored");
    let job = st
        .jobs
        .get(run.job_id.as_deref().expect("no job ID"))
        .unwrap();
    assert_eq!(job.role, "auditor");
    assert_eq!(job.task.as_deref(), Some("1234abcd"));
    assert_eq!(job.from_ref.as_deref(), Some(TIP));
    assert_eq!(run.need, fix_need(&ctx, TIP));
    assert_eq!(job.pool.as_deref(), Some("apool"));
    let logs = host.logs();
    assert!(
        logs.iter().any(|line| line.contains("audit queued for")),
        "{logs:?}"
    );
}

#[test]
fn start_audit_falls_back_to_the_gate_style_worker() {
    let (_dir, ctx, host) = setup(None);
    ready(&host);
    let mut st = State::default();
    start_audit(
        &ctx,
        &mut st,
        TIP,
        "tail",
        &[String::from("gate::a_fails")],
        &[],
        None,
    );
    dispatch_pending(&ctx, &mut st).unwrap();

    let dispatch = dispatch_command(&host);
    assert!(dispatch.contains("--read-only"), "{dispatch}");
    assert!(dispatch.contains("--model gatemodel"), "{dispatch}");
    assert!(dispatch.contains("--pool apool"), "{dispatch}");
    let run = current(&st).expect("no audit run stored");
    let job = st
        .jobs
        .get(run.job_id.as_deref().expect("no job ID"))
        .unwrap();
    assert_eq!(job.task.as_deref(), Some("1234abcd"));
    assert_eq!(job.builtin_role.as_deref(), Some("auditor"));
}

#[test]
fn start_audit_skips_when_fix_units_exist() {
    let (_dir, ctx, host) = setup(None);
    ready(&host);
    let mut st = State::default();
    let unit = Unit {
        need: fix_need(&ctx, TIP),
        ..Unit::default()
    };
    st.units.insert(String::from("pumpfix-g01234567-1"), unit);
    start_audit(
        &ctx,
        &mut st,
        TIP,
        "tail",
        &[String::from("gate::a_fails")],
        &[],
        None,
    );

    let commands = host.commands();
    assert!(commands.is_empty(), "{commands:?}");
    assert!(current(&st).is_none());
}

#[test]
fn start_audit_owns_new_names_and_residuals() {
    let (_dir, ctx, host) = setup(None);
    ready(&host);
    let mut st = State::default();
    let new = vec![String::from("gate::a_fails")];
    let residual = vec![(
        String::from("crate::old"),
        String::from("pumpfix-gdeadbee0"),
    )];
    start_audit(&ctx, &mut st, TIP, "tail", &new, &residual, None);

    let owner = fix_need(&ctx, TIP);
    assert_eq!(st.red.get("gate::a_fails"), Some(&owner));
    assert_eq!(st.red.get("crate::old"), Some(&owner));
}
