//! The planning of `vivi pool dispatch` and the extra refs it resolves:
//! exercised directly, with no slot claimed and nothing launched.

use super::*;
use vivi::letter::work::TaskFront;
use vivi::pool::bands::guard::DispatchGuard;
use vivi::pool::bands::runtime::WaitReason;
use vivi::pool::config::PoolCommitter;

/// A repository with `main` and `review/aaa`; returns the directory and the
/// repository path.
fn repo_with_branch() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .env("GIT_AUTHOR_NAME", "T")
            .env("GIT_AUTHOR_EMAIL", "t@localhost")
            .env("GIT_COMMITTER_NAME", "T")
            .env("GIT_COMMITTER_EMAIL", "t@localhost")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "first"]);
    git(&["branch", "review/aaa"]);
    (dir, repo)
}

fn plan_with(names: &[&str]) -> job::JobPlan {
    job::JobPlan {
        target: None,
        extras: Vec::new(),
        prepared_cache_key: None,
        prepared_local_source: None,
        ramdisk_size_gib: None,
        ramdisk_env: Vec::new(),
        job: "0123456789ab".into(),
        remote: String::new(),
        checkout: None,
        task: "abcd1234".into(),
        need: None,
        kind: None,
        subject: "Subject".into(),
        brief: "body".into(),
        rules: String::new(),
        refs: job::Refs {
            from: "main".into(),
            branch: "factory/x".into(),
        },
        publish_ref: None,
        publish_expect: None,
        publish_contains: None,
        delete_ref: None,
        delete_expect: None,
        publish_remote: None,
        with: names
            .iter()
            .map(|n| job::WithRef {
                name: (*n).to_string(),
                commit: String::new(),
            })
            .collect(),
        model: "m/x".into(),
        budget: 600,
        depth: 1,
        harness_cmd: "fake".into(),
        env_names: Vec::new(),
        committer: PoolCommitter {
            name: "bot".into(),
            email: "bot@example.com".into(),
            unknown: std::collections::BTreeMap::new(),
        },
        read_only: false,
        facts: Vec::new(),
        restore: Vec::new(),
        bridge: None,
    }
}

/// A pool config with a harness and one served band: enough to plan a model
/// job without touching a machine.
fn band_config() -> PoolConfig {
    toml::from_str(
        r#"
provider = 'local'
slots = 2
[harness]
name = 'fake'
run = 'fake --model {model} @{brief}'
models = ['m/ok', 'm/other']
[[bands.P1]]
model = 'm/ok'
reasoning = 'high'
[[bands.P1]]
model = 'm/other'
reasoning = 'low'
[committer]
name = 'bot'
email = 'bot@example.com'
"#,
    )
    .unwrap()
}

/// The dispatch flags of a `vivi pool dispatch` invocation, all flags unset.
fn dispatch_command(task: &str) -> PoolDispatchCommand {
    PoolDispatchCommand {
        target: None,
        also_target: Vec::new(),
        task: task.to_string(),
        need: None,
        kind: None,
        role: None,
        model: None,
        from: None,
        branch: None,
        publish_ref: None,
        publish_expect: None,
        publish_contains: None,
        delete_ref: None,
        delete_expect: None,
        with: Vec::new(),
        read_only: false,
        budget: None,
        command: None,
        allow_default_branch: false,
        ready_timeout: None,
        porcelain: true,
        pool: Some("p".into()),
        project: None,
    }
}

/// A plannable model dispatch for `task`: pinned to `m/ok`, with the
/// `from`/`branch` a publishing job needs.
fn model_command(task: &str) -> PoolDispatchCommand {
    let mut command = dispatch_command(task);
    command.model = Some("m/ok".into());
    command.from = Some("main".into());
    command.branch = Some("factory/x".into());
    command
}

/// A plannable dispatch command for `task` with no model named: the `from`
/// and `branch` a publishing job needs, nothing else.
fn ref_command(task: &str) -> PoolDispatchCommand {
    let mut command = dispatch_command(task);
    command.from = Some("main".into());
    command.branch = Some("factory/x".into());
    command
}

