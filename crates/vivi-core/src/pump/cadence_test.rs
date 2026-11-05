use super::*;
use chrono::TimeZone;

use crate::mailspace::SendRequest;
use crate::pump::config::{PumpConfig, PumpTarget};
use crate::pump::jobs::JobStatus;
use vivi_mail::letter::fixture::{need_body_with, want_body};
use vivi_mail::letter::work::NeedFront;

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
        command: None,
        target: None,
        effective_target: None,
        target_main: None,
        target_unavailable: false,
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

    let mut cadence = CadenceState::default();
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
        command: None,
        target: None,
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
        );
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

fn three_targets() -> PumpConfig {
    let mut cfg = PumpConfig::default();
    for (id, main) in [("radix", "trunk"), ("app", "staging"), ("other", "develop")] {
        cfg.targets.insert(
            String::from(id),
            PumpTarget {
                path: String::from(id),
                main: String::from(main),
                ..PumpTarget::default()
            },
        );
    }
    cfg
}

fn target_scan(name: &str, target: Option<&str>, command: Option<&str>) -> RoleScan {
    let mut scan = scan(name, Some("mind"), "active", Some("15m"));
    scan.target = target.map(String::from);
    scan.command = command.map(String::from);
    scan.pool = Some(String::from("check-pool"));
    scan
}

#[test]
fn command_cadence_spec_uses_target_main_without_a_charter() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, _host) = crate::pump::host::test_ctx(dir.path(), three_targets());
    let mut role = head_role("checker");
    role.command = Some(String::from("cargo test --workspace"));
    role.target = Some(String::from("app"));
    let mut scan = target_scan("checker", Some("app"), role.command.as_deref());
    scan.has_charter = false;
    assert!(command::ready(&State::default(), &scan, None, 1_000));

    let mut st = State::default();
    let mut cadence = CadenceState::default();
    command::enqueue(&ctx, &mut st, &mut cadence, &role, &scan, 1_000);

    let job = st.jobs.values().next().expect("one command job");
    assert_eq!(job.command.as_deref(), Some("cargo test --workspace"));
    assert_eq!(job.target.as_deref(), Some("app"));
    assert_eq!(job.from_ref.as_deref(), Some("staging"));
    assert_ne!(job.from_ref.as_deref(), Some(ctx.cfg.main.as_str()));
    assert_eq!(job.pool_candidates, vec![String::from("check-pool")]);
    assert_eq!(job.budget, ctx.cfg.dispatch.budget);
    assert!(job.read_only);
    assert!(job.branch.is_none());
    assert!(job.publish_ref.is_none());
    assert!(job.delete_ref.is_none());
    assert!(!job.allow_default_branch);
    assert_eq!(job.kind, "cadence");
    assert!(job.body.contains("Command: cargo test --workspace"));
    assert!(!job.body.contains("configured role charter"));
    assert!(!job.body.contains("Run your configured"));

    let check = cadence.checks.get("checker").expect("check state");
    let latest = check.latest.as_ref().expect("latest execution");
    assert_eq!(latest.command, "cargo test --workspace");
    assert_eq!(latest.target, "app");
    assert_eq!(latest.main, "staging");
    assert_eq!(latest.job, job.id);
    assert_eq!(
        latest.fingerprint,
        command::fingerprint_of(
            "checker",
            &TargetChoice::Named {
                id: String::from("app"),
                main: String::from("staging"),
            },
            "cargo test --workspace"
        )
    );
}

#[test]
fn model_cadence_with_target_uses_that_targets_main() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, _host) = crate::pump::host::test_ctx(dir.path(), three_targets());
    assert_eq!(ctx.cfg.primary_target(), None);
    let mut role = head_role("reader");
    role.cadence = Some(String::from("15m"));
    role.target = Some(String::from("radix"));
    let spec = job_spec(
        &ctx,
        &role,
        &scan("reader", Some("head"), "active", Some("15m")),
        1,
        1_000,
    );
    assert_eq!(spec.target.as_deref(), Some("radix"));
    assert_eq!(spec.from_ref.as_deref(), Some("trunk"));
    assert!(spec.command.is_none());
    assert!(spec.read_only);
}

#[test]
fn model_cadence_without_command_is_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, _host) = crate::pump::host::test_ctx(dir.path(), three_targets());
    let mut role = head_role("reader");
    role.cadence = Some(String::from("15m"));
    let spec = job_spec(
        &ctx,
        &role,
        &scan("reader", Some("head"), "active", Some("15m")),
        1,
        1_000,
    );
    assert_eq!(spec.target, None);
    assert_eq!(spec.from_ref.as_deref(), Some(ctx.cfg.main.as_str()));
    assert!(spec.command.is_none());
    assert!(spec.body.contains("Run your configured role charter"));
}

