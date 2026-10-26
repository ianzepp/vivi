//! Tests for [`crate::mind::probe`].

use tempfile::tempdir;

use super::*;

#[test]
fn health_probe_on_a_folder_without_a_mailspace_is_unknown_not_a_panic() {
    let dir = tempdir().unwrap();
    let signals = HealthProbe.probe(dir.path());
    assert_eq!(signals.root, dir.path());
    assert_eq!(signals.verdict, "unknown");
    assert!(!signals.reasons.is_empty());
    assert!(!signals.alarming());
}

#[test]
fn fingerprint_changes_with_any_signal() {
    let base = ProjectSignals {
        root: "/a".into(),
        verdict: "ok".into(),
        ..ProjectSignals::default()
    };
    let held = ProjectSignals {
        held_steps: 1,
        ..base.clone()
    };
    assert_ne!(base.fingerprint(), held.fingerprint());
    let reasons_only = ProjectSignals {
        reasons: vec!["noise".into()],
        ..base.clone()
    };
    assert_eq!(base.fingerprint(), reasons_only.fingerprint());
}

#[test]
fn signals_serialized_without_the_new_fields_still_load() {
    let legacy = r#"{"root":"/a","verdict":"ok","reasons":[],"locked_needs":1,"held_steps":0,
        "red_main":false,"repair_alarm":false,"envelope_failures":0,
        "last_envelope_failure_at":null,"stale_pending_jobs":0,"idle_steps":2,
        "deferring_jobs":0}"#;
    let signals: ProjectSignals = serde_json::from_str(legacy).unwrap();
    assert_eq!(signals.parked_needs, 0);
    assert!(signals.decisions.is_empty());
    assert_eq!(signals.locked_needs, 1);
    assert_eq!(signals.idle_steps, 2);
}

mod store_reads {
    use tempfile::tempdir;

    use super::super::*;
    use crate::pump::flow::decision::inventory;
    use crate::pump::flow::def::NodeClass;
    use crate::pump::flow::index::{RunRow, StepRow, StepState, run_state};
    use crate::pump::flow::tick::OPERATOR_HOLD;
    use crate::pump::jobs::{JobSpec, JobStatus, PoolJob};
    use crate::pump::store::PumpStore;
    use crate::pump::store::step_payload::StepPayloadRow;

    fn run(need: &str, kind: &str, entry: &str) -> RunRow {
        RunRow {
            need: need.into(),
            run: 1,
            flow: kind.into(),
            flow_version: 2,
            kind: kind.into(),
            entry: entry.into(),
            state: run_state::LIVE.into(),
        }
    }

    fn waiting(node: &str, task: &str) -> StepRow {
        StepRow {
            run: 1,
            node: node.into(),
            task: task.into(),
            state: StepState::Waiting,
            verdict: None,
        }
    }

    #[test]
    fn a_build_run_waiting_at_locked_counts_and_other_waits_do_not() {
        let dir = tempdir().unwrap();
        let store = PumpStore::open(&dir.path().join("pump.db")).unwrap();
        let locked = run("2025e1c1", "build", "plan");
        store.insert_run(&locked).unwrap();
        store
            .insert_run_step(&locked, &waiting("locked", "t1"))
            .unwrap();
        let busy = run("aaaa0001", "build", "plan");
        store.insert_run(&busy).unwrap();
        store
            .insert_run_step(&busy, &waiting("plan", "t2"))
            .unwrap();
        let mut signals = ProjectSignals::default();
        fold_decisions(&inventory(&store).unwrap(), &mut signals);
        assert_eq!(signals.locked_needs, 1);
        assert!(!signals.red_main);
        assert_eq!(signals.decisions.len(), 1);
        assert_eq!(signals.decisions[0].id, "lock 2025e1c1 r1 locked t1 a0");
        assert_eq!(signals.decisions[0].need, "2025e1c1");
        assert_eq!(signals.decisions[0].node, "locked");
        assert_eq!(signals.decisions[0].task, "t1");
        assert_eq!(signals.decisions[0].kind, "lock");
        assert!(signals.reasons[0].starts_with("lock 2025e1c1 r1 locked t1 a0: "));
        assert!(signals.alarming());
    }