/// Take the pool `p` dispatch guard out from under the mailspace, the way a
/// concurrent dispatch on this machine would hold it.
fn held_guard(space: &Mailspace) -> DispatchGuard {
    DispatchGuard::try_acquire(&space.dir, "p")
        .unwrap()
        .expect("the guard is free before the test takes it")
}

/// Seed an open dispatch record on `task`, the way `work::submit` records
/// one: pool `p`, the given model, nothing collected yet.
fn seed_open_dispatch(space: &Mailspace, task: &str, model: &str) {
    let mut made = record(&[]);
    made.model = model.to_string();
    space
        .record_pool_note(
            task,
            "pool_dispatched",
            &vivi::pool::record::dispatch_note(&made),
            &vivi::pool::record::dispatch_text(&made),
        )
        .unwrap();
}

/// The `Ready` plan of a planning attempt; a wait or a refusal fails the test.
fn planned(result: Result<Decision<Planned>, VivariumError>) -> Planned {
    match result {
        Ok(Decision::Ready(planned)) => planned,
        Ok(Decision::Waiting(wait)) => panic!("unexpected wait: {}", wait.reason.word()),
        Err(e) => panic!("unexpected refusal: {e}"),
    }
}

/// The `Waiting` answer of a planning attempt; a plan or a refusal fails the
/// test.
fn waiting(result: Result<Decision<Planned>, VivariumError>) -> Wait {
    match result {
        Ok(Decision::Waiting(wait)) => wait,
        Ok(Decision::Ready(_)) => panic!("unexpected plan; a wait was required"),
        Err(e) => panic!("unexpected refusal: {e}"),
    }
}

/// The refusal text of a planning attempt; a wait or a plan fails the test.
fn refused(result: Result<Decision<Planned>, VivariumError>) -> String {
    match result {
        Err(e) => e.to_string(),
        Ok(Decision::Ready(_)) => panic!("unexpected plan; a refusal was required"),
        Ok(Decision::Waiting(wait)) => panic!("unexpected wait: {}", wait.reason.word()),
    }
}

/// Plan one dispatch against a local worktree pool over a scratch
/// repository, answering `Ready`; a wait or a refusal fails the test.
fn ready_plan(
    space: &Mailspace,
    task: &PoolTask,
    config: PoolConfig,
    command: &PoolDispatchCommand,
) -> Planned {
    planned(plan_phase(
        command,
        space,
        task,
        &planning_pool(space, config),
    ))
}

/// A task body from a front, the way the dispatcher reads one.
fn task_body(front: &TaskFront, prose: &str) -> String {
    vivi::letter::render(front, prose).unwrap()
}

