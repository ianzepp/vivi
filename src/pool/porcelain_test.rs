use super::*;
use crate::pool::doctor::Status;
use crate::pool::slot::SlotStatus;

fn round_line(task: &str, state: &'static str, detail: &str) -> RoundLine {
    RoundLine {
        task: task.into(),
        pool: "p".into(),
        slot: "p-slot-2".into(),
        state,
        detail: detail.into(),
    }
}

fn outcome() -> SlotOutcome {
    SlotOutcome {
        pool: "p".into(),
        name: "p-slot-1".into(),
        id: "m1".into(),
        state: "stopped".into(),
        exit_code: Some(0),
        seconds: 42,
        timed_out: false,
        error: None,
    }
}

#[test]
fn a_plain_word_is_bare_and_anything_else_is_quoted() {
    assert_eq!(quote("finished"), "finished");
    assert_eq!(quote("a b"), r#""a b""#);
    assert_eq!(quote(""), r#""""#);
    assert_eq!(
        quote("-"),
        r#""-""#,
        "a literal dash is not an absent field"
    );
    assert_eq!(quote(r#"say "hi""#), r#""say \"hi\"""#);
    assert_eq!(quote(r"a\b"), r#""a\\b""#);
    assert_eq!(quote("two\nlines\ttab"), r#""two\nlines\ttab""#);
    assert_eq!(quote("bell\u{7}"), r#""bell\u{7}""#);
}

#[test]
fn dispatched_line() {
    let made = DispatchRecord {
        target: None,
        repository: None,
        pool: "p".into(),
        need: None,
        kind: None,
        slot: "p-slot-1".into(),
        machine: "m1".into(),
        job: "0123456789ab".into(),
        model: "sonnet".into(),
        from: "main".into(),
        branch: "work/x".into(),
        start: "a".repeat(40),
        with: Vec::new(),
        budget: 1800,
        at: 0,
        read_only: false,
        hardware: String::new(),
        abandoned: 0,
        abandoned_cost_micros: 0,
    };
    assert_eq!(
        dispatched("3f2a", &made),
        format!(
            "dispatched 3f2a p p-slot-1 0123456789ab sonnet main {} work/x 1800 write 0 -",
            "a".repeat(40)
        )
    );
}

#[test]
fn a_read_only_dispatch_has_no_branch_and_says_so() {
    let made = DispatchRecord {
        target: None,
        repository: None,
        pool: "p".into(),
        need: None,
        kind: None,
        slot: "p-slot-1".into(),
        machine: "m1".into(),
        job: "0123456789ab".into(),
        model: "sonnet".into(),
        from: "main".into(),
        branch: String::new(),
        start: "a".repeat(40),
        with: Vec::new(),
        budget: 600,
        at: 0,
        read_only: true,
        hardware: String::new(),
        abandoned: 0,
        abandoned_cost_micros: 0,
    };
    assert_eq!(
        dispatched("3f2a", &made),
        format!(
            "dispatched 3f2a p p-slot-1 0123456789ab sonnet main {} - 600 read-only 0 -",
            "a".repeat(40)
        )
    );
}

#[test]
fn read_only_results_are_collected_as_their_own_words() {
    let unchanged = round_line("3f2a", "finished", "read_only_unchanged");
    assert_eq!(
        collected(&unchanged),
        "collected 3f2a p p-slot-2 finished read_only_unchanged"
    );
    let discarded = round_line("3f2a", "finished", "read_only_discarded 3");
    assert_eq!(
        collected(&discarded),
        r#"collected 3f2a p p-slot-2 finished "read_only_discarded 3""#
    );
}

#[test]
fn create_lines() {
    assert_eq!(
        created(&outcome()),
        "created p p-slot-1 m1 stopped 42 0 seeded -"
    );
    let mut late = outcome();
    late.timed_out = true;
    late.state = "started".into();
    late.exit_code = None;
    assert_eq!(
        created(&late),
        "created p p-slot-1 m1 started 42 - timeout -"
    );
    let mut bad = outcome();
    bad.exit_code = Some(3);
    assert_eq!(created(&bad), "created p p-slot-1 m1 stopped 42 3 failed -");
    let mut unmarked = outcome();
    unmarked.error = Some("could not seal: no".into());
    assert_eq!(
        created(&unmarked),
        r#"created p p-slot-1 m1 stopped 42 0 unmarked "could not seal: no""#
    );
    assert_eq!(exists("lanes", 2), "exists lanes 2");
    assert_eq!(
        home("lanes", "created app lanes-1 in organization personal"),
        r#"home lanes "created app lanes-1 in organization personal""#
    );
}

#[test]
fn status_lines() {
    let mut busy = SlotStatus {
        pool: "p".into(),
        name: "p-slot-1".into(),
        id: "m1".into(),
        state: "busy".into(),
        provider_state: "started".into(),
        index: Some(1),
        base: Some("abc".into()),
        base_ref: None,
        task: Some("3f2a".into()),
        claimed_at: None,
        read_only: None,
        running_secs: Some(95),
        budget_secs: Some(1800),
        created_at: "2026-10-11T10:00:00Z".into(),
        region: "iad".into(),
        arch: Some("arm64".into()),
        space_mark: None,
    };
    assert_eq!(
        slot(&busy),
        "slot p p-slot-1 m1 busy 3f2a 95 1800 abc arm64 2026-10-11T10:00:00Z -"
    );
    busy.state = "free".into();
    busy.task = None;
    busy.running_secs = None;
    busy.budget_secs = None;
    busy.base = None;
    busy.arch = None;
    assert_eq!(
        slot(&busy),
        "slot p p-slot-1 m1 free - - - - - 2026-10-11T10:00:00Z -"
    );
    busy.read_only = Some(false);
    assert!(slot(&busy).ends_with("2026-10-11T10:00:00Z write"));
    busy.read_only = Some(true);
    assert!(slot(&busy).ends_with("2026-10-11T10:00:00Z read-only"));
}

#[test]
fn capacity_line() {
    assert_eq!(capacity("studio", 2), "capacity studio 2");
    assert_eq!(capacity("studio", 0), "capacity studio 0");
}

#[test]
fn wait_and_reconcile_lines() {
    let done = round_line("3f2a", "finished", "pushed work/x 0123456789");
    assert_eq!(
        collected(&done),
        r#"collected 3f2a p p-slot-2 finished "pushed work/x 0123456789""#
    );
    assert_eq!(
        reconciled(&round_line("3f2a", "running", "150s of 1800s")),
        r#"reconciled 3f2a p p-slot-2 running "150s of 1800s""#
    );
    assert_eq!(
        reconciled(&round_line(
            "-",
            "orphan-stopped",
            "stopped (running with no task)"
        )),
        r#"reconciled - p p-slot-2 orphan-stopped "stopped (running with no task)""#
    );
    assert_eq!(nothing_open(), "nothing-open");
}

#[test]
fn issue_lines() {
    let mut seen = Issue {
        pool: Some("p".into()),
        kind: "poll-error",
        subject: Some("3f2a".into()),
        message: "Fly unreachable".into(),
        first_seen: 1_760_000_000,
        cleared: Some(1_760_000_090),
    };
    assert_eq!(
        issue(&seen),
        r#"issue p poll-error 3f2a "Fly unreachable" 1760000000 1760000090"#
    );
    seen.cleared = None;
    seen.subject = None;
    assert_eq!(
        issue(&seen),
        r#"issue p poll-error - "Fly unreachable" 1760000000 -"#
    );
    seen.pool = None;
    assert_eq!(
        issue(&seen),
        r#"issue - poll-error - "Fly unreachable" 1760000000 -"#
    );
}

#[test]
fn doctor_lines() {
    let mut ok = Check::ok("Provider (Docker)", "daemon", "reachable").in_pool("p");
    assert_eq!(
        check(&ok),
        r#"check p ok "Provider (Docker)" daemon reachable - -"#
    );
    ok = Check::fail(
        "Live test: Vivi",
        "dispatch two at once",
        "expected two",
        "run it again",
    )
    .in_pool("p")
    .timed(std::time::Duration::from_millis(5120));
    assert_eq!(ok.status, Status::Fail);
    assert_eq!(
        check(&ok),
        r#"check p fail "Live test: Vivi" "dispatch two at once" "expected two" "run it again" 5120"#
    );
    let counts = Counts {
        ok: 11,
        warn: 1,
        fail: 0,
        skipped: 2,
    };
    assert_eq!(summary("p", &counts), "summary p 11 1 0 2");
}

#[test]
fn a_dispatch_with_no_recorded_pool_reads_as_an_absent_field() {
    let mut line = round_line("3f2a", "error", "no pool");
    line.pool = String::new();
    assert_eq!(
        collected(&line),
        r#"collected 3f2a - p-slot-2 error "no pool""#
    );
}

#[test]
fn scale_lines() {
    let mut s = SlotStatus {
        pool: "p".into(),
        name: "ms-p-slot-4".into(),
        id: "m4".into(),
        state: "busy".into(),
        provider_state: "started".into(),
        index: Some(4),
        base: None,
        base_ref: None,
        task: Some("3f2a".into()),
        claimed_at: None,
        read_only: None,
        running_secs: None,
        budget_secs: None,
        created_at: String::new(),
        region: String::new(),
        arch: None,
        space_mark: None,
    };
    assert_eq!(skipped(&s), "skipped p ms-p-slot-4 m4 busy 3f2a");
    s.task = None;
    s.state = "seeding".into();
    assert_eq!(skipped(&s), "skipped p ms-p-slot-4 m4 seeding -");
    assert_eq!(destroyed(&s), "destroyed p ms-p-slot-4 m4");
    assert_eq!(scaled("p", 0, 1), "scaled p 0 1");
}

#[test]
fn a_dispatch_that_gave_up_on_pods_ends_with_their_count_and_approximate_cost() {
    let made = DispatchRecord {
        target: None,
        repository: None,
        pool: "p".into(),
        need: None,
        kind: None,
        slot: "p-slot-1".into(),
        machine: "m1".into(),
        job: "0123456789ab".into(),
        model: "sonnet".into(),
        from: "main".into(),
        branch: "work/x".into(),
        start: "a".repeat(40),
        with: Vec::new(),
        budget: 1800,
        at: 0,
        read_only: false,
        hardware: String::new(),
        abandoned: 2,
        abandoned_cost_micros: 123_456,
    };
    assert_eq!(
        dispatched("3f2a", &made),
        format!(
            "dispatched 3f2a p p-slot-1 0123456789ab sonnet main {} work/x 1800 write 2 0.12",
            "a".repeat(40)
        )
    );
    let unpriced = DispatchRecord {
        abandoned_cost_micros: 0,
        ..made
    };
    assert!(dispatched("3f2a", &unpriced).ends_with(" write 2 -"));
}

#[test]
fn clean_lines_name_kind_bytes_and_dry_run() {
    let action = crate::pool::clean::CleanAction {
        pool: "lanes".into(),
        slot: "slot-1".into(),
        kind: crate::pool::clean::ActionKind::Build,
        bytes: 100,
        path: "/p/repo/target".into(),
        note: "over 2x baseline".into(),
    };
    assert_eq!(
        cleaned(false, &action),
        r#"cleaned lanes slot-1 build 100 /p/repo/target "over 2x baseline""#
    );
    assert!(cleaned(true, &action).starts_with("would "));
    let skip = crate::pool::clean::CleanSkip {
        pool: "lanes".into(),
        slot: "slot-1".into(),
        kind: crate::pool::clean::ActionKind::Worktree,
        reason: "uncommitted changes".into(),
    };
    assert_eq!(
        clean_skip(&skip),
        r#"skipped lanes slot-1 worktree "uncommitted changes""#
    );
    assert_eq!(clean_summary(2, 1, 100), "summary 2 1 100");
}

#[test]
fn reset_lines_name_the_origin_base() {
    let out = crate::pool::reset::ResetOutcome {
        base_ref: "main".into(),
        sha: "abc123def456".into(),
        why: "default-main",
    };
    assert_eq!(
        reset_line("lanes", "slot-1", &out),
        "reset lanes slot-1 main abc123def456 default-main"
    );
    assert_eq!(
        reset_skip("lanes", "slot-1", "next", "origin/next is missing"),
        r#"skipped lanes slot-1 reset "next: origin/next is missing""#
    );
    let named = SlotStatus {
        pool: "p".into(),
        name: "p-slot-1".into(),
        id: "m1".into(),
        state: "free".into(),
        provider_state: "stopped".into(),
        index: Some(1),
        base: Some("abc123def456".into()),
        base_ref: Some("main".into()),
        task: None,
        claimed_at: None,
        read_only: None,
        running_secs: None,
        budget_secs: None,
        created_at: "2026-10-11T10:00:00Z".into(),
        region: "iad".into(),
        arch: None,
        space_mark: None,
    };
    assert_eq!(
        slot(&named),
        "slot p p-slot-1 m1 free - - - main@abc123def456 - 2026-10-11T10:00:00Z -"
    );
}