    #[test]
    fn a_repair_run_waiting_at_unrepaired_is_a_red_main_alarm() {
        let dir = tempdir().unwrap();
        let store = PumpStore::open(&dir.path().join("pump.db")).unwrap();
        let repair = run("bbbb0002", "repair", "fix1");
        store.insert_run(&repair).unwrap();
        store
            .insert_run_step(&repair, &waiting("unrepaired", "t3"))
            .unwrap();
        let mut signals = ProjectSignals::default();
        fold_decisions(&inventory(&store).unwrap(), &mut signals);
        assert_eq!(signals.locked_needs, 0);
        assert_eq!(signals.parked_needs, 0);
        assert_eq!(signals.held_steps, 0);
        assert!(signals.red_main && signals.repair_alarm);
    }

    #[test]
    fn a_parked_run_counts_as_parked_not_locked() {
        let dir = tempdir().unwrap();
        let store = PumpStore::open(&dir.path().join("pump.db")).unwrap();
        let parked_run = run("aaaa000c", "build", "plan");
        store.insert_run(&parked_run).unwrap();
        store
            .insert_run_step(&parked_run, &waiting("parked", "t-park"))
            .unwrap();
        let mut signals = ProjectSignals::default();
        fold_decisions(&inventory(&store).unwrap(), &mut signals);
        assert_eq!(signals.parked_needs, 1);
        assert_eq!(signals.locked_needs, 0);
        assert_eq!(signals.held_steps, 0);
        assert_eq!(signals.decisions.len(), 1);
        assert_eq!(signals.decisions[0].kind, "park");
        assert_eq!(signals.decisions[0].id, "park aaaa000c r1 parked t-park a0");
        assert!(signals.alarming());
    }

    #[test]
    fn held_steps_count_operator_holds_including_slice_rows() {
        let dir = tempdir().unwrap();
        let store = PumpStore::open(&dir.path().join("pump.db")).unwrap();
        let held_run = run("bbbb000d", "build", "plan");
        store.insert_run(&held_run).unwrap();
        for (node, task) in [("slice:abc", "t-slice"), ("audit1", "t-hold")] {
            let held = StepRow {
                verdict: Some(OPERATOR_HOLD.into()),
                ..waiting(node, task)
            };
            store.insert_run_step(&held_run, &held).unwrap();
        }
        let mut signals = ProjectSignals::default();
        fold_decisions(&inventory(&store).unwrap(), &mut signals);
        assert_eq!(signals.held_steps, 2);
        assert_eq!(signals.locked_needs, 0);
        assert_eq!(signals.parked_needs, 0);
        assert!(signals.decisions.iter().all(|d| d.kind == "hold"));
        assert!(signals.alarming());
    }

    #[test]
    fn a_hold_without_a_raised_issue_states_the_absence() {
        let dir = tempdir().unwrap();
        let store = PumpStore::open(&dir.path().join("pump.db")).unwrap();
        let held_run = run("cccc000e", "build", "plan");
        store.insert_run(&held_run).unwrap();
        let held = StepRow {
            verdict: Some(OPERATOR_HOLD.into()),
            ..waiting("slice:abc", "t-quiet")
        };
        store.insert_run_step(&held_run, &held).unwrap();
        let mut signals = ProjectSignals::default();
        fold_decisions(&inventory(&store).unwrap(), &mut signals);
        let detail = &signals.decisions[0];
        assert!(
            detail
                .reason
                .contains("no issue was raised about task t-quiet")
        );
        assert!(
            detail
                .evidence
                .iter()
                .any(|line| line.contains("no raised issue found"))
        );
        assert!(signals.reasons[0].contains("no issue was raised"));
    }

    #[test]
    fn replacing_a_locked_need_at_equal_count_changes_the_fingerprint() {
        let dir = tempdir().unwrap();
        let store = PumpStore::open(&dir.path().join("pump.db")).unwrap();
        let first = run("dddd000f", "build", "plan");
        store.insert_run(&first).unwrap();
        store
            .insert_run_step(&first, &waiting("locked", "t-a"))
            .unwrap();
        let mut signals = ProjectSignals::default();
        fold_decisions(&inventory(&store).unwrap(), &mut signals);
        assert_eq!(signals.locked_needs, 1);
        let before = signals.fingerprint();

        // The need unsticks and another locks in its place: same count.
        store.set_run_state("dddd000f", 1, run_state::DONE).unwrap();
        let second = run("eeee0010", "build", "plan");
        store.insert_run(&second).unwrap();
        store
            .insert_run_step(&second, &waiting("locked", "t-b"))
            .unwrap();
        fold_decisions(&inventory(&store).unwrap(), &mut signals);
        assert_eq!(signals.locked_needs, 1);
        assert_eq!(signals.decisions.len(), 1);
        assert_eq!(signals.decisions[0].id, "lock eeee0010 r1 locked t-b a0");
        assert_ne!(before, signals.fingerprint());
    }

