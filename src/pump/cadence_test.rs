use super::*;
use chrono::TimeZone;

fn scan(name: &str, kind: Option<&str>, status: &str, cadence: Option<&str>) -> RoleScan {
    RoleScan {
        name: String::from(name),
        kind: kind.map(String::from),
        status: String::from(status),
        read_only: false,
        cadence: cadence.map(String::from),
        pool: None,
        open_wants: 0,
        open_needs: 0,
        has_charter: true,
        schedule_state: String::from("never"),
        last_signal_at: None,
        next_due_at: None,
    }
}

fn run(ended_at: Option<u64>, retry_at: Option<u64>) -> RoleRun {
    RoleRun {
        job: String::from("cadence-periodic-1"),
        task: String::from("t"),
        branch: None,
        read_only: true,
        started_at: 1_000,
        ended_at,
        outcome: Some(String::from(if ended_at.is_some() {
            "done"
        } else {
            "running"
        })),
        attempt: 1,
        retry_at,
    }
}

#[test]
fn core_state_round_trips_through_extra() {
    let empty = State::default();
    assert_eq!(state(&empty), CadenceState::default());

    let mut cadence = CadenceState {
        scanned_at: 5_000,
        ..CadenceState::default()
    };
    cadence.scans.insert(
        String::from("head-1"),
        scan("head-1", Some("head"), "active", Some("4h")),
    );
    cadence
        .runs
        .insert(String::from("head-1"), run(Some(4_900), None));

    let mut st = State::default();
    set_state(&mut st, &cadence);
    assert!(st.extra.contains_key(EXTRA_KEY));
    assert_eq!(state(&st), cadence);

    st.extra
        .insert(String::from(EXTRA_KEY), serde_json::json!(7));
    assert_eq!(state(&st), CadenceState::default());
}

#[test]
fn core_backoff_secs_doubles_and_caps() {
    assert_eq!(backoff_secs(0), 60);
    assert_eq!(backoff_secs(1), 60);
    assert_eq!(backoff_secs(2), 120);
    assert_eq!(backoff_secs(3), 240);
    assert_eq!(backoff_secs(6), 1_920);
    assert_eq!(backoff_secs(99), 1_920);
}

#[test]
fn cadence_eligibility_uses_schedule_and_role_state_without_a_kind_gate() {
    for kind in ["head", "mind", "digest", "auditor", "hand", "merge"] {
        let role = scan("periodic", Some(kind), "active", Some("4h"));
        assert!(launchable(&role, None, 1_000), "kind {kind}");
    }

    let no_schedule = scan("unscheduled", Some("mind"), "active", None);
    assert!(!launchable(&no_schedule, None, 1_000));

    let mut not_due = scan("not-due", Some("digest"), "active", Some("24h"));
    not_due.schedule_state = String::from("ok");
    assert!(!launchable(&not_due, None, 1_000));

    let mut no_charter = scan("unconfigured", Some("mind"), "active", Some("15m"));
    no_charter.has_charter = false;
    assert!(!launchable(&no_charter, None, 1_000));

    let mut capped = scan("capped", Some("head"), "active", Some("24h"));
    capped.open_wants = MAX_OPEN_WANTS;
    assert!(!launchable(&capped, None, 1_000));

    let mut needs_capped = scan("needs-capped", Some("head"), "active", Some("24h"));
    needs_capped.open_needs = MAX_OPEN_NEEDS;
    assert!(!launchable(&needs_capped, None, 1_000));

    for status in DISABLED_STATUSES {
        let role = scan("disabled", Some("mind"), status, Some("15m"));
        assert!(!launchable(&role, None, 1_000), "status {status}");
    }

    assert!(!launchable(
        &scan("inflight", Some("mind"), "active", Some("15m")),
        Some(&run(None, None)),
        1_000
    ));
    assert!(!launchable(
        &scan("backoff", Some("mind"), "active", Some("15m")),
        Some(&run(Some(900), Some(1_001))),
        1_000
    ));
    assert!(launchable(
        &scan("retry-due", Some("mind"), "active", Some("15m")),
        Some(&run(Some(900), Some(1_000))),
        1_000
    ));

    let mut successful = run(Some(0), None);
    successful.outcome = Some(String::from("success"));
    assert!(!launchable(
        &scan("success-wait", Some("mind"), "active", Some("15m")),
        Some(&successful),
        989
    ));
    assert!(launchable(
        &scan("success-due", Some("mind"), "active", Some("15m")),
        Some(&successful),
        990
    ));
}

#[test]
fn core_next_due_uses_cadence_grace_and_signal_age() {
    let now = chrono::Utc.timestamp_opt(1_000, 0).single().unwrap();
    let never = crate::role_schedule::evaluate(Some("100s"), None, now);
    assert_eq!(next_due_at(&never, 1_000), Some(1_110));

    let signal = crate::role_schedule::LastSignal {
        at: now - chrono::Duration::seconds(120),
        handle: String::from("m"),
        local_role: String::from("memos"),
    };
    let due = crate::role_schedule::evaluate(Some("100s"), Some(&signal), now);
    assert_eq!(next_due_at(&due, 1_000), Some(1_000));
}

