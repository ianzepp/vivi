use std::cell::RefCell;

use super::*;
use crate::pool::fake_provider::{Fake, claimed, pid, slot};
use crate::pool::pools::PoolSet;
use crate::pool::record::DispatchRecord;
use crate::pool::slot::{META_CLAIMED, PoolId};

const RESULT_OK: &str = "@@vivi-pool result\njob=j1\nstate=done\npush=pushed\nbranch=factory/a\nsha=0123456789abcdef0123456789abcdef01234567\nstart=fedcba9876543210\nagent_rc=0\nagent_s=12\njob_s=30\nmodel=m/x\nslot=p-slot-1\n@@vivi-pool receipt\nall done\nsecond line\n\n@@vivi-pool stderr\n\n@@vivi-pool log\nfrom main at fedcba987654\n\n";

fn open(task: &str, job: &str, at: u64) -> OpenDispatch {
    OpenDispatch {
        task: task.into(),
        record: DispatchRecord {
            pool: "p".into(),
            slot: "p-slot-1".into(),
            machine: "m1".into(),
            job: job.into(),
            model: "m/x".into(),
            from: "main".into(),
            branch: "factory/a".into(),
            start: "f".repeat(40),
            budget: 600,
            at,
            read_only: false,
            hardware: String::new(),
            abandoned: 0,
            abandoned_cost_micros: 0,
        },
    }
}

