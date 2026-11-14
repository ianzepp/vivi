use super::*;
use crate::mailspace::SendRequest;
use crate::pool::collect::{Collected, JobResult, Outcome};
use crate::pool::record::{DispatchRecord, OpenDispatch, collected_note, dispatch_note};

pub(in crate::mailspace) fn space_with_task() -> (tempfile::TempDir, Mailspace, String) {
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
            subject: "Fix the edge".into(),
            body: vivi_mail::letter::fixture::task_body_with(
                &vivi_mail::letter::work::TaskFront::default(),
                "do it",
            ),
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
        })
        .unwrap();
    let handle = sent.delivered[0].handle.clone();
    (tmp, space, handle)
}

pub(in crate::mailspace) fn record(job: &str, slot: &str) -> DispatchRecord {
    DispatchRecord {
        target: Some("app".into()),
        repository: Some("acme/app".into()),
        pool: "lanes".into(),
        need: Some("need-1".into()),
        kind: Some("hand".into()),
        slot: slot.into(),
        machine: "studio".into(),
        job: job.into(),
        model: "m/ok".into(),
        from: "main".into(),
        branch: "factory/x".into(),
        start: "a".repeat(40),
        with: Vec::new(),
        budget: 1800,
        check_allowance_s: 0,
        at: 1_793_527_200,
        read_only: false,
        hardware: String::new(),
        abandoned: 0,
        abandoned_cost_micros: 0,
    }
}

fn finished() -> Outcome {
    Outcome::Finished(Collected {
        result: JobResult::parse(
            "push=pushed\nsha=bbbb\nagent_rc=0\nagent_s=240\ncheck_rc=1\ncheck_s=30\njob_s=290\n",
        ),
        receipt: "did it".into(),
        stderr: String::new(),
        log: String::new(),
    })
}

pub(in crate::mailspace) fn dispatch(space: &Mailspace, task: &str, job: &str, slot: &str) {
    space
        .record_pool_note(
            task,
            "pool_dispatched",
            &dispatch_note(&record(job, slot)),
            "go",
        )
        .unwrap();
}

pub(in crate::mailspace) fn collect(space: &Mailspace, task: &str, job: &str, slot: &str) {
    let open = OpenDispatch {
        task: task.into(),
        record: record(job, slot),
    };
    space
        .record_pool_note(
            task,
            "pool_collected",
            &collected_note(&open, &finished()),
            "done",
        )
        .unwrap();
}

#[test]
fn a_dispatch_through_the_pool_record_writes_the_row_with_the_task_handle() {
    let (_tmp, space, handle) = space_with_task();
    dispatch(&space, &handle, "j1", "slot-a");

    let run = space.slot_run("j1").unwrap().unwrap();

    assert_eq!(run.task, handle);
    assert_eq!(run.run.pool, "lanes");
    assert_eq!(run.run.slot, "slot-a");
    assert_eq!(run.run.need.as_deref(), Some("need-1"));
    assert_eq!(run.run.budget_s, Some(1800));
    assert_eq!(run.run.start_sha, "a".repeat(40));
    assert_eq!(run.run.dispatched_at, 1_793_527_200);
    assert_eq!(run.run.collected_at, None);
    assert_eq!(space.open_slot_runs().unwrap().len(), 1);
}

#[test]
fn a_collect_through_the_pool_record_completes_the_row() {
    let (_tmp, space, handle) = space_with_task();
    dispatch(&space, &handle, "j1", "slot-a");
    collect(&space, &handle, "j1", "slot-a");

    let run = space.slot_run("j1").unwrap().unwrap().run;

    assert_eq!(run.outcome.as_deref(), Some("finished"));
    assert_eq!(run.push.as_deref(), Some("pushed"));
    assert_eq!((run.agent_rc, run.agent_s), (Some(0), Some(240)));
    assert_eq!(
        (run.check_rc, run.check_s, run.job_s),
        (Some(1), Some(30), Some(290))
    );
    let collected_at = run.collected_at.expect("stamped");
    assert!(collected_at >= run.dispatched_at, "{collected_at}");
    assert_eq!(run.need.as_deref(), Some("need-1"));
    assert!(space.open_slot_runs().unwrap().is_empty());
}

#[test]
fn a_collect_without_a_dispatch_still_makes_a_row() {
    let (_tmp, space, handle) = space_with_task();
    collect(&space, &handle, "j9", "slot-a");

    let run = space.slot_run("j9").unwrap().unwrap().run;

    assert_eq!(run.dispatched_at, run.collected_at.unwrap());
    assert_eq!(run.outcome.as_deref(), Some("finished"));
}

#[test]
fn a_malformed_note_still_writes_the_event_and_the_reply_but_no_row() {
    let (_tmp, space, handle) = space_with_task();
    space
        .record_pool_note(&handle, "pool_dispatched", "not json", "go")
        .unwrap();
    space
        .record_pool_note(
            &handle,
            "pool_collected",
            r#"{"outcome":"finished"}"#,
            "done",
        )
        .unwrap();

    assert_eq!(space.pool_events().unwrap().len(), 2);
    assert!(space.recent_slot_runs(0, 10).unwrap().is_empty());
    let thread = space.thread(&handle, false, 10, 10).unwrap();
    assert!(thread.len() >= 3, "task plus two replies: {}", thread.len());
}

#[test]
fn rebuild_reproduces_the_incremental_rows() {
    let (_tmp, space, handle) = space_with_task();
    dispatch(&space, &handle, "j1", "slot-a");
    collect(&space, &handle, "j1", "slot-a");
    dispatch(&space, &handle, "j2", "slot-b");
    let before = space.recent_slot_runs(0, 10).unwrap();
    assert_eq!(before.len(), 2);

    assert_eq!(space.rebuild_slot_runs().unwrap(), 2);
    assert_eq!(space.recent_slot_runs(0, 10).unwrap(), before);
    assert_eq!(space.rebuild_slot_runs().unwrap(), 2);
    assert_eq!(space.recent_slot_runs(0, 10).unwrap(), before);
}

#[test]
fn history_reads_a_slot_newest_first_in_one_pool_or_any() {
    let (_tmp, mut space, handle) = space_with_task();
    dispatch(&space, &handle, "j1", "slot-a");
    dispatch(&space, &handle, "j2", "slot-a");
    dispatch(&space, &handle, "j3", "slot-b");
    space.config.pools.insert(
        "lanes".into(),
        toml::from_str("provider = \"local\"\nslots = 1").unwrap(),
    );

    let in_pool = space.slot_history("lanes", "slot-a", 10).unwrap();
    assert_eq!(in_pool.len(), 2);
    assert_eq!(space.slot_history("lanes", "slot-a", 1).unwrap().len(), 1);
    assert!(
        space
            .slot_history("other", "slot-a", 10)
            .unwrap()
            .is_empty()
    );
    assert_eq!(space.slot_history_any_pool("slot-a", 10).unwrap().len(), 2);
    assert_eq!(space.need_slot_runs("need-1", 10).unwrap().len(), 3);
}
