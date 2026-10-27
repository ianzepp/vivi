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

/// `band_config` with a cap: `m/ok` may run once in this pool at a time.
fn capped_config() -> PoolConfig {
    let mut config = band_config();
    config.caps.insert(String::from("m/ok"), 1);
    config
}

/// Record one open dispatch of `model` in pool `p` on a task of its own —
/// the live evidence an admission reads and its caps judge.
fn seeded_open(space: &Mailspace, task: &str, model: &str) {
    let made = vivi::pool::record::DispatchRecord {
        model: model.into(),
        ..record(&[])
    };
    space
        .record_pool_note(
            task,
            "pool_dispatched",
            &vivi::pool::record::dispatch_note(&made),
            &vivi::pool::record::dispatch_text(&made),
        )
        .unwrap();
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

/// The permanent prep of a dispatch request; a refusal fails the test.
fn prep_of(result: Result<DispatchPrep, VivariumError>) -> DispatchPrep {
    match result {
        Ok(prep) => prep,
        Err(e) => panic!("unexpected refusal: {e}"),
    }
}

/// The `Ready` plan of a plan phase; a wait or a refusal fails the test.
fn ready_plan(result: Result<Decision<Planned>, VivariumError>) -> Planned {
    match result {
        Ok(Decision::Ready(planned)) => planned,
        Ok(Decision::Waiting(wait)) => panic!("unexpected wait: {}", wait.reason.word()),
        Err(e) => panic!("unexpected refusal: {e}"),
    }
}

/// The wait a plan phase answered; readiness or a refusal fails the test.
fn wait_answer(result: Result<Decision<Planned>, VivariumError>) -> Wait {
    match result {
        Ok(Decision::Waiting(wait)) => wait,
        Ok(Decision::Ready(_)) => panic!("unexpected readiness"),
        Err(e) => panic!("unexpected refusal: {e}"),
    }
}

/// The refusal a plan phase raised; readiness or a wait fails the test.
fn refusal(result: Result<Decision<Planned>, VivariumError>) -> String {
    match result {
        Err(e) => e.to_string(),
        Ok(Decision::Ready(_)) => panic!("unexpected readiness"),
        Ok(Decision::Waiting(wait)) => panic!("unexpected wait: {}", wait.reason.word()),
    }
}

/// A task body from a front, the way the dispatcher reads one.
fn task_body(front: &TaskFront, prose: &str) -> String {
    vivi::letter::render(front, prose).unwrap()
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
    let task = space.pool_task(&handle).unwrap();
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
fn resolve_dispatch_normalizes_a_pinned_model_but_draws_nothing() {
    let (_tmp, space, task) = plain_task();
    let mut command = dispatch_command(&task.handle);
    command.model = Some("m/other".into());

    let prep = prep_of(resolve_dispatch(
        &command,
        &band_config(),
        "p",
        &space,
        &task,
        "pool rules".into(),
    ));

    let intent = prep.intent.as_ref().unwrap();
    assert_eq!(
        intent.selection,
        role_dispatch::Selection::Pin {
            model: "m/other".into(),
            reasoning: None,
        }
    );
    assert_eq!(prep.flags.model, "", "the pair waits for the live admission");
    assert!(!prep.flags.read_only);
    assert_eq!(prep.rules, "pool rules", "no charter, no project rules");
    assert!(prep.role_line.is_empty(), "the role line waits for the pair");
}

#[test]
fn resolve_dispatch_defers_a_band_draw_to_the_live_snapshot() {
    let (_tmp, space, task) = stored_task_with(TaskFront {
        band: Some("P1".into()),
        ..TaskFront::default()
    });
    let command = dispatch_command(&task.handle);

    let prep = prep_of(resolve_dispatch(
        &command,
        &band_config(),
        "p",
        &space,
        &task,
        String::new(),
    ));

    assert_eq!(
        prep.intent.map(|intent| intent.selection),
        Some(role_dispatch::Selection::Draw { band: 1 })
    );
    assert_eq!(prep.flags.model, "", "no configuration-only draw");
}

#[test]
fn resolve_command_planning_stays_outside_model_selection() {
    let (_tmp, space, task) = plain_task();
    let mut command = dispatch_command(&task.handle);
    command.command = Some("echo hi".into());

    let prep = prep_of(resolve_dispatch(
        &command,
        &band_config(),
        "p",
        &space,
        &task,
        "pool rules".into(),
    ));

    assert!(prep.intent.is_none(), "a --command job stays outside admission");
    assert_eq!(prep.flags.model, "", "a --command job names no model");
    assert_eq!(prep.flags.reasoning, None);
    assert_eq!(prep.flags.command.as_deref(), Some("echo hi"));
    assert_eq!(prep.rules, "pool rules", "the pool rules verbatim");
    assert!(prep.role_line.is_empty());
}

#[test]
fn permanent_planning_faults_stay_errors_not_waits() {
    let (_tmp, space, task) = plain_task();
    let config = band_config();
    let mut unknown = dispatch_command(&task.handle);
    unknown.model = Some("m/nope".into());
    let err = resolve_dispatch(&unknown, &config, "p", &space, &task, String::new())
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("not in [harness].models") && err.contains("m/nope"),
        "{err}"
    );

    let (_tmp, space, unserved) = stored_task_with(TaskFront {
        band: Some("P5".into()),
        ..TaskFront::default()
    });
    let no_model = dispatch_command(&unserved.handle);
    let err = resolve_dispatch(&no_model, &config, "p", &space, &unserved, String::new())
        .unwrap_err()
        .to_string();
    assert!(err.contains("P5"), "{err}");
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