/// Send one more task into an existing mailspace, as the dispatcher would
/// find it: the body carries `front`, the addressee is a plain `pump` role.
fn send_task(space: &mut Mailspace, front: TaskFront) -> PoolTask {
    let sent = space
        .send(vivi::mailspace::SendRequest {
            from: "mind".into(),
            to: vec!["pump".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "go".into(),
            body: task_body(&front, "do it"),
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
        })
        .unwrap();
    let handle = sent.delivered[0].handle.clone();
    space.pool_task(&handle).unwrap()
}

/// A mailspace with one stored task, as the dispatcher would find it: the
/// body carries `front`, the addressee is a plain `pump` role.
fn stored_task_with(front: TaskFront) -> (tempfile::TempDir, Mailspace, PoolTask) {
    let tmp = tempfile::tempdir().unwrap();
    let mut space = Mailspace::init(Some(tmp.path())).unwrap();
    space.add_identity("mind").unwrap();
    space
        .add_role("pump", vivi::mailspace::RoleUpdate::default())
        .unwrap();
    let task = send_task(&mut space, front);
    (tmp, space, task)
}

/// A task whose body names no band, ready to plan against `band_config`.
fn plain_task() -> (tempfile::TempDir, Mailspace, PoolTask) {
    stored_task_with(TaskFront::default())
}

#[test]
fn builtin_auditor_marker_replaces_the_recipient_charter_for_one_task() {
    let tmp = tempfile::tempdir().unwrap();
    let mut space = Mailspace::init(Some(tmp.path())).unwrap();
    space.add_identity("hand").unwrap();
    space
        .set_charter("hand", "Hand-only standing instructions")
        .unwrap();
    let task = PoolTask {
        handle: String::from("abcd1234"),
        subject: String::from("audit"),
        body: vivi::letter::render(
            &vivi::letter::work::TaskFront {
                job: Some(vivi::letter::work::JobFront {
                    id: String::from("audit-1"),
                    kind: String::from("auditor:range"),
                    builtin_role: Some(String::from("auditor")),
                }),
                ..Default::default()
            },
            "Audit this range.",
        )
        .unwrap(),
        addressee: Some(String::from("hand")),
    };

    let role = role_facts(&space, &task, None).unwrap();

    assert_eq!(role.kind.as_deref(), Some("auditor"));
    assert!(role.read_only.unwrap_or(false));
    assert!(role.charter.contains("# Auditor"));
    assert!(!role.charter.contains("Hand-only standing instructions"));
}

fn commit_of(repo: &std::path::Path, rev: &str) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", rev])
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn extra_refs_resolve_in_the_source_repository_before_any_slot_is_claimed() {
    let (_dir, repo) = repo_with_branch();
    let main = commit_of(&repo, "main");
    let mut plan = plan_with(&["review/aaa", &main[..8]]);
    resolve_with_local(&repo, &mut plan, &Progress::silent()).unwrap();
    assert_eq!(
        plan.with,
        vec![
            job::WithRef {
                name: "review/aaa".into(),
                commit: commit_of(&repo, "review/aaa"),
            },
            job::WithRef {
                name: main.clone(),
                commit: main,
            },
        ],
        "a branch resolves to its tip; an abbreviated hash becomes the full commit"
    );
    assert!(
        plan.with
            .iter()
            .all(|w| w.commit.len() == 40 && w.commit.chars().all(|c| c.is_ascii_hexdigit()))
    );
}

#[test]
fn remote_dispatch_does_not_load_a_pump_reset_base() {
    let tmp = tempfile::tempdir().unwrap();
    let space = Mailspace::init(Some(tmp.path())).unwrap();
    let config: PoolConfig = toml::from_str("provider = 'fly'\nslots = 1\n").unwrap();

    assert_eq!(dispatch_reset_base(&config, &space), None);
}

#[test]
fn an_unknown_ref_is_an_error_naming_the_ref_and_the_repository() {
    let (_dir, repo) = repo_with_branch();
    let mut plan = plan_with(&["nope/ref"]);
    let err = resolve_with_local(&repo, &mut plan, &Progress::silent())
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("nope/ref") && err.contains(repo.to_str().unwrap()),
        "{err}"
    );
}

/// A finished dispatch record, the way `dispatch` builds one: the `with`
/// entries already carry their resolved commit, cut to 12 hex digits.
fn record(with: &[String]) -> vivi::pool::record::DispatchRecord {
    vivi::pool::record::DispatchRecord {
        target: None,
        repository: None,
        pool: "p".into(),
        need: None,
        kind: None,
        slot: "p-slot-1".into(),
        machine: "m1".into(),
        job: "0123456789ab".into(),
        model: "m/x".into(),
        from: "main".into(),
        branch: "factory/a".into(),
        start: "a".repeat(40),
        with: with.to_vec(),
        budget: 600,
        at: 1000,
        read_only: false,
        hardware: String::new(),
        abandoned: 0,
        abandoned_cost_micros: 0,
    }
}

#[test]
fn the_human_dispatch_line_carries_the_extra_refs_when_there_are_some() {
    let made = record(&[
        "review/aaa@aaaa1111aaaa".into(),
        "bb034af1@bbbb2222bbbb".into(),
    ]);
    let text = dispatched_text("abcd1234", &made, None, "");
    assert!(
        text.contains("  with review/aaa@aaaa1111aaaa bb034af1@bbbb2222bbbb"),
        "{text}"
    );
}

#[test]
fn the_human_dispatch_line_has_no_with_segment_when_the_job_has_none() {
    let text = dispatched_text("abcd1234", &record(&[]), None, "");
    assert!(!text.contains("with"), "{text}");
}

