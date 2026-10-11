use std::cell::RefCell;

use super::*;
use crate::pool::fake_provider::{Fake, claimed};
use crate::pool::record::DispatchRecord;

const RESULT_OK: &str = "@@vivi-pool result\njob=j1\nstate=done\npush=pushed\nbranch=factory/a\nsha=0123456789abcdef0123456789abcdef01234567\nstart=fedcba9876543210\nagent_rc=0\nagent_s=12\njob_s=30\nmodel=m/x\nslot=p-slot-1\n@@vivi-pool receipt\nall done\nsecond line\n\n@@vivi-pool stderr\n\n@@vivi-pool log\nfrom main at fedcba987654\n\n";

fn open(task: &str, job: &str, at: u64) -> OpenDispatch {
    OpenDispatch {
        task: task.into(),
        record: DispatchRecord {
            slot: "p-slot-1".into(),
            machine: "m1".into(),
            job: job.into(),
            model: "m/x".into(),
            from: "main".into(),
            branch: "factory/a".into(),
            start: "f".repeat(40),
            budget: 600,
            at,
        },
    }
}

fn pacing() -> Pacing {
    Pacing {
        poll: Duration::ZERO,
        ready_timeout: Duration::ZERO,
    }
}

fn fake_with(state: &str, output: &str) -> Fake {
    let fake = Fake::default();
    fake.add(claimed("m1", "p-slot-1", state, "p", "aaaa1111"));
    if !output.is_empty() {
        fake.exec_out
            .borrow_mut()
            .insert("m1".into(), output.into());
    }
    fake
}

#[test]
fn result_file_parses_key_value_lines() {
    let r =
        JobResult::parse("job=j\npush=pushed\nbranch=b/x\nsha=0123456789abcdef\njunk\n k = v \n");
    assert_eq!(r.get("job"), "j");
    assert_eq!(r.get("k"), "v");
    assert_eq!(r.get("missing"), "");
    assert_eq!(r.push_line(), "pushed b/x 0123456789");
    let refused = JobResult::parse("push=push_refused\n");
    assert_eq!(refused.push_line(), "push_refused");
}

#[test]
fn collected_output_parses_into_sections() {
    let done = parse_collected(RESULT_OK).unwrap();
    assert_eq!(done.result.get("job"), "j1");
    assert_eq!(done.receipt, "all done\nsecond line\n\n");
    assert!(done.stderr.trim().is_empty());
    assert!(done.log.starts_with("from main"));
    assert!(done.result.summary().contains("agent exit 0 in 12s"));
}

#[test]
fn empty_or_jobless_output_is_not_a_result() {
    assert!(parse_collected("").is_none());
    assert!(parse_collected("@@vivi-pool result\npush=pushed\n").is_none());
    assert!(parse_collected("garbage with no markers").is_none());
}

#[test]
fn a_running_slot_with_a_matching_result_finishes() {
    let fake = fake_with("started", RESULT_OK);
    let outcome = poll(&fake, &open("aaaa1111", "j1", 1000), 1100, &pacing()).unwrap();
    assert!(matches!(outcome, Some(Outcome::Finished(_))));
}

#[test]
fn a_leftover_result_from_another_job_is_not_ours() {
    let fake = fake_with("started", RESULT_OK);
    let outcome = poll(&fake, &open("aaaa1111", "other-job", 1000), 1100, &pacing()).unwrap();
    assert!(outcome.is_none(), "still running");
}

#[test]
fn a_running_slot_without_a_result_keeps_running_until_overdue() {
    let fake = fake_with("started", "");
    let d = open("aaaa1111", "j1", 1000);
    assert!(poll(&fake, &d, 1500, &pacing()).unwrap().is_none());
    let late = 1000 + 600 + TIMEOUT_GRACE_SECS + 1;
    assert_eq!(
        poll(&fake, &d, late, &pacing()).unwrap(),
        Some(Outcome::Timeout)
    );
}

#[test]
fn a_stopped_slot_is_started_read_and_the_result_collected() {
    let fake = fake_with("stopped", RESULT_OK);
    // The fake runs a started machine at once, so the first read succeeds.
    let outcome = poll(&fake, &open("aaaa1111", "j1", 1000), 1200, &pacing()).unwrap();
    assert!(matches!(outcome, Some(Outcome::Finished(_))));
    assert_eq!(fake.calls_to("start"), 1);
}

#[test]
fn a_stopped_slot_without_a_result_is_lost_or_timed_out() {
    let fake = fake_with("stopped", "");
    let d = open("aaaa1111", "j1", 1000);
    assert_eq!(
        poll(&fake, &d, 1200, &pacing()).unwrap(),
        Some(Outcome::Lost)
    );
    let late = 1000 + 600 + TIMEOUT_GRACE_SECS + 1;
    assert_eq!(
        poll(&fake, &d, late, &pacing()).unwrap(),
        Some(Outcome::Timeout)
    );
}

#[test]
fn a_freshly_stopped_slot_is_given_time_to_start() {
    let fake = fake_with("stopped", "");
    let d = open("aaaa1111", "j1", 1000);
    assert!(poll(&fake, &d, 1010, &pacing()).unwrap().is_none());
    assert_eq!(fake.calls_to("start"), 0);
}

