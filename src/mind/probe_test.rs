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

mod store_reads {
    use tempfile::tempdir;

    use super::super::*;
    use crate::pump::flow::index::{RunRow, StepRow, StepState, run_state};
    use crate::pump::jobs::{JobSpec, JobStatus, PoolJob};
    use crate::pump::store::PumpStore;

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
        read_lock_waits(&store, &mut signals).unwrap();
        assert_eq!(signals.locked_needs, 1);
        assert!(!signals.red_main);
        assert_eq!(
            signals.reasons,
            vec!["need 2025e1c1 r1 waits at `locked` (build flow)"]
        );
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
        read_lock_waits(&store, &mut signals).unwrap();
        assert_eq!(signals.locked_needs, 0);
        assert!(signals.red_main && signals.repair_alarm);
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
}