#[test]
fn the_named_role_wins_over_the_task_addressee() {
    let tmp = tempfile::tempdir().unwrap();
    let mut space = Mailspace::init(Some(tmp.path())).unwrap();
    let kind = |kind: &str| vivi::mailspace::RoleUpdate {
        kind: Some(Some(kind.into())),
        ..Default::default()
    };
    space.add_role("pump", kind("pump")).unwrap();
    space.add_role("merge", kind("merge")).unwrap();
    let task = PoolTask {
        handle: String::from("abcd1234"),
        subject: String::from("step"),
        body: vivi::letter::render(&vivi::letter::work::TaskFront::step("n", 1, "merge"), "")
            .unwrap(),
        addressee: Some(String::from("pump")),
    };

    let named = role_facts(&space, &task, Some("merge")).unwrap();
    assert_eq!(named.name, "merge");
    assert_eq!(named.kind.as_deref(), Some("merge"));

    let default = role_facts(&space, &task, None).unwrap();
    assert_eq!(default.name, "pump");
    assert_eq!(default.kind.as_deref(), Some("pump"));

    assert!(role_facts(&space, &task, Some("nobody")).is_none());
}

#[test]
fn plan_phase_carries_a_pinned_model_into_the_plan_and_holds_admission() {
    let (_tmp, space, task) = plain_task();
    let mut command = model_command(&task.handle);
    command.model = Some("m/other".into());

    let planned = ready_plan(&space, &task, band_config(), &command);

    assert_eq!(planned.flags.model, "m/other");
    assert_eq!(planned.flags.reasoning, None, "no band names the reasoning");
    assert!(!planned.flags.read_only);
    assert_eq!(planned.plan.model, "m/other");
    assert_eq!(
        planned.plan.rules,
        job::DEFAULT_RULES,
        "no charter, no project rules"
    );
    assert!(
        planned.role_line.contains("role pump band - -> m/other"),
        "{}",
        planned.role_line
    );
    assert!(
        planned.admission.is_some(),
        "a model job holds the pool's admission"
    );
}

#[test]
fn plan_phase_draws_a_band_pair_from_the_live_snapshot() {
    let (_tmp, space, task) = stored_task_with(TaskFront {
        band: Some("P1".into()),
        ..TaskFront::default()
    });
    let command = ref_command(&task.handle);

    let planned = ready_plan(&space, &task, band_config(), &command);

    let pair = (
        planned.flags.model.as_str(),
        planned.flags.reasoning.as_deref().unwrap_or_default(),
    );
    assert!(
        pair == ("m/ok", "high") || pair == ("m/other", "low"),
        "the draw answers one of the band's own pairs, not {pair:?}"
    );
    assert_eq!(planned.plan.model, planned.flags.model);
    assert!(
        planned.role_line.contains("band P1 -> "),
        "{}",
        planned.role_line
    );
}

#[test]
fn command_jobs_stay_outside_model_admission() {
    let (_tmp, space, task) = plain_task();
    let mut command = ref_command(&task.handle);
    command.command = Some("echo hi".into());
    // Another dispatch owns the pool's guard; a --command job neither takes
    // it nor waits for it.
    let _holder = held_guard(&space);

    let planned = planned(plan_phase(
        &command,
        &space,
        &task,
        &planning_pool(&space, band_config()),
    ));

    assert_eq!(planned.flags.model, "", "a --command job names no model");
    assert_eq!(planned.flags.reasoning, None);
    assert_eq!(planned.flags.command.as_deref(), Some("echo hi"));
    assert_eq!(
        planned.plan.rules,
        job::DEFAULT_RULES,
        "the pool rules verbatim"
    );
    assert!(planned.role_line.is_empty());
    assert!(
        planned.admission.is_none(),
        "a --command job takes no admission and no guard"
    );
}