#[test]
fn investigation_need_links_to_its_source_want() {
    assert_eq!(
        investigations::target("investigate\nfor: <want-1234>\n"),
        Some(String::from("want-1234"))
    );
    assert_eq!(
        investigations::target("for: `want-1234`"),
        Some(String::from("want-1234"))
    );
    assert_eq!(investigations::target("investigate only"), None);
}

#[test]
fn head_ceo_charter_can_override_the_default_investigation_round_cap() {
    let dir = tempfile::tempdir().unwrap();
    let mut mailspace = crate::mailspace::Mailspace::init(Some(dir.path())).unwrap();
    mailspace.add_identity("head-ceo").unwrap();
    assert_eq!(investigations::round_limit(&mailspace), 2);

    mailspace
        .set_charter("head-ceo", "investigation-rounds: 4\n")
        .unwrap();
    assert_eq!(investigations::round_limit(&mailspace), 4);
}

#[test]
fn investigation_report_must_be_a_recent_auditor_reply_on_the_want() {
    let dir = tempfile::tempdir().unwrap();
    let mut mailspace = crate::mailspace::Mailspace::init(Some(dir.path())).unwrap();
    for role in ["head-cto", "head-ceo", "auditor"] {
        mailspace.add_identity(role).unwrap();
    }
    let domain = mailspace.address_domain();
    // This places the root after the later same-day auditor reply in sorted
    // thread output, independently of its content hash.
    let future_date =
        (chrono::Utc::now() + chrono::Duration::days(1)).format("%a, %d %b %Y %H:%M:%S +0000");
    let want_letter = format!(
        "From: {}\r\nTo: {}\r\nSubject: review this issue\r\n\
         Message-ID: <investigation-want@{domain}>\r\nDate: {future_date}\r\n\
         X-Vivi-Kind: want\r\n\r\ntriage requested\r\n",
        mailspace.address_for("head-cto"),
        mailspace.address_for("head-ceo"),
    );
    let want = mailspace
        .deliver_raw(want_letter.as_bytes(), "wants")
        .unwrap()
        .delivered[0]
        .handle
        .clone();
    let started_at = chrono::Utc::now()
        .timestamp()
        .cast_unsigned()
        .saturating_sub(1);
    let run = investigation_run(&want, started_at);
    assert!(!investigations::report_landed(&mailspace, &run));
    mailspace
        .reply(
            &want,
            "auditor",
            vec![String::from("head-ceo")],
            Vec::new(),
            Some(String::from("triage report")),
            String::from("The issue is already fixed."),
        )
        .unwrap();
    let thread = mailspace
        .thread(&want, false, usize::MAX, usize::MAX)
        .unwrap();
    let root_index = thread
        .iter()
        .position(|message| message.handle == want)
        .unwrap();
    let reply_index = thread
        .iter()
        .position(|message| message.parent_content_id.is_some())
        .unwrap();
    assert!(reply_index < root_index, "thread={thread:?}");
    assert_eq!(
        thread[reply_index].parent_content_id.as_deref(),
        Some(thread[root_index].content_id.as_str())
    );
    assert!(
        investigations::report_landed(&mailspace, &run),
        "thread={:?}; auditor={}; head-ceo={}; started_at={}",
        thread,
        mailspace.address_for("auditor"),
        mailspace.address_for("head-ceo"),
        run.started_at
    );
}

fn investigation_run(want: &str, started_at: u64) -> InvestigationRun {
    InvestigationRun {
        need: String::from("need1"),
        want: want.to_string(),
        auditor: String::from("auditor"),
        job: String::from("job1"),
        task: String::from("task1"),
        round: 1,
        attempt: 1,
        started_at,
        ended_at: None,
        outcome: None,
        retry_at: None,
    }
}

fn head_role(name: &str) -> RoleView {
    RoleView {
        name: String::from(name),
        address: String::new(),
        aliases: Vec::new(),
        kind: Some(String::from("head")),
        labels: Vec::new(),
        status: String::from("active"),
        harness: None,
        provider: None,
        model: None,
        thinking: None,
        pid: None,
        host: None,
        cadence: None,
        pool: None,
        read_only: None,
        models: Vec::new(),
        charter: String::new(),
        has_charter: true,
    }
}

#[test]
fn head_ceo_cadence_text_files_research_needs_and_asks_for_the_envelope() {
    let text = cadence_run_text(&head_role("head-ceo"), 0);
    assert!(text.contains("need send --mark research"));
    assert!(text.contains("for: <want-handle>"));
    assert!(text.contains("before `vivi pump send`"));
    assert!(text.contains("`reviewed` or `no-op`"));
    assert!(text.contains("promoted"));
    assert!(text.contains("Do not implement work."));
    assert!(!text.contains("investigat"));
    assert!(!text.contains("investigate"));
}

#[test]
fn other_head_cadence_text_has_no_ceo_instructions() {
    let text = cadence_run_text(&head_role("head-cto"), 0);
    assert!(text.contains("file findings as wants only"));
    assert!(!text.contains("research need"));
}
