use std::collections::BTreeMap;
use std::time::Duration;

use super::super::events::ProjectEvents;
use super::super::model::RowState;
use super::rows;
use crate::pool::record::{DispatchRecord, OpenDispatch};

#[test]
fn claimed_stopped_local_slot_is_retained_as_uncollected_with_a_tail() {
    let temp = tempfile::tempdir().unwrap();
    let slot = temp.path().join("pool/lanes/slot-1");
    std::fs::create_dir_all(slot.join("marks")).unwrap();
    for (name, value) in [
        ("vivi_pool", "lanes"),
        ("vivi_pool_mailspace", "alpha"),
        ("vivi_pool_slot", "1"),
        ("vivi_pool_state", "ready"),
    ] {
        std::fs::write(slot.join("marks").join(name), format!("{value}\n")).unwrap();
    }
    std::fs::write(slot.join("claim"), "task=hand-12345678\nclaimed=1000\n").unwrap();
    std::fs::create_dir_all(slot.join("job")).unwrap();
    std::fs::write(slot.join("job/agent.out"), "receipt\n").unwrap();
    let pools: BTreeMap<String, crate::pool::config::PoolConfig> =
        toml::from_str("[lanes]\nprovider='local'\nslots=2\n[lanes.local]\nroot='pool'\n").unwrap();
    let (found, warnings) = rows(
        "alpha",
        temp.path(),
        "",
        &pools,
        &ProjectEvents::default(),
        1060,
    );
    assert!(warnings.is_empty());
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].task.as_deref(), Some("hand-12345678"));
    assert_eq!(found[0].state, RowState::Finished("uncollected".into()));
    assert_eq!(found[0].running_secs, Some(60));
    assert_eq!(found[0].finished_at, None);
    assert!(
        found[0]
            .tail
            .as_ref()
            .is_some_and(|path| path.ends_with("job/agent.out"))
    );
}

#[test]
fn shared_local_slot_path_uses_the_raw_mailspace_uuid() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("shared");
    let mailspace_id = "1a2b3c4d-55ee-4f99-8444-123456789abc";
    let slot = root.join(mailspace_id).join("lanes/slot-1");
    std::fs::create_dir_all(slot.join("marks")).unwrap();
    for (name, value) in [
        ("vivi_pool", "lanes"),
        ("vivi_pool_mailspace", mailspace_id),
        ("vivi_pool_slot", "1"),
        ("vivi_pool_state", "ready"),
    ] {
        std::fs::write(slot.join("marks").join(name), format!("{value}\n")).unwrap();
    }
    std::fs::write(slot.join("claim"), "task=hand-12345678\nclaimed=1000\n").unwrap();
    std::fs::create_dir_all(slot.join("job")).unwrap();
    std::fs::write(slot.join("job/agent.out"), "receipt\n").unwrap();
    let pools: BTreeMap<String, crate::pool::config::PoolConfig> = toml::from_str(&format!(
        "[lanes]\nprovider='local'\nslots=2\n[lanes.local]\nroot={:?}\n",
        root.to_string_lossy()
    ))
    .unwrap();
    let (found, warnings) = rows(
        "alpha",
        temp.path(),
        mailspace_id,
        &pools,
        &ProjectEvents::default(),
        1060,
    );
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].task.as_deref(), Some("hand-12345678"));
    assert_eq!(found[0].finished_at, None);
    assert!(
        found[0]
            .tail
            .as_ref()
            .is_some_and(|path| path.starts_with(&root))
    );
    assert!(
        found[0]
            .tail
            .as_ref()
            .is_some_and(|path| path.ends_with("job/agent.out"))
    );
}

#[test]
fn free_slots_do_not_create_monitor_rows() {
    let temp = tempfile::tempdir().unwrap();
    let slot = temp.path().join("pool/lanes/slot-1");
    std::fs::create_dir_all(slot.join("marks")).unwrap();
    for (name, value) in [
        ("vivi_pool", "lanes"),
        ("vivi_pool_mailspace", "alpha"),
        ("vivi_pool_slot", "1"),
        ("vivi_pool_state", "ready"),
    ] {
        std::fs::write(slot.join("marks").join(name), format!("{value}\n")).unwrap();
    }
    let pools: BTreeMap<String, crate::pool::config::PoolConfig> =
        toml::from_str("[lanes]\nprovider='local'\nslots=2\n[lanes.local]\nroot='pool'\n").unwrap();
    let (found, _) = rows(
        "alpha",
        temp.path(),
        "",
        &pools,
        &ProjectEvents::default(),
        1000,
    );
    assert!(found.is_empty());
}

/// A stopped, claimed local slot under the relative pool root, with an empty
/// job directory; the result artifact, if any, is written by the caller.
fn stopped_slot(temp: &tempfile::TempDir) -> std::path::PathBuf {
    let slot = temp.path().join("pool/lanes/slot-1");
    std::fs::create_dir_all(slot.join("marks")).unwrap();
    for (name, value) in [
        ("vivi_pool", "lanes"),
        ("vivi_pool_mailspace", "alpha"),
        ("vivi_pool_slot", "1"),
        ("vivi_pool_state", "ready"),
    ] {
        std::fs::write(slot.join("marks").join(name), format!("{value}\n")).unwrap();
    }
    std::fs::write(slot.join("claim"), "task=hand-12345678\nclaimed=1000\n").unwrap();
    std::fs::create_dir_all(slot.join("job")).unwrap();
    slot
}

fn pools_lanes() -> BTreeMap<String, crate::pool::config::PoolConfig> {
    toml::from_str("[lanes]\nprovider='local'\nslots=2\n[lanes.local]\nroot='pool'\n").unwrap()
}