#[test]
fn permanent_planning_faults_stay_errors_despite_contention() {
    let (_tmp, space, task) = plain_task();
    let config = band_config();
    let pool = planning_pool(&space, config.clone());
    let _holder = held_guard(&space);

    let mut unknown = model_command(&task.handle);
    unknown.model = Some("m/nope".into());
    let err = refused(plan_phase(&unknown, &space, &task, &pool));
    assert!(
        err.contains("not in [harness].models") && err.contains("m/nope"),
        "{err}"
    );

    let over = {
        let mut command = model_command(&task.handle);
        command.budget = Some(43_201);
        command
    };
    let err = refused(plan_phase(&over, &space, &task, &pool));
    assert!(err.contains("budget"), "{err}");

    let (_sent, unserved_space, unserved) = stored_task_with(TaskFront {
        band: Some("P5".into()),
        from: Some("main".into()),
        branch: Some("factory/x".into()),
        ..TaskFront::default()
    });
    let unserved_pool = planning_pool(&unserved_space, config);
    let no_model = dispatch_command(&unserved.handle);
    let err = refused(plan_phase(
        &no_model,
        &unserved_space,
        &unserved,
        &unserved_pool,
    ));
    assert!(err.contains("P5"), "{err}");
}

#[test]
fn a_contended_pool_answers_a_wait_before_any_side_effect() {
    let (_tmp, space, task) = plain_task();
    let command = model_command(&task.handle);
    let _holder = held_guard(&space);
    let pool = planning_pool(&space, band_config());

    let wait = waiting(plan_phase(&command, &space, &task, &pool));

    assert_eq!(wait.reason, WaitReason::DispatchBusy);
    assert_eq!(wait.retry_at, None);
    assert!(
        work::open_dispatches(&space).unwrap().is_empty(),
        "a wait claims nothing and records nothing"
    );
}

#[test]
fn an_exhausted_pin_waits_as_itself_instead_of_substituting() {
    let (_tmp, mut space, task) = plain_task();
    let busy = send_task(&mut space, TaskFront::default());
    seed_open_dispatch(&space, &busy.handle, "m/ok");
    let mut config = band_config();
    config.caps.insert("m/ok".into(), 1);

    let mut pinned = model_command(&task.handle);
    pinned.model = Some("m/ok".into());
    let pool = planning_pool(&space, config.clone());
    let wait = waiting(plan_phase(&pinned, &space, &task, &pool));
    assert_eq!(wait.reason, WaitReason::ModelCap);
    assert_eq!(wait.retry_at, None, "a cap has no knowable release");

    // The free sibling model is no substitute: the pin stays the pin, and
    // only its own occupancy is judged.
    let mut other = model_command(&task.handle);
    other.model = Some("m/other".into());
    let planned = ready_plan(&space, &task, config, &other);
    assert_eq!(planned.flags.model, "m/other");
    assert_eq!(
        planned.flags.reasoning, None,
        "the pin runs as normalized; no band names the reasoning"
    );
}

#[test]
fn a_wholly_blocked_band_waits_instead_of_drawing() {
    let (_tmp, mut space, task) = stored_task_with(TaskFront {
        band: Some("P1".into()),
        ..TaskFront::default()
    });
    let first = send_task(&mut space, TaskFront::default());
    let second = send_task(&mut space, TaskFront::default());
    seed_open_dispatch(&space, &first.handle, "m/ok");
    seed_open_dispatch(&space, &second.handle, "m/other");
    let mut config = band_config();
    config.caps.insert("m/ok".into(), 1);
    config.caps.insert("m/other".into(), 1);
    let command = ref_command(&task.handle);
    let pool = planning_pool(&space, config);

    let wait = waiting(plan_phase(&command, &space, &task, &pool));

    assert_eq!(wait.reason, WaitReason::ModelCap);
    assert_eq!(
        work::open_dispatches(&space).unwrap().len(),
        2,
        "the wait dispatched nothing new"
    );
}

#[test]
fn the_admission_guard_is_held_by_the_plan_until_dropped() {
    let (_tmp, space, task) = plain_task();
    let command = model_command(&task.handle);
    let config = band_config();

    let planned = ready_plan(&space, &task, config.clone(), &command);
    assert!(planned.admission.is_some());

    // The plan still owns the pool: a second dispatch of it waits on the
    // guard instead of planning past it.
    let pool = planning_pool(&space, config.clone());
    assert_eq!(
        waiting(plan_phase(&command, &space, &task, &pool)).reason,
        WaitReason::DispatchBusy
    );

    // Dropping the plan releases the pool for the next dispatch.
    drop(planned);
    let again = ready_plan(&space, &task, config, &command);
    assert!(again.admission.is_some());
}

