use super::*;
use chrono::TimeZone;

use crate::letter::fixture::{need_body_with, want_body};
use crate::letter::work::NeedFront;
use crate::mailspace::{RoleUpdate, SendRequest};

fn scan(name: &str, kind: Option<&str>, status: &str, cadence: Option<&str>) -> RoleScan {
    RoleScan {
        name: String::from(name),
        kind: kind.map(String::from),
        status: String::from(status),
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
    assert!(text.contains(
        "need send --mark research --explores <want-handle> --done-when \"<what the research must answer>\""
    ));
    assert!(text.contains("before `vivi pump send`"));
    assert!(text.contains("`reviewed` or `no-op`"));
    assert!(text.contains("served"));
    assert!(text.contains("--serves <want-handle>"));
    assert!(!text.contains("romot"));
    assert!(text.contains("Do not implement work."));
    assert!(text.contains("names in `explores`"));
    assert!(text.contains("at most two"));
    assert!(!text.contains("need-12"));
    assert!(!text.contains("want-3"));
    assert!(!text.contains("investigat"));
    assert!(!text.contains("investigate"));
}

#[test]
fn other_head_cadence_text_has_no_ceo_instructions() {
    let text = cadence_run_text(&head_role("head-cto"), 0);
    assert!(text.contains("file findings as wants only"));
    assert!(!text.contains("research need"));
}

#[test]
fn cadence_spec_is_read_only_without_a_branch_for_writable_roles() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, _host) =
        crate::pump::host::test_ctx(dir.path(), crate::pump::config::PumpConfig::default());
    for (kind, role_name) in [("hand", "hand-writer"), ("head", "head-writer")] {
        let mut role = head_role(role_name);
        role.kind = Some(String::from(kind));
        role.cadence = Some(String::from("4h"));
        role.read_only = Some(false);
        let scan = scan(role_name, Some(kind), "active", Some("4h"));
        let spec = finish_cadence_spec(
            &ctx,
            &role,
            &scan,
            1,
            1_000,
            String::from("cadence-x-1"),
            String::from("cadence"),
            String::from("subject"),
            String::from("body"),
        )
        .unwrap();
        assert!(spec.read_only, "{kind}");
        assert!(spec.branch.is_none(), "{kind}");
        assert!(spec.target.is_none(), "{kind}");
        assert_eq!(
            spec.from_ref.as_deref(),
            Some(ctx.cfg.main.as_str()),
            "{kind}"
        );
    }
}

/// One cadence `JobSpec` for the direct construction tests.
fn direct_spec(ctx: &Ctx, kind: &str) -> JobSpec {
    let mut role = head_role("periodic");
    role.kind = Some(String::from(kind));
    role.cadence = Some(String::from("4h"));
    let scan = scan("periodic", Some(kind), "active", Some("4h"));
    job_spec(ctx, &role, &scan, 1, 1_000).unwrap()
}

#[test]
fn cadence_uses_effective_default_target_main() {
    // A sole named target is the implicit default for both job kinds; its own
    // main, not the workspace main, is the start point.
    let dir = tempfile::tempdir().unwrap();
    let cfg = PumpConfig {
        targets: BTreeMap::from([(
            String::from("vivi"),
            crate::pump::config::PumpTarget {
                main: String::from("vivi/main"),
                ..crate::pump::config::PumpTarget::default()
            },
        )]),
        ..PumpConfig::default()
    };
    let (ctx, _host) = crate::pump::host::test_ctx(dir.path(), cfg);
    for kind in ["hand", "digest"] {
        let spec = direct_spec(&ctx, kind);
        assert_eq!(spec.target.as_deref(), Some("vivi"), "{kind}");
        assert_eq!(spec.from_ref.as_deref(), Some("vivi/main"), "{kind}");
        assert!(spec.read_only, "{kind}");
        assert!(spec.branch.is_none(), "{kind}");
        assert!(spec.extra_targets.is_empty(), "{kind}");
        assert_eq!(spec.budget, ctx.cfg.dispatch.budget, "{kind}");
        assert_eq!(spec.payload["cadence"], "4h", "{kind}");
        assert_eq!(spec.payload["scheduled_at"], 1_000, "{kind}");
        assert_eq!(spec.payload["attempt"], 1, "{kind}");
        assert_eq!(spec.payload["open_wants"], 0, "{kind}");
        assert_eq!(spec.payload["open_needs"], 0, "{kind}");
    }

    // An explicit `default` target wins beside another named target.
    let dir = tempfile::tempdir().unwrap();
    let target = |main: &str| crate::pump::config::PumpTarget {
        main: String::from(main),
        ..crate::pump::config::PumpTarget::default()
    };
    let cfg = PumpConfig {
        targets: BTreeMap::from([
            (String::from("default"), target("default/main")),
            (String::from("other"), target("other/main")),
        ]),
        ..PumpConfig::default()
    };
    let (ctx, _host) = crate::pump::host::test_ctx(dir.path(), cfg);
    let spec = direct_spec(&ctx, "hand");
    assert_eq!(spec.target.as_deref(), Some("default"));
    assert_eq!(spec.from_ref.as_deref(), Some("default/main"));
    assert!(spec.read_only);
    assert!(spec.branch.is_none());
    assert!(spec.extra_targets.is_empty());

    // Legacy config with no named targets keeps target None and starts from
    // the workspace main, whatever it is named.
    let dir = tempfile::tempdir().unwrap();
    let cfg = PumpConfig {
        main: String::from("legacy-main"),
        ..PumpConfig::default()
    };
    let (ctx, _host) = crate::pump::host::test_ctx(dir.path(), cfg);
    let spec = direct_spec(&ctx, "hand");
    assert!(spec.target.is_none());
    assert_eq!(spec.from_ref.as_deref(), Some("legacy-main"));
    assert!(spec.read_only);
    assert!(spec.branch.is_none());
    assert!(spec.extra_targets.is_empty());
}