    #[test]
    fn changing_only_a_wait_reason_keeps_the_fingerprint() {
        let dir = tempdir().unwrap();
        let store = PumpStore::open(&dir.path().join("pump.db")).unwrap();
        let locked = run("ffff0011", "build", "plan");
        store.insert_run(&locked).unwrap();
        store
            .insert_run_step(&locked, &waiting("locked", "t-cto"))
            .unwrap();
        let mut signals = ProjectSignals::default();
        fold_decisions(&inventory(&store).unwrap(), &mut signals);
        assert!(signals.reasons[0].contains("reached without a CTO answer"));
        let before = signals.fingerprint();

        store
            .record_step_payload(&StepPayloadRow {
                need: "ffff0011".into(),
                run: 1,
                node: "cto".into(),
                class: NodeClass::Cto.name().into(),
                task: "t-cto".into(),
                payload: serde_json::json!({
                    "front": {"answer": "slice", "reason": "waiting on the operator"}
                }),
            })
            .unwrap();
        fold_decisions(&inventory(&store).unwrap(), &mut signals);
        assert_eq!(signals.locked_needs, 1);
        assert_eq!(signals.reasons.len(), 2);
        assert!(signals.reasons[1].contains("the CTO answered `slice`"));
        assert_eq!(before, signals.fingerprint());
    }

    #[test]
    fn waits_that_disappear_clear_their_counts_and_details() {
        let dir = tempdir().unwrap();
        let store = PumpStore::open(&dir.path().join("pump.db")).unwrap();
        let repair = run("aaaa0012", "repair", "fix1");
        store.insert_run(&repair).unwrap();
        store
            .insert_run_step(&repair, &waiting("unrepaired", "t-fix"))
            .unwrap();
        let mut signals = ProjectSignals::default();
        fold_decisions(&inventory(&store).unwrap(), &mut signals);
        assert!(signals.red_main && signals.repair_alarm);
        assert_eq!(signals.decisions.len(), 1);

        fold_decisions(&[], &mut signals);
        assert_eq!(signals.locked_needs, 0);
        assert_eq!(signals.parked_needs, 0);
        assert_eq!(signals.held_steps, 0);
        assert!(!signals.red_main);
        assert!(!signals.repair_alarm);
        assert!(signals.decisions.is_empty());
        assert!(!signals.alarming());
    }

    #[test]
    fn only_recent_envelope_failures_count_and_the_newest_is_kept() {
        let rows = vec![
            (100, ENVELOPE_FAILED.to_string(), "u".into(), "old".into()),
            (9_000, ENVELOPE_FAILED.to_string(), "u".into(), "new".into()),
            (9_500, "gate".to_string(), "u".into(), "other".into()),
            (
                9_400,
                ENVELOPE_FAILED.to_string(),
                "u".into(),
                "newer".into(),
            ),
        ];
        let mut signals = ProjectSignals::default();
        fold_exceptions(&rows, 10_000, &mut signals);
        assert_eq!(signals.envelope_failures, 2);
        assert_eq!(signals.last_envelope_failure_at, Some(9_400));
    }

    #[test]
    fn only_old_pending_jobs_are_stale() {
        let job = |id: &str, queued_at: u64, status: JobStatus| {
            let mut job = PoolJob::from(JobSpec::new(id, "plan", "pump-planner", "s", "b"));
            job.queued_at = queued_at;
            job.status = status;
            job
        };
        let now = 10_000;
        let jobs = [
            job("plan-old", now - STALE_PENDING_SECS, JobStatus::Pending),
            job("plan-new", now - 60, JobStatus::Pending),
            job("plan-out", 0, JobStatus::Dispatched),
        ];
        let mut signals = ProjectSignals::default();
        fold_jobs(jobs.iter(), now, &mut signals);
        assert_eq!(signals.stale_pending_jobs, 1);
        assert_eq!(signals.reasons.len(), 1);
        assert!(signals.reasons[0].starts_with("job plan-old pending"));
    }

    #[test]
    fn an_envelope_failure_ageing_out_keeps_the_fingerprint() {
        let base = ProjectSignals {
            envelope_failures: 3,
            last_envelope_failure_at: Some(42),
            ..ProjectSignals::default()
        };
        let fewer = ProjectSignals {
            envelope_failures: 1,
            ..base.clone()
        };
        assert_eq!(base.fingerprint(), fewer.fingerprint());
    }

