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
        ..CommandCheck::default()
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

fn full_sha(fill: char) -> String {
    fill.to_string().repeat(40)
}

/// One command-check bench: a pump context with three targets and a fake
/// host, a temporary mailspace the pool notes land in, and the state a tick
/// would carry.
struct Bench {
    _tmp: tempfile::TempDir,
    ctx: Ctx,
    host: std::sync::Arc<crate::pump::host::FakeHost>,
    space: Mailspace,
    task: String,
    st: State,
    cadence: CadenceState,
}

fn command_role(name: &str, command: &str, target: Option<&str>) -> RoleView {
    let mut role = head_role(name);
    role.kind = Some(String::from("mind"));
    role.cadence = Some(String::from("15m"));
    role.command = Some(String::from(command));
    role.target = target.map(String::from);
    role
}

fn bench_task(space: &Mailspace) -> String {
    space
        .send(SendRequest {
            from: "checker".into(),
            to: vec!["pump".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: String::from("command check"),
            body: vivi_mail::letter::fixture::task_body_with(
                &vivi_mail::letter::work::TaskFront::default(),
                "run the check",
            ),
            role: String::from("tasks"),
            kind: Some(String::from("task")),
            reply_to: None,
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle
}

impl Bench {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let (ctx, host) = crate::pump::host::test_ctx(tmp.path(), three_targets());
        let mut space = Mailspace::init(Some(&tmp.path().join("project"))).unwrap();
        space.add_identity("checker").unwrap();
        space.add_identity("pump").unwrap();
        let task = bench_task(&space);
        Self {
            _tmp: tmp,
            ctx,
            host,
            space,
            task,
            st: State::default(),
            cadence: CadenceState::default(),
        }
    }

    fn scan(&self, target: Option<&str>, command: &str) -> RoleScan {
        let mut scan = target_scan("checker", target, Some(command));
        scan.effective_target = target.map(String::from);
        scan
    }

    /// Enqueue the command job and give it the mailspace task a dispatch
    /// would have created.
    fn launch(&mut self, target: Option<&str>, command: &str, now: u64) -> String {
        let role = command_role("checker", command, target);
        let scan = self.scan(target, command);
        command::enqueue(
            &self.ctx,
            &mut self.st,
            &mut self.cadence,
            &role,
            &scan,
            now,
        );
        let job = self.cadence.checks["checker"].latest.clone().unwrap().job;
        self.st.jobs.get_mut(&job).unwrap().task = Some(self.task.clone());
        job
    }

    /// Mark the job terminal with its pool receipt and, when the runner wrote
    /// one, the collected record carrying the start commit.
    fn finish(
        &mut self,
        job: &str,
        status: JobStatus,
        receipt: &str,
        at: u64,
        start: Option<&str>,
    ) {
        if let Some(start) = start {
            let note =
                serde_json::json!({ "job": "runner-7", "outcome": "finished", "start": start });
            self.space
                .record_pool_note(&self.task, "pool_collected", &note.to_string(), "result")
                .unwrap();
        }
        let job = self.st.jobs.get_mut(job).unwrap();
        job.status = status;
        job.outcome = Some(crate::pump::jobs::JobOutcome {
            state: String::from(if status == JobStatus::Finished {
                "finished"
            } else {
                "lost"
            }),
            detail: String::from("finished"),
            receipt: String::from(receipt),
            at,
        });
    }

    fn collect(&mut self, now: u64) {
        result::collect(&self.ctx, &mut self.st, &mut self.cadence, &self.space, now);
    }

    fn latest(&self) -> CommandExecution {
        self.cadence.checks["checker"].latest.clone().unwrap()
    }

    fn check(&self) -> CommandCheck {
        self.cadence.checks["checker"].clone()
    }

    fn applied(&self, job: &str) -> bool {
        self.st.jobs[job].applied_at.is_some()
    }
}

#[test]
fn command_cadence_skips_last_green_tip() {
    let mut bench = Bench::new();
    let green = full_sha('a');
    let job = bench.launch(Some("app"), "make check", 1_000);
    bench.finish(
        &job,
        JobStatus::Finished,
        "agent exit 0 in 5s",
        2_000,
        Some(&green),
    );
    bench.collect(2_100);

    assert_eq!(bench.latest().outcome.as_deref(), Some("green"));
    assert!(bench.applied(&job), "a green with no report owed applies");
    assert_eq!(
        bench.check().last_green_commit.as_deref(),
        Some(green.as_str())
    );

    // The tip is still the commit this check last passed on: no launch.
    bench
        .host
        .reply("ls-remote", 0, &format!("{green}\trefs/heads/staging\n"));
    let scan = bench.scan(Some("app"), "make check");
    assert!(!result::gate_due(
        &bench.ctx,
        &mut bench.cadence,
        &scan,
        100_000
    ));
    assert_eq!(bench.check().waiting_main.as_deref(), Some(green.as_str()));

    // A moved tip runs.
    bench.host.reply(
        "ls-remote",
        0,
        &format!("{}\trefs/heads/staging\n", full_sha('b')),
    );
    assert!(result::gate_due(
        &bench.ctx,
        &mut bench.cadence,
        &scan,
        100_000
    ));
    assert!(bench.check().waiting_main.is_none());
}

#[test]
fn command_cadence_uses_actual_runner_start() {
    let mut bench = Bench::new();
    let scanned = full_sha('a');
    let tested = full_sha('b');
    bench
        .host
        .reply("ls-remote", 0, &format!("{scanned}\trefs/heads/staging\n"));
    let job = bench.launch(Some("app"), "make check", 1_000);
    assert_eq!(bench.latest().main, "staging");
    bench.finish(
        &job,
        JobStatus::Finished,
        "agent exit 0 in 5s",
        2_000,
        Some(&tested),
    );
    bench.collect(2_100);

    let latest = bench.latest();
    assert_eq!(latest.outcome.as_deref(), Some("green"));
    assert_eq!(latest.exit, Some(0));
    assert_eq!(latest.tested_commit.as_deref(), Some(tested.as_str()));
    assert_eq!(
        bench.check().last_green_commit.as_deref(),
        Some(tested.as_str())
    );

    // A shortened human receipt hash is not execution provenance.
    let mut other = Bench::new();
    let job = other.launch(None, "make check", 1_000);
    other.finish(
        &job,
        JobStatus::Finished,
        "agent exit 0 in 5s",
        2_000,
        Some("fedcba9876"),
    );
    other.collect(2_100);
    assert_eq!(other.latest().outcome.as_deref(), Some("environment"));
    assert!(other.check().last_green_commit.is_none());
    assert!(other.applied(&job));
}

#[test]
fn command_environment_failure_retries_without_a_need() {
    for (label, status, receipt) in [
        ("missing tool", JobStatus::Finished, "agent exit 127 in 1s"),
        ("setup", JobStatus::Finished, "agent exit 126 in 1s"),
        ("no exit", JobStatus::Finished, "the worker said nothing"),
        ("lost", JobStatus::Lost, "agent exit 0 in 1s"),
    ] {
        let mut bench = Bench::new();
        let start = full_sha('c');
        let job = bench.launch(Some("app"), "make check", 1_000);
        bench.finish(&job, status, receipt, 2_000, Some(&start));
        bench.collect(2_100);

        let latest = bench.latest();
        assert_eq!(latest.outcome.as_deref(), Some("environment"), "{label}");
        assert!(latest.awaiting.is_none(), "{label}");
        assert!(bench.applied(&job), "{label}");
        let retry_at = latest.retry_at.expect("a retry is scheduled");
        assert_eq!(retry_at, 2_000 + backoff_secs(1), "{label}");
        assert!(bench.check().last_green_commit.is_none(), "{label}");
        assert!(bench.check().red_need.is_none(), "{label}");
        let exception = bench.check().exception.expect("a cadence exception");
        assert_eq!(exception.kind, result::ENV_EXCEPTION, "{label}");
        assert!(
            bench
                .space
                .list_kind_all(None, "sent", "need")
                .unwrap()
                .is_empty()
        );

        let scan = bench.scan(Some("app"), "make check");
        let check = bench.check();
        assert!(
            !command::ready(&bench.st, &scan, Some(&check), retry_at - 1),
            "{label}"
        );
        assert!(
            command::ready(&bench.st, &scan, Some(&check), retry_at),
            "{label}"
        );
    }
}

#[test]
fn command_check_configuration_change_invalidates_green_skip() {
    let mut bench = Bench::new();
    let green = full_sha('a');
    bench
        .host
        .reply("ls-remote", 0, &format!("{green}\trefs/heads/staging\n"));
    let job = bench.launch(Some("app"), "make check", 1_000);
    bench.finish(
        &job,
        JobStatus::Finished,
        "agent exit 0 in 5s",
        2_000,
        Some(&green),
    );
    bench.collect(2_100);
    assert_eq!(
        bench.check().last_green_commit.as_deref(),
        Some(green.as_str())
    );

    // Same tip, new command: a different check, so it runs.
    let changed = bench.scan(Some("app"), "make lint");
    assert!(result::gate_due(
        &bench.ctx,
        &mut bench.cadence,
        &changed,
        100_000
    ));

    // Same tip, another target main: again a different check.
    bench
        .host
        .reply("ls-remote", 0, &format!("{green}\trefs/heads/trunk\n"));
    let retargeted = bench.scan(Some("radix"), "make check");
    assert!(result::gate_due(
        &bench.ctx,
        &mut bench.cadence,
        &retargeted,
        100_000
    ));

    // Recording the changed execution drops the old green and red need.
    let mut check = bench.check();
    check.red_need = Some(String::from("need-1"));
    check.record(CommandExecution {
        fingerprint: String::from("other"),
        ..CommandExecution::default()
    });
    assert!(check.last_green_commit.is_none());
    assert!(check.red_need.is_none());
}

#[test]
fn command_red_stages_its_report_and_never_becomes_green() {
    let mut bench = Bench::new();
    let start = full_sha('d');
    let job = bench.launch(Some("app"), "make check", 1_000);
    bench.finish(
        &job,
        JobStatus::Finished,
        "agent exit 1 in 5s",
        2_000,
        Some(&start),
    );
    bench.collect(2_100);

    let latest = bench.latest();
    assert_eq!(latest.outcome.as_deref(), Some("red"));
    assert_eq!(latest.exit, Some(1));
    assert_eq!(latest.awaiting.as_deref(), Some(result::AWAITING_RED));
    assert!(!bench.applied(&job), "the red need is still owed");
    assert!(bench.check().last_green_commit.is_none());

    // The staged result keeps the single-flight slot closed.
    let scan = bench.scan(Some("app"), "make check");
    let check = bench.check();
    assert!(!command::ready(&bench.st, &scan, Some(&check), 100_000));
}

#[test]
fn command_green_over_an_open_red_need_awaits_a_recovery_report() {
    let mut bench = Bench::new();
    let start = full_sha('e');
    let choice = TargetChoice::Named {
        id: String::from("app"),
        main: String::from("staging"),
    };
    bench.cadence.checks.insert(
        String::from("checker"),
        CommandCheck {
            fingerprint: command::fingerprint_of("checker", &choice, "make check"),
            red_need: Some(String::from("need-1")),
            ..CommandCheck::default()
        },
    );
    let job = bench.launch(Some("app"), "make check", 1_000);
    bench.finish(
        &job,
        JobStatus::Finished,
        "agent exit 0 in 5s",
        2_000,
        Some(&start),
    );
    bench.collect(2_100);

    let latest = bench.latest();
    assert_eq!(latest.outcome.as_deref(), Some("green"));
    assert_eq!(latest.awaiting.as_deref(), Some(result::AWAITING_RECOVERY));
    assert!(!bench.applied(&job));
    assert!(bench.check().last_green_commit.is_none());
}

#[test]
fn command_evidence_is_not_consumed_without_a_mailspace() {
    let tmp = tempfile::tempdir().unwrap();
    let (ctx, _host) = crate::pump::host::test_ctx(tmp.path(), three_targets());
    let start = full_sha('f');
    let mut st = State::default();
    let mut cadence = CadenceState::default();
    let role = command_role("checker", "make check", Some("app"));
    let scan = target_scan("checker", Some("app"), Some("make check"));
    command::enqueue(&ctx, &mut st, &mut cadence, &role, &scan, 1_000);
    let job = cadence.checks["checker"].latest.clone().unwrap().job;
    let entry = st.jobs.get_mut(&job).unwrap();
    entry.status = JobStatus::Finished;
    entry.outcome = Some(crate::pump::jobs::JobOutcome {
        state: String::from("finished"),
        detail: String::from("finished"),
        receipt: String::from("agent exit 0 in 5s"),
        at: 2_000,
    });
    let entry = start.as_str();
    assert_eq!(result::tested_commit(&[], Some(entry)), None);

    prepare_due(&ctx, &mut st);
    let check = cadence.checks["checker"].latest.clone().unwrap();
    assert!(check.ended_at.is_none(), "nothing was consumed");
    assert!(check.outcome.is_none());
    assert!(st.jobs[&job].applied_at.is_none());
}

#[test]
fn command_target_tip_exception_backs_off_without_a_need() {
    let mut bench = Bench::new();
    bench.host.reply("ls-remote", 128, "fatal: unreachable");
    let scan = bench.scan(Some("app"), "make check");
    assert!(!result::gate_due(
        &bench.ctx,
        &mut bench.cadence,
        &scan,
        1_000
    ));
    let check = bench.check();
    let exception = check.exception.clone().expect("a cadence exception");
    assert_eq!(exception.kind, result::TIP_EXCEPTION);
    assert_eq!(exception.retry_at, Some(1_000 + backoff_secs(1)));
    assert!(bench.st.jobs.is_empty(), "no shell job was launched");
    assert!(
        bench
            .space
            .list_kind_all(None, "sent", "need")
            .unwrap()
            .is_empty()
    );

    // The backoff holds the next attempt back, then allows it once the tip
    // can be read again.
    assert!(!result::gate_due(
        &bench.ctx,
        &mut bench.cadence,
        &scan,
        1_059
    ));
    assert_eq!(bench.check().exception.unwrap().attempt, 1);
    bench.host.reply(
        "ls-remote",
        0,
        &format!("{}\trefs/heads/staging\n", full_sha('b')),
    );
    assert!(result::gate_due(
        &bench.ctx,
        &mut bench.cadence,
        &scan,
        1_060
    ));
    assert!(bench.check().exception.is_none());
}

#[test]
fn command_target_main_absent_is_an_exception_not_a_skip() {
    let mut bench = Bench::new();
    bench.host.reply("ls-remote", 0, "");
    let scan = bench.scan(Some("app"), "make check");
    assert!(!result::gate_due(
        &bench.ctx,
        &mut bench.cadence,
        &scan,
        1_000
    ));
    let exception = bench.check().exception.expect("a cadence exception");
    assert!(exception.reason.contains("absent"), "{}", exception.reason);
    assert!(bench.check().waiting_main.is_none());
}