fn pacing() -> Pacing {
    Pacing {
        poll: Duration::ZERO,
        ready_timeout: Duration::ZERO,
        stop_timeout: Duration::ZERO,
        start_grace: Duration::from_secs(START_GRACE_SECS),
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

const RESULT_READ_ONLY: &str = "@@vivi-pool result\njob=j1\nstate=done\npush=read_only_discarded\nbranch=\nsha=\nstart=fedcba9876543210\nagent_rc=0\nagent_s=12\njob_s=30\nmodel=m/x\nslot=p-slot-1\ndiscarded_count=2\ndiscarded_path_1=a.txt\ndiscarded_path_2=new.txt\n@@vivi-pool receipt\nreviewed\n\n@@vivi-pool stderr\n\n@@vivi-pool log\n\n";

#[test]
fn the_two_read_only_outcomes_are_their_own_words() {
    let unchanged =
        JobResult::parse("push=read_only_unchanged\nbranch=\nsha=\nstart=abcdef0123456789\n");
    assert_eq!(unchanged.push_line(), "read_only_unchanged");
    assert!(unchanged.discarded_paths().is_empty());
    assert!(
        unchanged
            .summary()
            .starts_with("result: read_only_unchanged\n")
    );
    let done = parse_collected(RESULT_READ_ONLY).unwrap();
    assert_eq!(done.result.push_line(), "read_only_discarded 2");
    assert_eq!(done.result.discarded_paths(), ["a.txt", "new.txt"]);
    assert!(
        done.result
            .summary()
            .starts_with("result: read_only_discarded 2\n")
    );
    for word in [READ_ONLY_UNCHANGED, READ_ONLY_DISCARDED] {
        for old in ["no_changes", "pushed", "committed", "push_refused"] {
            assert_ne!(word, old);
        }
    }
}

#[test]
fn restored_paths_are_parsed_in_numeric_order_and_a_count_alone_is_not_one() {
    let done = parse_collected(
        "@@vivi-pool result\njob=j1\nstate=done\npush=pushed\nbranch=factory/a\nsha=0123456789abcdef0123456789abcdef01234567\nstart=fedcba9876543210\nrestored_count=3\nrestored_path_1=runtime/llvm/Cargo.lock\nrestored_path_2=dir/b c.txt\nrestored_path_10=z.txt\n@@vivi-pool receipt\nreviewed\n\n@@vivi-pool stderr\n\n@@vivi-pool log\n\n",
    )
    .unwrap();
    assert_eq!(
        done.result.restored_paths(),
        ["runtime/llvm/Cargo.lock", "dir/b c.txt", "z.txt"]
    );
    let none = JobResult::parse("push=pushed\nrestored_count=0\n");
    assert_eq!(none.get("restored_count"), "0");
    assert!(none.restored_paths().is_empty());
}

#[test]
fn a_read_only_result_is_collected_from_a_running_slot() {
    let fake = fake_with("started", RESULT_READ_ONLY);
    let Some(Outcome::Finished(done)) =
        poll(&fake, &open("aaaa1111", "j1", 1000), 1100, &pacing()).unwrap()
    else {
        panic!("not finished");
    };
    assert_eq!(done.result.push(), READ_ONLY_DISCARDED);
    assert_eq!(done.receipt, "reviewed\n\n");
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

fn readable(result: &str) -> Fake {
    let fake = fake_with("stopped", "");
    let files = [
        (RESULT_PATHS[0], result),
        (RESULT_PATHS[1], "the receipt\n"),
        (RESULT_PATHS[3], "run log\n"),
    ];
    *fake.stopped_files.borrow_mut() = Some(
        files
            .iter()
            .map(|(p, t)| ((*p).to_string(), t.as_bytes().to_vec()))
            .collect(),
    );
    fake
}

#[test]
fn a_provider_that_reads_the_stopped_disk_collects_without_a_restart() {
    let fake = readable("job=j1\nstate=done\npush=pushed\nbranch=b\nsha=0123456789abcdef\n");
    // Younger than the start grace: no grace applies, a stopped slot there is final.
    let outcome = poll(&fake, &open("aaaa1111", "j1", 1000), 1005, &pacing()).unwrap();
    let Some(Outcome::Finished(done)) = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(done.result.push(), "pushed");
    assert_eq!(done.receipt, "the receipt\n\n");
    assert_eq!(done.stderr, "\n", "a file that is not there reads as empty");
    assert_eq!(done.log, "run log\n\n");
    assert_eq!(fake.calls_to("start"), 0);
    assert_eq!(fake.calls_to("exec"), 0);
}

#[test]
fn the_result_of_another_job_on_a_readable_stopped_disk_is_lost_or_timed_out() {
    let fake = readable("job=earlier\nstate=done\npush=pushed\n");
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
    assert_eq!(fake.calls_to("start"), 0);
}

#[test]
fn collected_from_files_keeps_the_head_of_the_receipt_and_the_tails_of_the_logs() {
    let mut files = BTreeMap::new();
    files.insert(RESULT_PATHS[0].to_string(), b"job=j9\n".to_vec());
    files.insert(RESULT_PATHS[1].to_string(), vec![b'a'; RECEIPT_BYTES + 10]);
    files.insert(
        RESULT_PATHS[2].to_string(),
        [vec![b'x'; 5000], b"END".to_vec()].concat(),
    );
    let got = collected_from_files(&files).unwrap();
    assert_eq!(got.receipt.len(), RECEIPT_BYTES + 1);
    assert_eq!(got.stderr.len(), TAIL_BYTES + 1);
    assert!(got.stderr.ends_with("END\n"));
    assert!(collected_from_files(&BTreeMap::new()).is_none());
    files.insert(RESULT_PATHS[0].to_string(), b"state=done\n".to_vec());
    assert!(
        collected_from_files(&files).is_none(),
        "no job id, no result"
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
    release(&fake, "m1", "aaaa1111", &pacing()).unwrap();
    assert_eq!(fake.state_of("m1"), "stopped");
    assert!(fake.meta("m1", META_TASK).is_none());
}

#[test]
fn release_leaves_a_slot_another_task_now_holds() {
    let fake = fake_with("started", "");
    release(&fake, "m1", "bbbb2222", &pacing()).unwrap();
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
        &PoolSet::single(&pid("p"), &fake),
        &[open("aaaa1111", "j1", 1000)],
        1100,
        &pacing(),
        Missing::Report,
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
        &PoolSet::single(&pid("p"), &fake),
        &[open("aaaa1111", "j1", 1000)],
        1100,
        &pacing(),
        Missing::Report,
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
        &PoolSet::single(&pid("p"), &fake),
        &[open("aaaa1111", "j1", 1000)],
        1100,
        &pacing(),
        Missing::Report,
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
        &PoolSet::single(&pid("p"), &fake),
        &[open("aaaa1111", "j1", 1000)],
        1090,
        &pacing(),
        Missing::Report,
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
        &PoolSet::single(&pid("p"), &fake),
        &[open("aaaa1111", "j1", 1000)],
        late,
        &pacing(),
        Missing::Report,
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
    let slots = crate::pool::slot::pool_slots(&pid("p"), &[old, recent, live]);
    let mut dispatch = open("aaaa1111", "j1", 100);
    dispatch.record.machine = "m3".into();
    let stale = stale_claims(&slots, &[dispatch], 2000, STALE_CLAIM_SECS);
    let names: Vec<&str> = stale.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["p-slot-1"]);
}

// --- provider errors are not "still running" ---

#[test]
fn an_unreadable_running_slot_is_an_error_not_still_running() {
    let fake = fake_with("started", RESULT_OK);
    fake.exec_fails.set(1);
    let d = open("aaaa1111", "j1", 1000);
    let err = poll(&fake, &d, 1100, &pacing()).unwrap_err();
    assert!(err.to_string().contains("HTTP 401"), "{err}");
    // Once the provider answers again the same dispatch finishes.
    assert!(matches!(
        poll(&fake, &d, 1100, &pacing()).unwrap(),
        Some(Outcome::Finished(_))
    ));
}

#[test]
fn an_unreadable_overdue_slot_still_times_out() {
    let fake = fake_with("started", "");
    fake.exec_fails.set(5);
    let late = 1000 + 600 + TIMEOUT_GRACE_SECS + 1;
    let outcome = poll(&fake, &open("aaaa1111", "j1", 1000), late, &pacing()).unwrap();
    assert_eq!(outcome, Some(Outcome::Timeout));
}

#[test]
fn a_stopped_slot_that_never_answers_is_an_error_not_lost() {
    let fake = fake_with("stopped", RESULT_OK);
    fake.exec_fails.set(100);
    let d = open("aaaa1111", "j1", 1000);
    let err = poll(&fake, &d, 1200, &pacing()).unwrap_err();
    assert!(err.to_string().contains("HTTP 401"), "{err}");
}

#[test]
fn a_transitional_machine_state_is_looked_at_again_next_round() {
    for state in ["starting", "stopping"] {
        let fake = fake_with(state, RESULT_OK);
        let d = open("aaaa1111", "j1", 1000);
        assert!(
            poll(&fake, &d, 1100, &pacing()).unwrap().is_none(),
            "{state}"
        );
        assert_eq!(fake.calls_to("exec"), 0);
    }
}

fn round(fake: &Fake, now: u64) -> Result<Vec<RoundLine>, VivariumError> {
    let mut record = |_: &OpenDispatch, _: &Outcome| Ok(true);
    run_round(
        &PoolSet::single(&pid("p"), fake),
        &[open("aaaa1111", "j1", 1000)],
        now,
        &pacing(),
        Missing::Report,
        &mut record,
    )
}

#[test]
fn a_provider_that_fails_for_a_while_then_recovers_is_reported_then_collected() {
    let fake = fake_with("started", RESULT_OK);
    fake.machine_fails.set(3);
    let mut watch = ErrorWatch::default();
    let mut notices = Vec::new();
    let mut finished = false;
    for _ in 0..6 {
        let lines = round(&fake, 1100).unwrap();
        notices.extend(watch.observe(&lines, 0));
        if lines.iter().any(RoundLine::settled) {
            finished = true;
            break;
        }
    }
    assert!(
        finished,
        "the dispatch is collected once the provider answers"
    );
    assert_eq!(notices.len(), 2, "{notices:?}");
    assert!(notices[0].starts_with("warning: cannot poll aaaa1111"));
    assert!(notices[0].contains("Fly unreachable"));
    assert!(notices[1].starts_with("recovered: polling aaaa1111"));
}

fn error_line(task: &str, detail: &str) -> RoundLine {
    RoundLine {
        task: task.into(),
        pool: "p".into(),
        slot: "s".into(),
        state: "error",
        detail: detail.into(),
    }
}

fn running_line(task: &str) -> RoundLine {
    RoundLine {
        task: task.into(),
        pool: "p".into(),
        slot: "s".into(),
        state: "running",
        detail: "1s of 2s".into(),
    }
}

#[test]
fn the_same_error_is_shown_once_and_a_changed_one_again() {
    let mut watch = ErrorWatch::default();
    assert_eq!(watch.observe(&[error_line("t", "403")], 0).len(), 1);
    assert!(watch.observe(&[error_line("t", "403")], 0).is_empty());
    assert_eq!(watch.observe(&[error_line("t", "timeout")], 0).len(), 1);
    assert_eq!(watch.observe(&[running_line("t")], 0).len(), 1, "recovery");
    assert!(watch.observe(&[running_line("t")], 0).is_empty());
    assert_eq!(
        watch.observe(&[error_line("t", "403")], 0).len(),
        1,
        "again"
    );
}

// --- the slot ends stopped and free ---

#[test]
fn collecting_from_a_stopped_slot_leaves_it_stopped_and_unmarked() {
    let fake = fake_with("stopped", RESULT_OK);
    let lines = round(&fake, 1200).unwrap();
    assert_eq!(lines[0].state, "finished");
    assert_eq!(fake.calls_to("start"), 1);
    assert_eq!(fake.calls_to("stop"), 1);
    assert_eq!(fake.state_of("m1"), "stopped");
    assert!(fake.meta("m1", META_TASK).is_none());
}

#[test]
fn a_stop_that_does_not_take_is_asked_again() {
    let fake = fake_with("started", "");
    fake.stops_ignored.set(1);
    release(&fake, "m1", "aaaa1111", &pacing_with_stop()).unwrap();
    assert_eq!(fake.state_of("m1"), "stopped");
    assert_eq!(fake.calls_to("stop"), 2);
    assert!(fake.meta("m1", META_TASK).is_none());
}

fn pacing_with_stop() -> Pacing {
    Pacing {
        stop_timeout: Duration::from_secs(5),
        ..pacing()
    }
}

#[test]
fn a_slot_that_will_not_stop_keeps_its_marks_for_reconcile() {
    let fake = fake_with("started", "");
    fake.stops_ignored.set(1000);
    let pacing = Pacing {
        stop_timeout: Duration::from_millis(20),
        poll: Duration::from_millis(1),
        ..pacing()
    };
    let err = release(&fake, "m1", "aaaa1111", &pacing).unwrap_err();
    assert!(err.to_string().contains("did not stop"), "{err}");
    assert_eq!(fake.meta("m1", META_TASK).as_deref(), Some("aaaa1111"));
}

#[test]
fn a_result_is_reported_even_when_the_slot_would_not_free() {
    let fake = fake_with("started", RESULT_OK);
    fake.stops_ignored.set(1000);
    let mut record = |_: &OpenDispatch, _: &Outcome| Ok(true);
    let quick = Pacing {
        stop_timeout: Duration::from_millis(5),
        poll: Duration::from_millis(1),
        ..pacing()
    };
    let lines = run_round(
        &PoolSet::single(&pid("p"), &fake),
        &[open("aaaa1111", "j1", 1000)],
        1100,
        &quick,
        Missing::Report,
        &mut record,
    )
    .unwrap();
    assert_eq!(lines[0].state, "finished");
    assert!(
        lines[0].detail.contains("slot not freed yet"),
        "{}",
        lines[0].detail
    );
}

#[test]
fn a_dispatch_closed_elsewhere_stops_a_slot_this_poll_started_unmarked() {
    // The holder freed the slot (marks gone); this poll started it afterwards.
    let fake = Fake::default();
    fake.add(slot("m1", "p-slot-1", "started", "p", 1));
    let mut record = |_: &OpenDispatch, _: &Outcome| Ok(false);
    // Overdue, so the running slot yields an outcome without a result.
    let late = 1000 + 600 + TIMEOUT_GRACE_SECS + 1;
    let lines = run_round(
        &PoolSet::single(&pid("p"), &fake),
        &[open("aaaa1111", "j1", 1000)],
        late,
        &pacing(),
        Missing::Report,
        &mut record,
    )
    .unwrap();
    assert!(lines.is_empty());
    assert_eq!(fake.state_of("m1"), "stopped");
}

#[test]
fn release_stops_a_running_slot_nobody_holds_but_not_one_another_task_holds() {
    let fake = Fake::default();
    fake.add(slot("m1", "p-slot-1", "started", "p", 1));
    release(&fake, "m1", "aaaa1111", &pacing()).unwrap();
    assert_eq!(fake.state_of("m1"), "stopped", "unmarked and running");

    let held = fake_with("started", "");
    release(&held, "m1", "bbbb2222", &pacing()).unwrap();
    assert_eq!(held.state_of("m1"), "started", "held by another task");
}

#[test]
fn a_seeding_machine_is_never_stopped_as_an_orphan() {
    let fake = Fake::default();
    let mut seeding = slot("m1", "p-slot-1", "started", "p", 1);
    seeding.metadata.insert(
        crate::pool::slot::META_STATE.into(),
        crate::pool::slot::STATE_SEEDING.into(),
    );
    fake.add(seeding.clone());
    stop_unclaimed(&fake, &seeding, &pacing()).unwrap();
    assert_eq!(fake.state_of("m1"), "started");
}

#[test]
fn orphan_runners_are_running_unmarked_slots_with_no_dispatch() {
    let marked = claimed("m1", "p-slot-1", "started", "p", "aaaa1111");
    let orphan = slot("m2", "p-slot-2", "started", "p", 2);
    let idle = slot("m3", "p-slot-3", "stopped", "p", 3);
    let mut busy = slot("m4", "p-slot-4", "started", "p", 4);
    busy.id = "m4".into();
    let slots = crate::pool::slot::pool_slots(&pid("p"), &[marked, orphan, idle, busy]);
    let mut dispatch = open("bbbb2222", "j2", 100);
    dispatch.record.machine = "m4".into();
    let names: Vec<&str> = orphan_runners(&slots, &[dispatch])
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(names, ["p-slot-2"]);
}

#[test]
fn a_machine_the_provider_no_longer_knows_is_lost_and_closed_not_an_error_forever() {
    let fake = Fake::default();
    let mut dispatch = open("aaaa1111", "j1", 1000);
    dispatch.record.machine = "m-gone".into();
    let seen = RefCell::new(Vec::new());
    let mut record = |d: &OpenDispatch, outcome: &Outcome| {
        seen.borrow_mut().push((d.task.clone(), outcome.word()));
        Ok(true)
    };
    let lines = run_round(
        &PoolSet::single(&pid("p"), &fake),
        &[dispatch],
        1100,
        &pacing(),
        Missing::Report,
        &mut record,
    )
    .unwrap();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].state, "lost", "{lines:?}");
    assert!(lines[0].settled());
    assert_eq!(*seen.borrow(), [("aaaa1111".to_string(), "lost")]);
    assert_eq!(fake.calls_to("start"), 0, "nothing is started to look");
}

#[test]
fn the_watch_keeps_each_problem_with_when_it_began_and_cleared() {
    let mut watch = ErrorWatch::default();
    watch.observe(&[error_line("t", "403")], 100);
    watch.observe(&[error_line("t", "403")], 110);
    watch.observe(&[error_line("t", "timeout")], 120);
    watch.observe(&[running_line("t")], 150);
    watch.observe(&[error_line("u", "403")], 160);
    let seen: Vec<_> = watch
        .issues()
        .iter()
        .map(|i| {
            (
                i.kind,
                i.subject.clone(),
                i.message.clone(),
                i.first_seen,
                i.cleared,
            )
        })
        .collect();
    assert_eq!(
        seen,
        [
            (
                "poll-error",
                Some("t".to_string()),
                "403".to_string(),
                100,
                Some(120)
            ),
            (
                "poll-error",
                Some("t".to_string()),
                "timeout".to_string(),
                120,
                Some(150)
            ),
            (
                "poll-error",
                Some("u".to_string()),
                "403".to_string(),
                160,
                None
            ),
        ]
    );
}

fn set_with_no_p<'a>(id: &'a PoolId, fake: &'a Fake) -> PoolSet<'a> {
    PoolSet {
        members: PoolSet::single(id, fake).members,
        all: true,
    }
}

fn gone_pool_dispatch() -> OpenDispatch {
    let mut gone = open("bbbb2222", "j9", 1000);
    gone.record.pool = "removed".into();
    gone
}

#[test]
fn a_dispatch_of_a_removed_pool_is_reported_not_polled_and_left_open() {
    let fake = fake_with("started", RESULT_OK);
    let other = pid("other");
    let set = set_with_no_p(&other, &fake);
    let mut record = |_: &OpenDispatch, _: &Outcome| Ok(true);
    let lines = run_round(
        &set,
        &[gone_pool_dispatch()],
        1100,
        &pacing(),
        Missing::Report,
        &mut record,
    )
    .unwrap();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].state, "error");
    assert_eq!(lines[0].pool, "removed");
    assert!(
        lines[0]
            .detail
            .contains("pool \"removed\" is not in .vivi/mailspace.toml")
    );
    assert!(lines[0].detail.contains("reconcile"));
    assert_eq!(fake.calls_to("exec"), 0, "no machine host was asked");
}