#[test]
fn a_destroyed_slot_is_lost() {
    let fake = fake_with("failed", "");
    let outcome = poll(&fake, &open("aaaa1111", "j1", 1000), 1200, &pacing()).unwrap();
    assert_eq!(outcome, Some(Outcome::Lost));
}

#[test]
fn release_stops_a_running_slot_then_drops_the_marks() {
    let fake = fake_with("started", "");
    release(&fake, "m1", "aaaa1111").unwrap();
    assert_eq!(fake.state_of("m1"), "stopped");
    assert!(fake.meta("m1", META_TASK).is_none());
}

#[test]
fn release_leaves_a_slot_another_task_now_holds() {
    let fake = fake_with("started", "");
    release(&fake, "m1", "bbbb2222").unwrap();
    assert_eq!(fake.state_of("m1"), "started");
    assert_eq!(fake.meta("m1", META_TASK).as_deref(), Some("aaaa1111"));
}

#[test]
fn a_round_records_before_it_frees_the_slot() {
    let fake = fake_with("started", RESULT_OK);
    let order = RefCell::new(Vec::new());
    let mut record = |d: &OpenDispatch, o: &Outcome| {
        order.borrow_mut().push(format!(
            "record {} {} slot-still-marked={}",
            d.task,
            o.word(),
            fake.meta("m1", META_TASK).is_some()
        ));
        Ok(true)
    };
    let lines = run_round(
        &fake,
        &[open("aaaa1111", "j1", 1000)],
        1100,
        &pacing(),
        &mut record,
    )
    .unwrap();
    assert_eq!(
        order.borrow()[0],
        "record aaaa1111 finished slot-still-marked=true"
    );
    assert_eq!(lines[0].state, "finished");
    assert!(lines[0].detail.starts_with("pushed factory/a"));
    assert!(fake.meta("m1", META_TASK).is_none());
    assert_eq!(fake.state_of("m1"), "stopped");
}

#[test]
fn a_failed_record_leaves_the_slot_for_the_next_round() {
    let fake = fake_with("started", RESULT_OK);
    let mut record =
        |_: &OpenDispatch, _: &Outcome| Err(VivariumError::Message("db locked".into()));
    let result = run_round(
        &fake,
        &[open("aaaa1111", "j1", 1000)],
        1100,
        &pacing(),
        &mut record,
    );
    assert!(result.is_err());
    assert_eq!(fake.state_of("m1"), "started");
    assert!(fake.meta("m1", META_TASK).is_some());
}

#[test]
fn a_dispatch_closed_elsewhere_is_neither_reported_nor_freed() {
    let fake = fake_with("started", RESULT_OK);
    let mut record = |_: &OpenDispatch, _: &Outcome| Ok(false);
    let lines = run_round(
        &fake,
        &[open("aaaa1111", "j1", 1000)],
        1100,
        &pacing(),
        &mut record,
    )
    .unwrap();
    assert!(lines.is_empty());
    assert_eq!(fake.state_of("m1"), "started");
}

#[test]
fn a_running_dispatch_reports_its_age_and_is_left_alone() {
    let fake = fake_with("started", "");
    let mut record = |_: &OpenDispatch, _: &Outcome| Ok(true);
    let lines = run_round(
        &fake,
        &[open("aaaa1111", "j1", 1000)],
        1090,
        &pacing(),
        &mut record,
    )
    .unwrap();
    assert_eq!(lines[0].state, "running");
    assert_eq!(lines[0].detail, "90s of 600s");
    assert!(!lines[0].settled());
}

#[test]
fn a_timeout_is_recorded_and_the_slot_stopped() {
    let fake = fake_with("started", "");
    let seen = RefCell::new(String::new());
    let mut record = |_: &OpenDispatch, o: &Outcome| {
        *seen.borrow_mut() = o.word().to_string();
        Ok(true)
    };
    let late = 1000 + 600 + TIMEOUT_GRACE_SECS + 5;
    let lines = run_round(
        &fake,
        &[open("aaaa1111", "j1", 1000)],
        late,
        &pacing(),
        &mut record,
    )
    .unwrap();
    assert_eq!(*seen.borrow(), "timeout");
    assert_eq!(lines[0].state, "timeout");
    assert_eq!(fake.state_of("m1"), "stopped");
    assert!(fake.meta("m1", META_TASK).is_none());
}

#[test]
fn stale_claims_need_no_dispatch_and_enough_age() {
    let mut old = claimed("m1", "p-slot-1", "stopped", "p", "orphan11");
    old.metadata.insert(META_CLAIMED.into(), "1000".into());
    let mut recent = claimed("m2", "p-slot-2", "stopped", "p", "orphan22");
    recent.metadata.insert(META_CLAIMED.into(), "1900".into());
    let mut live = claimed("m3", "p-slot-3", "started", "p", "aaaa1111");
    live.metadata.insert(META_CLAIMED.into(), "100".into());
    let slots = crate::pool::slot::pool_slots("p", &[old, recent, live]);
    let mut dispatch = open("aaaa1111", "j1", 100);
    dispatch.record.machine = "m3".into();
    let stale = stale_claims(&slots, &[dispatch], 2000);
    let names: Vec<&str> = stale.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["p-slot-1"]);
}