/// An open dispatch for the fixture slot, started at epoch 1000.
fn dispatch(job: &str) -> DispatchRecord {
    DispatchRecord {
        target: None,
        repository: None,
        pool: "lanes".into(),
        need: None,
        kind: None,
        slot: "lanes-slot-1".into(),
        machine: "lanes/slot-1".into(),
        job: job.into(),
        model: "model-x".into(),
        from: "main".into(),
        branch: "agent/work".into(),
        start: "main".into(),
        with: Vec::new(),
        budget: 600,
        at: 1000,
        read_only: false,
        hardware: String::new(),
        abandoned: 0,
        abandoned_cost_micros: 0,
    }
}

fn open_dispatch(record: DispatchRecord) -> ProjectEvents {
    ProjectEvents {
        open: vec![OpenDispatch {
            task: "abcd1234".into(),
            record,
        }],
        ..ProjectEvents::default()
    }
}

/// Set a file's modification time without sleeping.
fn backdate(path: &std::path::Path, epoch: u64) {
    let file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    file.set_times(
        std::fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH + Duration::from_secs(epoch)),
    )
    .unwrap();
}

#[test]
fn a_matched_current_attempt_result_finishes_the_row_at_the_artifact_time() {
    let temp = tempfile::tempdir().unwrap();
    let slot = stopped_slot(&temp);
    std::fs::write(
        slot.join("job/result"),
        "job=job-1\ntarget=alpha\nrepository=vivi\nstate=done\npush=pushed\n",
    )
    .unwrap();
    backdate(&slot.join("job/result"), 1100);
    std::fs::write(slot.join("job/agent.out"), "receipt\n").unwrap();
    let mut record = dispatch("job-1");
    record.target = Some("alpha".into());
    record.repository = Some("vivi".into());
    let (found, warnings) = rows(
        "alpha",
        temp.path(),
        "",
        &pools_lanes(),
        &open_dispatch(record),
        2000,
    );
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].state, RowState::Finished("uncollected".into()));
    assert_eq!(found[0].started_at, Some(1000));
    assert_eq!(found[0].finished_at, Some(1100));
    // The completed duration stops at the artifact time, not at `now`.
    assert_eq!(found[0].running_secs, Some(100));
    assert_eq!(found[0].task.as_deref(), Some("abcd1234"));
    assert!(found[0].tail.is_some());
}

#[test]
fn a_result_with_a_wrong_job_identity_is_not_a_completion() {
    let temp = tempfile::tempdir().unwrap();
    let slot = stopped_slot(&temp);
    std::fs::write(slot.join("job/result"), "job=other\nstate=done\n").unwrap();
    backdate(&slot.join("job/result"), 1100);
    let (found, _) = rows(
        "alpha",
        temp.path(),
        "",
        &pools_lanes(),
        &open_dispatch(dispatch("job-1")),
        2000,
    );
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].finished_at, None);
    assert_eq!(found[0].running_secs, Some(1000));
}

#[test]
fn a_result_from_a_foreign_repository_is_not_a_completion() {
    let temp = tempfile::tempdir().unwrap();
    let slot = stopped_slot(&temp);
    std::fs::write(
        slot.join("job/result"),
        "job=job-1\nrepository=other\nstate=done\n",
    )
    .unwrap();
    backdate(&slot.join("job/result"), 1100);
    let mut record = dispatch("job-1");
    record.repository = Some("vivi".into());
    let (found, _) = rows(
        "alpha",
        temp.path(),
        "",
        &pools_lanes(),
        &open_dispatch(record),
        2000,
    );
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].finished_at, None);
}

#[test]
fn a_result_older_than_the_current_attempt_is_not_a_completion() {
    let temp = tempfile::tempdir().unwrap();
    let slot = stopped_slot(&temp);
    std::fs::write(slot.join("job/result"), "job=job-1\nstate=done\n").unwrap();
    // Written before the current dispatch (at 1000): a previous attempt.
    backdate(&slot.join("job/result"), 900);
    let (found, _) = rows(
        "alpha",
        temp.path(),
        "",
        &pools_lanes(),
        &open_dispatch(dispatch("job-1")),
        2000,
    );
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].finished_at, None);
    assert_eq!(found[0].running_secs, Some(1000));
}

#[test]
fn a_missing_result_leaves_the_finish_unknown() {
    let temp = tempfile::tempdir().unwrap();
    let slot = stopped_slot(&temp);
    std::fs::write(slot.join("job/agent.out"), "receipt\n").unwrap();
    let (found, _) = rows(
        "alpha",
        temp.path(),
        "",
        &pools_lanes(),
        &open_dispatch(dispatch("job-1")),
        2000,
    );
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].finished_at, None);
    assert_eq!(found[0].running_secs, Some(1000));
}

#[test]
fn a_running_slot_never_reads_a_completion_artifact() {
    let temp = tempfile::tempdir().unwrap();
    let slot = stopped_slot(&temp);
    let me = std::process::id();
    std::fs::write(slot.join("boot.pid"), format!("{me}\n")).unwrap();
    std::fs::write(
        slot.join("boot.start"),
        format!("{}\n", crate::pool::local_proc::identity(me).unwrap()),
    )
    .unwrap();
    std::fs::write(slot.join("job/result"), "job=job-1\nstate=done\n").unwrap();
    backdate(&slot.join("job/result"), 1100);
    let (found, _) = rows(
        "alpha",
        temp.path(),
        "",
        &pools_lanes(),
        &open_dispatch(dispatch("job-1")),
        2000,
    );
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].state, RowState::Running);
    assert_eq!(found[0].finished_at, None);
    // Still elapsed time to now, not a completed duration.
    assert_eq!(found[0].running_secs, Some(1000));
}