#[test]
fn reconcile_closes_a_dispatch_of_a_removed_pool_as_lost() {
    let fake = fake_with("started", RESULT_OK);
    let other = pid("other");
    let set = set_with_no_p(&other, &fake);
    let seen = RefCell::new(Vec::new());
    let mut record = |d: &OpenDispatch, o: &Outcome| {
        seen.borrow_mut().push(format!("{} {}", d.task, o.word()));
        Ok(true)
    };
    let lines = run_round(
        &set,
        &[gone_pool_dispatch()],
        1100,
        &pacing(),
        Missing::Close,
        &mut record,
    )
    .unwrap();
    assert_eq!(*seen.borrow(), ["bbbb2222 lost"]);
    assert_eq!(lines[0].state, "lost");
    assert!(lines[0].detail.contains("closed as lost"));
    assert!(lines[0].settled());
}

#[test]
fn a_set_for_one_pool_leaves_the_dispatches_of_other_pools_alone() {
    let fake = fake_with("started", "");
    let id = pid("p");
    let set = PoolSet::single(&id, &fake);
    let mut record = |_: &OpenDispatch, _: &Outcome| Ok(true);
    let lines = run_round(
        &set,
        &[gone_pool_dispatch()],
        1100,
        &pacing(),
        Missing::Close,
        &mut record,
    )
    .unwrap();
    assert!(
        lines.is_empty(),
        "an explicit --pool does not close other pools' dispatches"
    );
}

#[test]
fn a_dispatch_recorded_without_a_pool_reads_as_made_before_pools_were_named() {
    assert!(missing_pool_text("").contains("before pools were named"));
}

#[test]
fn facts_survive_the_collect_output_as_result_lines() {
    let output = "@@vivi-pool result\njob=j1\nstate=done\npush=read_only_unchanged\nfact_1_cmd=gpu-query --csv\nfact_1_exit=0\nfact_1_out=Some GPU, 550.1\\n8 GiB\n@@vivi-pool receipt\nfact_2_cmd=not a fact: the receipt is another section\n";
    let done = parse_collected(output).unwrap();
    let facts = done.result.facts();
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].command, "gpu-query --csv");
    assert_eq!(facts[0].output, "Some GPU, 550.1\n8 GiB");
}
