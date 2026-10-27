//! Tests of the live admission seam: evidence folded from a real mailspace
//! event log through the fake provider's census, guard contention and
//! release, and the failed-census refusal.

use std::collections::BTreeMap;

use super::*;
use crate::error::VivariumError;
use crate::mailspace::Mailspace;
use crate::mailspace::SendRequest;
use crate::pool::doctor::Check;
use crate::pool::fake_provider::{Fake, claimed, pid, slot};
use crate::pool::provider::{CreateSpec, ExecOutput, Hold, MachineRecord, PoolProvider};
use crate::pool::record::{self, DispatchRecord};

/// The instant every snapshot is taken at.
const NOW: u64 = 1_700_000_100;
/// The unexpired cooldown deadline the shared evidence records.
const UNTIL: u64 = 1_700_000_600;

/// A mailspace with one task, and the task's display handle.
fn space_with_task() -> (tempfile::TempDir, Mailspace, String) {
    let tmp = tempfile::tempdir().unwrap();
    let mut space = Mailspace::init(Some(tmp.path())).unwrap();
    space.add_identity("mind").unwrap();
    space.add_identity("hand").unwrap();
    let sent = space
        .send(SendRequest {
            from: "mind".into(),
            to: vec!["hand".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "Do it".into(),
            body: crate::letter::fixture::task_body("do it\n"),
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
        })
        .unwrap();
    let handle = sent.delivered[0].handle.clone();
    (tmp, space, handle)
}

/// The dispatch record an open occupancy of pool `p` on `machine` carries.
fn open_record(machine: &str) -> DispatchRecord {
    DispatchRecord {
        target: None,
        repository: None,
        pool: "p".into(),
        need: None,
        kind: None,
        slot: "p-slot-1".into(),
        machine: machine.into(),
        job: "j1".into(),
        model: "zai/glm-5.3-flash".into(),
        from: "main".into(),
        branch: "factory/a".into(),
        start: "f".repeat(40),
        with: Vec::new(),
        budget: 600,
        at: 1_700_000_000,
        read_only: false,
        hardware: String::new(),
        abandoned: 0,
        abandoned_cost_micros: 0,
    }
}

/// Record `machine`'s dispatch of pool `p` on `task`: an open occupancy.
fn record_dispatch(space: &Mailspace, task: &str, machine: &str) {
    space
        .record_pool_note(
            task,
            "pool_dispatched",
            &record::dispatch_note(&open_record(machine)),
            "dispatched",
        )
        .unwrap();
}

/// Record a collection whose note carries typed cooldown evidence for the
/// `zai` provider until [`UNTIL`], as a real collection writes it.
fn record_cooldown(space: &Mailspace, task: &str) {
    let note = format!(
        r#"{{"job":"j1","pool":"p","outcome":"failed","band_cooldown":{{"provider":"zai","until":{UNTIL},"class":"rate-limit"}}}}"#
    );
    space
        .record_pool_note(task, "pool_collected", &note, "collected")
        .unwrap();
}

/// The admission `acquire` grants, unwrapped for reading.
fn ready(decision: Decision<Admission>) -> Admission {
    match decision {
        Decision::Ready(admission) => admission,
        Decision::Waiting(wait) => {
            panic!(
                "expected an admission, got waiting as {}",
                wait.reason.word()
            )
        }
    }
}

/// The wait `acquire` answers, asserted to carry `reason` and no deadline.
fn waiting(decision: Decision<Admission>, reason: WaitReason) {
    match decision {
        Decision::Waiting(wait) => {
            assert_eq!(wait.reason, reason);
            assert_eq!(wait.retry_at, None);
        }
        Decision::Ready(_) => {
            panic!("expected waiting as {}, got an admission", reason.word())
        }
    }
}

/// A provider whose census always fails: the host cannot be reached.
struct Failing;

impl PoolProvider for Failing {
    fn ensure_home(&self) -> Result<Option<String>, VivariumError> {
        Ok(None)
    }

    fn doctor(&self, _harness_env: &[String]) -> Vec<Check> {
        Vec::new()
    }

    fn harness_env_gaps(&self, names: &[String]) -> Result<Vec<String>, VivariumError> {
        Ok(names.to_vec())
    }

    fn env_gaps_at_dispatch(&self, names: &[String]) -> Result<Vec<String>, VivariumError> {
        Ok(names.to_vec())
    }

    fn env_gap_hint(&self, _missing: &[String]) -> String {
        String::new()
    }

    fn live_test_notice(&self) -> String {
        String::new()
    }

    fn supplies_build_mount(&self) -> bool {
        false
    }

    fn build_persists(&self) -> bool {
        false
    }

    fn runs_unprivileged(&self) -> bool {
        true
    }

    fn list(&self) -> Result<Vec<MachineRecord>, VivariumError> {
        Err(VivariumError::Message("the host cannot be listed".into()))
    }

    fn create(&self, _spec: &CreateSpec) -> Result<String, VivariumError> {
        Err(VivariumError::Message("unused".into()))
    }

    fn destroy(&self, _id: &str) -> Result<(), VivariumError> {
        Err(VivariumError::Message("unused".into()))
    }

    fn destroy_command(&self, _id: &str) -> String {
        String::new()
    }

    fn machine(&self, _id: &str) -> Result<MachineRecord, VivariumError> {
        Err(VivariumError::Message("unused".into()))
    }

    fn claim(&self, _id: &str, _task: &str, _now: u64) -> Result<Option<Hold>, VivariumError> {
        Err(VivariumError::Message("unused".into()))
    }

    fn unclaim(&self, _id: &str) -> Result<(), VivariumError> {
        Err(VivariumError::Message("unused".into()))
    }

    fn release(&self, _id: &str, _hold: &Hold) -> Result<(), VivariumError> {
        Err(VivariumError::Message("unused".into()))
    }

    fn put_file(
        &self,
        _id: &str,
        _hold: &Hold,
        _path: &str,
        _content: &[u8],
    ) -> Result<(), VivariumError> {
        Err(VivariumError::Message("unused".into()))
    }

    fn seal(
        &self,
        _id: &str,
        _drop_env: &[&str],
        _key: &str,
        _value: &str,
    ) -> Result<(), VivariumError> {
        Err(VivariumError::Message("unused".into()))
    }

    fn start(&self, _id: &str) -> Result<(), VivariumError> {
        Err(VivariumError::Message("unused".into()))
    }

    fn stop(&self, _id: &str) -> Result<(), VivariumError> {
        Err(VivariumError::Message("unused".into()))
    }

    fn exec(
        &self,
        _id: &str,
        _script: &str,
        _timeout_secs: u32,
    ) -> Result<ExecOutput, VivariumError> {
        Err(VivariumError::Message("unused".into()))
    }

    fn read_stopped(
        &self,
        _id: &str,
        _paths: &[&str],
    ) -> Result<Option<BTreeMap<String, Vec<u8>>>, VivariumError> {
        Err(VivariumError::Message("unused".into()))
    }
}

#[test]
fn an_open_record_is_the_pool_occupancy_the_snapshot_reports() {
    let (_tmp, space, task) = space_with_task();
    record_dispatch(&space, &task, "m1");
    let fake = Fake::default();
    fake.add(slot("m1", "p-slot-1", "started", "p", 1));
    let admission = ready(acquire(&space, &fake, &pid("p"), NOW).unwrap());
    assert_eq!(
        admission.availability.model_counts.get("zai/glm-5.3-flash"),
        Some(&1)
    );
    assert_eq!(admission.availability.unresolved_claims, 0);
    assert!(admission.availability.provider_until.is_empty());
    assert_eq!(admission.availability.now, NOW);
}

#[test]
fn a_claimed_slot_without_a_record_is_an_unresolved_occupancy() {
    let (_tmp, space, task) = space_with_task();
    let fake = Fake::default();
    fake.add(claimed("m1", "p-slot-1", "started", "p", &task));
    let admission = ready(acquire(&space, &fake, &pid("p"), NOW).unwrap());
    assert!(admission.availability.model_counts.is_empty());
    assert_eq!(admission.availability.unresolved_claims, 1);
}

#[test]
fn cooldown_evidence_recorded_under_one_pool_cools_the_provider_mailspaces_wide() {
    let (_tmp, space, task) = space_with_task();
    record_dispatch(&space, &task, "m1");
    record_cooldown(&space, &task);
    // The collection closed the dispatch: the census is quiet, but the
    // provider the failure cooled still answers with its deadline.
    let fake = Fake::default();
    fake.add(slot("m1", "p-slot-1", "started", "p", 1));
    let admission = ready(acquire(&space, &fake, &pid("p"), NOW).unwrap());
    let mut deadlines = BTreeMap::new();
    deadlines.insert("zai".to_string(), UNTIL);
    assert_eq!(admission.availability.provider_until, deadlines);
    assert!(admission.availability.model_counts.is_empty());
}

#[test]
fn the_guard_stays_held_after_acquire_and_releases_when_dropped() {
    let (_tmp, space, _task) = space_with_task();
    let fake = Fake::default();
    let first = ready(acquire(&space, &fake, &pid("p"), NOW).unwrap());
    // The returned admission still owns the guard: a second dispatch of
    // the pool waits for it.
    let second = acquire(&space, &fake, &pid("p"), NOW).unwrap();
    waiting(second, WaitReason::DispatchBusy);
    drop(first);
    let third = ready(acquire(&space, &fake, &pid("p"), NOW).unwrap());
    assert!(third.availability.model_counts.is_empty());
}

#[test]
fn a_failed_census_waits_and_releases_the_guard_instead_of_granting() {
    let (_tmp, space, _task) = space_with_task();
    let decision = acquire(&space, &Failing, &pid("p"), NOW).unwrap();
    waiting(decision, WaitReason::BandUnavailable);
    // The refused attempt did not keep the guard: a working census of the
    // same pool can be admitted right away.
    let fake = Fake::default();
    ready(acquire(&space, &fake, &pid("p"), NOW).unwrap());
}