#[test]
fn a_racing_open_dispatch_is_refused_under_the_guard() {
    let (_tmp, space, task) = plain_task();
    // The record lands after dispatch's early check ran, while this dispatch
    // already plans: only the guard-time repeat can refuse it.
    seed_open_dispatch(&space, &task.handle, "m/ok");
    let command = model_command(&task.handle);
    let pool = planning_pool(&space, band_config());

    let err = refused(plan_phase(&command, &space, &task, &pool));

    assert!(err.contains("already has an open dispatch"), "{err}");
}

/// A local pool handle over `root`, ready to plan against.
fn local_pool(space: &Mailspace, config: PoolConfig) -> pools::PoolHandle {
    let ctx = vivi::pool::runpod::HostContext {
        dir: space.dir.clone(),
        mailspace: String::from("test"),
        pool: String::from("p"),
        pools: vec![String::from("p")],
    };
    let provider = vivi::pool::open_provider(&config, &ctx);
    pools::PoolHandle {
        id: PoolId::new(&space.config.name, space.mailspace_id(), "p"),
        config,
        provider,
    }
}

/// A plannable local pool: a scratch repository beside the handle, so the
/// dispatch's source checks pass without touching a machine.
fn planning_pool(space: &Mailspace, config: PoolConfig) -> pools::PoolHandle {
    let repo = tempfile::tempdir().unwrap();
    let mut config = config;
    config.local.worktree_of = Some(repo.path().display().to_string());
    local_pool(space, config)
}

#[test]
fn plan_phase_answers_ready_without_launching_anything() {
    let tmp = tempfile::tempdir().unwrap();
    let (_sent_dir, space, task) = stored_task_with(TaskFront {
        from: Some("main".into()),
        branch: Some("factory/x".into()),
        ..TaskFront::default()
    });
    let handle = task.handle.clone();

    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    let mut config = band_config();
    config.local.worktree_of = Some(repo.display().to_string());
    let pool = local_pool(&space, config);
    let mut command = dispatch_command(&handle);
    command.model = Some("m/ok".into());

    let planned = plan_phase(&command, &space, &task, &pool).unwrap();
    let Decision::Ready(planned) = planned else {
        panic!("a pinned job with free config is ready, not waiting");
    };
    assert_eq!(planned.plan.model, "m/ok");
    assert_eq!(planned.plan.refs.branch, "factory/x");
    assert_eq!(planned.plan.refs.from, "main");
    assert_eq!(planned.plan.job, planned.job_id);
    assert!(planned.plan.checkout.is_none(), "no slot was prepared");
    assert!(
        planned.admission.is_some(),
        "a model job holds the pool's admission while it plans"
    );
    assert!(
        work::open_dispatches(&space).unwrap().is_empty(),
        "planning records no dispatch"
    );
}

#[test]
fn a_wait_reports_porcelain_lines_that_parse_back() {
    let wait = Wait {
        reason: WaitReason::ModelCap,
        retry_at: Some(1700),
    };
    let line = waiting_line("abcd1234", "p", &wait, true);
    assert_eq!(line, porcelain::waiting("abcd1234", "p", &wait));
    let parsed = porcelain::parse_waiting(&line).unwrap();
    assert_eq!(parsed.task, "abcd1234");
    assert_eq!(parsed.pool, "p");
    assert_eq!(parsed.reason, WaitReason::ModelCap);
    assert_eq!(parsed.retry_at, Some(1700));
}

#[test]
fn a_wait_reports_one_concise_human_line_without_a_refusal() {
    let waiting = Wait {
        reason: WaitReason::ProviderCooldown,
        retry_at: None,
    };
    let line = waiting_line("abcd1234", "p", &waiting, false);
    assert!(
        line.starts_with("waiting abcd1234 in p: provider-cooldown"),
        "{line}"
    );
    assert!(
        !line.contains("refused") && !line.contains("dispatched"),
        "{line}"
    );
    let deadlined = waiting_line(
        "abcd1234",
        "p",
        &Wait {
            reason: WaitReason::BandUnavailable,
            retry_at: Some(99),
        },
        false,
    );
    assert!(deadlined.contains("ask again after 99"), "{deadlined}");
}