#[test]
fn cadence_without_a_resolvable_default_enqueues_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    let mut mailspace = Mailspace::init(Some(&project)).unwrap();
    for name in ["watch-a", "watch-b"] {
        mailspace
            .add_role(
                name,
                RoleUpdate {
                    kind: Some(Some(String::from("hand"))),
                    cadence: Some(Some(String::from("4h"))),
                    ..RoleUpdate::default()
                },
            )
            .unwrap();
        mailspace.set_charter(name, "do the work").unwrap();
    }
    let target = |main: &str| crate::pump::config::PumpTarget {
        main: String::from(main),
        ..crate::pump::config::PumpTarget::default()
    };
    let cfg = PumpConfig {
        targets: BTreeMap::from([
            (String::from("alpha"), target("alpha/main")),
            (String::from("beta"), target("beta/main")),
        ]),
        ..PumpConfig::default()
    };
    let (ctx, host) = crate::pump::host::test_ctx(dir.path(), cfg);

    // Direct construction is a Config error: two named targets, no default.
    let mut role = head_role("periodic");
    role.kind = Some(String::from("hand"));
    role.cadence = Some(String::from("4h"));
    let scan = scan("periodic", Some("hand"), "active", Some("4h"));
    let error = finish_cadence_spec(
        &ctx,
        &role,
        &scan,
        1,
        1_000,
        String::from("cadence-x-1"),
        String::from("cadence"),
        String::from("subject"),
        String::from("body"),
    )
    .unwrap_err();
    assert!(matches!(error, VivariumError::Config(_)));

    // The real scan logs one failure per due role and enqueues nothing.
    let mut st = State::default();
    prepare_due(&ctx, &mut st);
    assert!(st.jobs.is_empty());
    assert!(state(&st).runs.is_empty());
    let logs = host.logs();
    for name in ["watch-a", "watch-b"] {
        assert!(
            logs.iter()
                .any(|line| line.contains("cadence spec") && line.contains(name)),
            "no failure logged for {name}: {logs:?}"
        );
    }
}

fn space() -> (tempfile::TempDir, Mailspace) {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("mind").unwrap();
    mailspace.add_identity("hand").unwrap();
    (tmp, mailspace)
}

fn send(mailspace: &Mailspace, kind: &str, body: String) -> String {
    mailspace
        .send(SendRequest {
            from: "mind".into(),
            to: vec!["hand".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: format!("{kind} subject"),
            body,
            role: format!("{kind}s"),
            kind: Some(kind.into()),
            reply_to: None,
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle
}

fn want(mailspace: &Mailspace) -> String {
    send(mailspace, "want", want_body("q", "p"))
}

fn serving_need(mailspace: &Mailspace, want: &str) -> String {
    let mut front = NeedFront::new(vec!["done".to_string()]);
    front.serves = Some(want.to_string());
    send(mailspace, "need", need_body_with(&front, "prose"))
}

fn close(mailspace: &Mailspace, handle: &str) {
    mailspace
        .move_item("mind", handle, "done", None, "close", None)
        .unwrap();
}

#[test]
fn open_want_count_skips_wants_whose_serving_needs_are_all_done() {
    let (_tmp, mailspace) = space();
    let open = want(&mailspace);
    let served = want(&mailspace);
    let plain = want(&mailspace);
    serving_need(&mailspace, &open);
    let need = serving_need(&mailspace, &served);
    assert_eq!(open_want_count(&mailspace, "mind").unwrap(), 3);

    close(&mailspace, &need);
    assert_eq!(open_want_count(&mailspace, "mind").unwrap(), 2);

    close(&mailspace, &plain);
    assert_eq!(open_want_count(&mailspace, "mind").unwrap(), 1);
}