    fn pool_job(id: &str, queued_at: u64, status: JobStatus) -> PoolJob {
        let mut job = PoolJob::from(JobSpec::new(id, "step", "hand", "s", "b"));
        job.queued_at = queued_at;
        job.status = status;
        job
    }

    #[test]
    fn a_running_step_whose_job_never_dispatches_goes_idle() {
        let dir = tempdir().unwrap();
        let store = PumpStore::open(&dir.path().join("pump.db")).unwrap();
        let live = run("cccc0003", "build", "plan");
        store.insert_run(&live).unwrap();
        for (node, task, job) in [("hand", "t-idle", "j-idle"), ("audit", "t-busy", "j-busy")] {
            let step = StepRow {
                state: StepState::Running,
                ..waiting(node, task)
            };
            store.insert_run_step(&live, &step).unwrap();
            let row = crate::pump::flow::index::StepJobRow {
                job: job.into(),
                run: 1,
                node: node.into(),
                task: task.into(),
                attempt: 1,
            };
            store.insert_step_job("cccc0003", &row).unwrap();
        }
        let now = 10_000;
        let idle = pool_job("j-idle", now - IDLE_STEP_SECS, JobStatus::Pending);
        let busy = pool_job("j-busy", now - 9_000, JobStatus::Dispatched);
        let mut signals = ProjectSignals::default();
        read_idle_steps(&store, &[&idle, &busy], now, &mut signals).unwrap();
        assert_eq!(signals.idle_steps, 1);
        assert!(signals.reasons[0].contains("step `hand` running with no dispatched job"));
        assert!(signals.alarming());

        let mut early = ProjectSignals::default();
        let young = pool_job("j-idle", now - 30, JobStatus::Pending);
        read_idle_steps(&store, &[&young], now, &mut early).unwrap();
        assert_eq!(early.idle_steps, 0);
    }

    #[test]
    fn repeated_deferrals_of_a_pending_job_signal_and_cleared_ones_do_not() {
        let log = "\
20:28:30 job dispatched other-a1 role=hand pool=pi task=x
20:29:00 job dispatch deferred step-1-a1 on pi: pools.pi.harness.models holds an empty slug
20:29:05 job dispatch deferred step-1-a1 on pi: pools.pi.harness.models holds an empty slug
20:29:10 job dispatch deferred step-1-a1 on pi: pools.pi.harness.models holds an empty slug
20:29:10 job dispatch deferred gone-a1 on pi: boom
20:29:15 job dispatch deferred gone-a1 on pi: boom
20:29:20 job dispatch deferred gone-a1 on pi: boom
20:29:20 job dispatch deferred step-2-a1 on local: once
";
        let pending: BTreeSet<&str> = ["step-1-a1", "step-2-a1"].into_iter().collect();
        let mut signals = ProjectSignals::default();
        fold_deferrals(log, &pending, &mut signals);
        assert_eq!(signals.deferring_jobs, 1);
        assert_eq!(
            signals.reasons,
            vec![
                "job step-1-a1 dispatch deferred 3 times on pi: pools.pi.harness.models holds an empty slug"
            ]
        );
    }

    #[test]
    fn the_log_tail_starts_at_a_whole_line_and_a_missing_log_is_empty() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("log.txt");
        assert_eq!(log_tail(&path).unwrap(), "");
        let line = "12:00:00 job dispatched x role=hand pool=pi task=t\n";
        std::fs::write(&path, line.repeat(4_000)).unwrap();
        let tail = log_tail(&path).unwrap();
        assert!(tail.len() <= 64 * 1024);
        assert!(tail.starts_with("12:00:00 "));
        assert!(tail.lines().all(|got| got == line.trim_end()));
    }

    #[test]
    fn a_tick_step_failing_repeatedly_and_recently_signals() {
        let failing = "10:00:00 tick step dispatch-jobs failed: pools.pi.harness.models holds an empty slug\n";
        let quiet = "10:00:05 collect took 0s\n";
        let mut signals = ProjectSignals::default();
        fold_deferrals(&failing.repeat(3), &BTreeSet::new(), &mut signals);
        assert_eq!(signals.deferring_jobs, 1);
        assert!(signals.reasons[0].starts_with("tick step dispatch-jobs failed 3 times: "));

        let fixed = format!("{}{}", failing.repeat(3), quiet.repeat(RECENT_LINES));
        let mut after = ProjectSignals::default();
        fold_deferrals(&fixed, &BTreeSet::new(), &mut after);
        assert_eq!(after.deferring_jobs, 0);
    }
}