#[test]
fn command_eligibility_ignores_caps_and_outbound_mail_but_not_status() {
    let st = State::default();
    let mut role = target_scan("checker", None, Some("make check"));
    role.has_charter = false;
    role.schedule_state = String::from("ok");
    role.last_signal_at = Some(String::from("2000-01-01T00:00:00+00:00"));
    role.open_wants = MAX_OPEN_WANTS;
    role.open_needs = MAX_OPEN_NEEDS;
    assert!(command::ready(&st, &role, None, 1_000));

    for status in DISABLED_STATUSES {
        let mut disabled = role.clone();
        disabled.status = String::from(status);
        assert!(
            !command::ready(&st, &disabled, None, 1_000),
            "status {status}"
        );
    }

    let mut no_cadence = role.clone();
    no_cadence.cadence = None;
    assert!(!command::ready(&st, &no_cadence, None, 1_000));

    let mut no_command = role.clone();
    no_command.command = Some(String::from("   "));
    assert!(!command::ready(&st, &no_command, None, 1_000));

    let mut unavailable = role.clone();
    unavailable.target_unavailable = true;
    assert!(!command::ready(&st, &unavailable, None, 1_000));
}

#[test]
fn command_single_flight_blocks_across_configuration_changes() {
    let dir = tempfile::tempdir().unwrap();
    let (ctx, _host) = crate::pump::host::test_ctx(dir.path(), PumpConfig::default());
    let mut role = head_role("checker");
    role.command = Some(String::from("make check"));
    let scan = target_scan("checker", None, Some("make check"));
    let mut st = State::default();
    let mut cadence = CadenceState::default();
    command::enqueue(&ctx, &mut st, &mut cadence, &role, &scan, 1_000);

    let changed = target_scan("checker", None, Some("make lint"));
    let check = cadence.checks.get("checker").cloned().unwrap();
    assert!(!command::ready(&st, &changed, Some(&check), 100_000));

    let job_id = check.latest.as_ref().unwrap().job.clone();
    st.jobs.get_mut(&job_id).unwrap().status = JobStatus::Finished;
    assert!(!command::ready(&st, &changed, Some(&check), 100_000));
    st.jobs.get_mut(&job_id).unwrap().applied_at = Some(2_000);
    assert!(command::ready(&st, &changed, Some(&check), 100_000));
}

#[test]
fn command_intervals_use_completion_time_and_environment_backoff() {
    let st = State::default();
    let scan = target_scan("checker", None, Some("make check"));
    let mut check = CommandCheck {
        fingerprint: String::from("f"),
        last_green_commit: Some(String::from("abc")),
        red_need: Some(String::from("need-1")),
        latest: None,
    };
    // First run is immediately due.
    assert!(command::ready(&st, &scan, Some(&check), 0));

    check.latest = Some(CommandExecution {
        job: String::from("gone"),
        fingerprint: String::from("f"),
        command: String::from("make check"),
        role: String::from("checker"),
        attempt: 1,
        started_at: 1_000,
        ended_at: Some(1_000),
        outcome: Some(String::from("green")),
        ..CommandExecution::default()
    });
    assert!(!command::ready(&st, &scan, Some(&check), 1_899));
    assert!(command::ready(&st, &scan, Some(&check), 1_900));

    let mut red = check.clone();
    if let Some(latest) = red.latest.as_mut() {
        latest.outcome = Some(String::from("red"));
        latest.ended_at = Some(1_000);
        latest.retry_at = Some(1_000 + backoff_secs(1));
    }
    assert!(!command::ready(&st, &scan, Some(&red), 1_059));
    assert!(command::ready(&st, &scan, Some(&red), 1_060));
}

#[test]
fn old_cadence_state_deserializes_with_defaulted_command_state() {
    let old = serde_json::json!({
        "runs": {"mind": {"job": "cadence-mind-1", "task": "t", "started_at": 5, "attempt": 2}},
        "scans": {"mind": {"name": "mind", "cadence": "15m", "status": "active"}}
    });
    let cadence: CadenceState = serde_json::from_value(old).unwrap();
    assert!(cadence.checks.is_empty());
    let scan = cadence.scans.get("mind").unwrap();
    assert!(scan.command.is_none());
    assert!(scan.target.is_none());
    assert!(!scan.target_unavailable);
    assert_eq!(cadence.runs.get("mind").unwrap().attempt, 2);
}

#[test]
fn command_check_fingerprint_is_stable_and_configuration_sensitive() {
    let base = fingerprint("checker", "app", "staging", "make check");
    assert_eq!(
        base,
        fingerprint(" Checker ", " app ", "staging ", "make check")
    );
    assert_eq!(base.len(), 64);
    assert_ne!(
        base,
        fingerprint("checker", "radix", "staging", "make check")
    );
    assert_ne!(base, fingerprint("checker", "app", "trunk", "make check"));
    assert_ne!(base, fingerprint("checker", "app", "staging", "make lint"));

    let mut check = CommandCheck::default();
    check.record(CommandExecution {
        fingerprint: base.clone(),
        ..CommandExecution::default()
    });
    let mut cadence = CadenceState::default();
    cadence
        .checks
        .insert(String::from("checker"), check.clone());
    let mut st = State::default();
    set_state(&mut st, &cadence);
    assert_eq!(state(&st), cadence);
    assert_eq!(state(&st).checks.get("checker"), Some(&check));
}
