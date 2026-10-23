//! Tests for Mind oversight: allowlist, records, and a non-blocking tick.

use crate::mailspace::RoleView;
use crate::pump::config::PumpConfig;
use crate::pump::host::test_ctx;
use crate::pump::jobs::{JobOutcome, JobSpec, JobStatus, PoolJob};
use crate::pump::oversight::{self, RESULT_MARKER, job_parts};
use crate::pump::state::{NeedRecord, State, Status, Unit};

fn role() -> RoleView {
    RoleView {
        name: String::from("mind"),
        address: String::from("mind@test.local"),
        aliases: Vec::new(),
        kind: Some(String::from("mind")),
        labels: Vec::new(),
        status: String::from("active"),
        harness: None,
        provider: None,
        model: None,
        thinking: None,
        pid: None,
        host: None,
        cadence: Some(String::from("1h")),
        pool: Some(String::from("p")),
        read_only: Some(true),
        models: Vec::new(),
        charter: String::from("steer"),
        has_charter: true,
    }
}

fn job_with(receipt: &str) -> PoolJob {
    let mut spec = JobSpec::new("cadence-mind-1", "cadence", "mind", "oversight", "brief");
    spec.read_only = true;
    spec.payload = serde_json::json!({ "oversight": true });
    let mut job = PoolJob::from(spec);
    job.status = JobStatus::Finished;
    job.outcome = Some(JobOutcome {
        state: String::from("finished"),
        detail: String::from("agent exit 0"),
        receipt: receipt.to_owned(),
        at: 1_000_000,
    });
    job
}

fn result_line(actions: &str) -> String {
    format!("agent exit 0 in 1s\n{RESULT_MARKER}{{\"version\":1,\"actions\":{actions}}}\n")
}

#[test]
fn the_mind_brief_carries_compact_pump_state_and_the_allowlist() {
    let mut st = State::default();
    st.units.insert(
        String::from("u1"),
        Unit {
            status: Status::Pending,
            ..Unit::default()
        },
    );
    let (kind, _, body) = job_parts(&st, &role(), 1_000);
    assert_eq!(kind, "cadence");
    assert!(body.contains("PUMP-STATE-BEGIN"));
    assert!(body.contains("units pending=1"));
    assert!(body.contains("Allowlist: replan-need, note."));
    assert!(!body.contains("hold-merge"));
    assert!(body.contains(RESULT_MARKER));
}

#[test]
fn allowlisted_actions_apply_and_unknown_ops_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let mut st = State::default();
    st.needs.insert(
        String::from("n1"),
        NeedRecord {
            verdict: String::from("units"),
            ..NeedRecord::default()
        },
    );
    st.jobs.insert(
        String::from("cadence-mind-1"),
        job_with(&result_line(
            r#"[{"op":"hold-merge"},{"op":"skip-unit","id":"u1"},{"op":"replan-need","id":"n1"},{"op":"note","text":"look"},{"op":"explode","id":"nope"}]"#,
        )),
    );
    oversight::apply(&ctx, &mut st, "cadence-mind-1");
    let flag_files: Vec<String> = std::fs::read_dir(&ctx.home)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_lowercase())
        .filter(|name| name.contains("hold") || name.contains("skip"))
        .collect();
    assert!(flag_files.is_empty(), "retired ops wrote {flag_files:?}");
    assert!(st.needs["n1"].amendment_pending);
    let log = oversight::state(&st);
    assert_eq!(log.interventions.len(), 5);
    assert!(
        log.interventions
            .iter()
            .filter(|row| row.result == "applied")
            .count()
            == 2
    );
    for retired in ["hold-merge", "skip-unit"] {
        let row = log
            .interventions
            .iter()
            .find(|row| row.op == retired)
            .unwrap();
        assert!(row.result.contains("not on the allowlist"), "{retired}");
    }
    let refused = log
        .interventions
        .iter()
        .find(|row| row.op == "explode")
        .unwrap();
    assert!(refused.result.contains("not on the allowlist"));
}

#[test]
fn a_missing_or_malformed_result_does_not_block_or_change_state() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let mut st = State::default();
    st.jobs.insert(
        String::from("cadence-mind-1"),
        job_with("cadence survived\n"),
    );
    oversight::apply(&ctx, &mut st, "cadence-mind-1");
    oversight::apply(&ctx, &mut st, "missing-job");
    st.jobs.insert(
        String::from("cadence-mind-2"),
        job_with(&format!("{RESULT_MARKER}{{nope}}\n")),
    );
    oversight::apply(&ctx, &mut st, "cadence-mind-2");
    assert!(oversight::state(&st).interventions.is_empty());
}
