use super::*;
use crate::pool::fake_provider::{Fake, claimed, pid, slot};
use crate::pool::slot::{META_CLAIMED, META_TASK};

fn config() -> PoolConfig {
    toml::from_str(
        r#"
repo = "o/r"
slots = 2
image = "debian:12"
build_dir = "target"
[fly]
app = "a"
[github]
app_id = 1
private_key = "k.pem"
[harness]
name = "fake"
run = "fake --model {model} @{brief}"
models = ["m/ok", "m/other"]
env = ["SOME_KEY"]
[committer]
name = "bot"
email = "bot@example.com"
"#,
    )
    .unwrap()
}

fn go(
    provider: &dyn PoolProvider,
    id: &PoolId,
    p: &JobPlan,
    token: &Secret,
    start: &str,
    now: u64,
) -> Result<DispatchRecord, VivariumError> {
    let config = config();
    let progress = crate::pool::progress::Progress::silent();
    let launch = Launch {
        config: &config,
        plan: p,
        token,
        start,
        progress: &progress,
        ready_timeout: None,
    };
    dispatch(provider, id, &launch, now, &pacing())
}

fn flags(model: &str) -> DispatchFlags {
    DispatchFlags {
        model: model.into(),
        from: Some("main".into()),
        branch: Some("factory/a".into()),
        budget: None,
        allow_default_branch: false,
        read_only: false,
    }
}

fn plan(flags: &DispatchFlags) -> Result<JobPlan, VivariumError> {
    plan_job(
        &config(),
        "p",
        ("abcd1234", "Subject line", "body text"),
        flags,
        "rules".into(),
        "job000000001".into(),
    )
}

fn pacing() -> Pacing {
    Pacing {
        poll: std::time::Duration::ZERO,
        ready_timeout: std::time::Duration::ZERO,
        stop_timeout: std::time::Duration::ZERO,
        start_grace: std::time::Duration::from_secs(super::super::collect::START_GRACE_SECS),
    }
}

#[test]
fn a_valid_request_plans_a_job() {
    let p = plan(&flags("m/ok")).unwrap();
    assert_eq!(p.budget, DEFAULT_BUDGET_SECS);
    assert_eq!(p.harness_cmd, "fake --model m/ok @/home/hand/brief.md");
    assert_eq!(p.env_names, ["SOME_KEY"]);
    assert_eq!(p.refs.branch, "factory/a");
    assert_eq!(p.brief, "body text");
}

#[test]
fn a_pool_with_no_depth_setting_plans_jobs_at_fetch_depth_fifty() {
    assert_eq!(plan(&flags("m/ok")).unwrap().depth, 50);
}

#[test]
fn the_pool_depth_setting_reaches_the_plan() {
    let mut config = config();
    config.depth = Some(7);
    let p = plan_job(
        &config,
        "p",
        ("abcd1234", "Subject line", "body text"),
        &flags("m/ok"),
        "rules".into(),
        "job000000001".into(),
    )
    .unwrap();
    assert_eq!(p.depth, 7);
}

#[test]
fn the_pool_restore_setting_reaches_the_plan() {
    let mut config = config();
    config.restore = vec!["runtime/llvm/Cargo.lock".into()];
    let p = plan_job(
        &config,
        "p",
        ("abcd1234", "Subject line", "body text"),
        &flags("m/ok"),
        "rules".into(),
        "job000000001".into(),
    )
    .unwrap();
    assert_eq!(p.restore, ["runtime/llvm/Cargo.lock"]);
}

#[test]
fn an_unlisted_model_is_refused() {
    let err = plan(&flags("m/nope")).unwrap_err().to_string();
    assert!(err.contains("not in [harness].models"), "{err}");
    assert!(err.contains("m/ok, m/other"), "{err}");
}

#[test]
fn from_and_branch_default_to_the_body() {
    let no_flags = DispatchFlags {
        model: "m/ok".into(),
        ..DispatchFlags::default()
    };
    let p = plan_job(
        &config(),
        "p",
        ("t", "s", "do it\nfrom: main\nbranch: factory/body"),
        &no_flags,
        "r".into(),
        "j".into(),
    )
    .unwrap();
    assert_eq!(
        (p.refs.from.as_str(), p.refs.branch.as_str()),
        ("main", "factory/body")
    );
    assert!(
        plan(&DispatchFlags {
            from: None,
            ..flags("m/ok")
        })
        .is_err()
    );
}

#[test]
fn budget_is_bounded() {
    for bad in [0, 43_201] {
        let f = DispatchFlags {
            budget: Some(bad),
            ..flags("m/ok")
        };
        assert!(plan(&f).unwrap_err().to_string().contains("budget"));
    }
    let f = DispatchFlags {
        budget: Some(30),
        ..flags("m/ok")
    };
    assert_eq!(plan(&f).unwrap().budget, 30);
}

#[test]
fn harness_and_committer_are_required() {
    let mut c = config();
    c.harness = None;
    let err = plan_job(
        &c,
        "lanes",
        ("t", "s", "b"),
        &flags("m/ok"),
        "r".into(),
        "j".into(),
    );
    assert!(
        err.unwrap_err()
            .to_string()
            .contains("[pools.lanes.harness]")
    );
    let mut c = config();
    c.committer = None;
    let err = plan_job(
        &c,
        "lanes",
        ("t", "s", "b"),
        &flags("m/ok"),
        "r".into(),
        "j".into(),
    );
    assert!(
        err.unwrap_err()
            .to_string()
            .contains("[pools.lanes.committer]")
    );
}

#[test]
fn job_ids_are_twelve_hex_digits_and_differ() {
    let a = new_job_id("t", 1, 1);
    assert_eq!(a.len(), 12);
    assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    assert_ne!(a, new_job_id("t", 2, 1));
}

#[test]
fn dispatch_claims_delivers_one_archive_and_starts() {
    let fake = Fake::default();
    fake.add(slot("m1", "p-slot-1", "started", "p", 1));
    fake.add(slot("m2", "p-slot-2", "stopped", "p", 2));
    let token = Secret::new("tok-secret-value".into());
    let p = plan(&flags("m/ok")).unwrap();
    let record = go(&fake, &pid("p"), &p, &token, &"c".repeat(40), 5000).unwrap();
    assert_eq!(record.slot, "p-slot-2");
    assert_eq!(record.machine, "m2");
    assert_eq!(record.at, 5000);
    assert_eq!(record.start, "c".repeat(40));
    let files = fake.files.borrow();
    assert_eq!(files.len(), 1, "one upload");
    assert_eq!(
        (files[0].0.as_str(), files[0].1.as_str()),
        ("m2", "/work/job.tgz")
    );
    assert_eq!(fake.state_of("m2"), "started");
    assert_eq!(fake.meta("m2", META_TASK).as_deref(), Some("abcd1234"));
    assert!(
        fake.leases.borrow().is_empty(),
        "lease released after delivery"
    );
    assert_eq!(fake.calls_to("start"), 1);
}

#[test]
fn dispatch_with_no_free_slot_touches_no_machine() {
    let fake = Fake::default();
    fake.add(claimed("m1", "p-slot-1", "stopped", "p", "oldtask1"));
    let p = plan(&flags("m/ok")).unwrap();
    let err = go(&fake, &pid("p"), &p, &Secret::new("t".into()), "s", 1).unwrap_err();
    assert!(err.to_string().contains("no free slot"));
    assert!(fake.files.borrow().is_empty());
    assert_eq!(fake.calls_to("start"), 0);
}

#[test]
fn a_failed_start_undoes_the_claim() {
    let fake = Fake::default();
    fake.add(slot("m1", "p-slot-1", "stopped", "p", 1));
    fake.fail_start.set(true);
    let p = plan(&flags("m/ok")).unwrap();
    let err = go(&fake, &pid("p"), &p, &Secret::new("t".into()), "s", 1);
    assert!(err.is_err());
    assert!(fake.meta("m1", META_TASK).is_none());
    assert!(fake.meta("m1", META_CLAIMED).is_none());
    assert!(fake.leases.borrow().is_empty());
}

#[test]
fn the_default_branch_is_refused_unless_allowed() {
    let err = check_push_target("main", "main", false)
        .unwrap_err()
        .to_string();
    assert!(err.contains("default branch \"main\""), "{err}");
    assert!(err.contains("--allow-default-branch"), "{err}");
    check_push_target("main", "main", true).unwrap();
    check_push_target("factory/x", "main", false).unwrap();
    check_push_target("main", "trunk", false).unwrap();
}

#[test]
fn the_dispatch_record_names_its_pool() {
    let fake = Fake::default();
    fake.add(slot("m1", "ms-p-slot-1", "stopped", "p", 1));
    let p = plan(&flags("m/ok")).unwrap();
    let id = pid("p");
    let record = go(&fake, &id, &p, &Secret::new("t".into()), "s", 1).unwrap();
    assert_eq!(record.pool, "p");
}

#[test]
fn a_dispatch_never_claims_a_free_slot_of_another_pool_or_mailspace() {
    let fake = Fake::default();
    fake.add(slot("m1", "ms-q-slot-1", "stopped", "q", 1));
    let mut elsewhere = slot("m2", "other-p-slot-1", "stopped", "p", 1);
    elsewhere
        .metadata
        .insert(crate::pool::slot::META_MAILSPACE.into(), "other".into());
    fake.add(elsewhere);
    let p = plan(&flags("m/ok")).unwrap();
    let err = go(&fake, &pid("p"), &p, &Secret::new("t".into()), "s", 1).unwrap_err();
    assert!(err.to_string().contains("pool p has no slots"), "{err}");
    assert!(fake.meta("m1", META_TASK).is_none());
    assert!(fake.meta("m2", META_TASK).is_none());
}

#[test]
fn a_pool_with_no_repository_refuses_to_dispatch_and_names_the_setting() {
    let mut c = config();
    c.repo = None;
    c.github = None;
    let err = plan_job(
        &c,
        "lanes",
        ("t", "s", "b\nfrom: a\nbranch: b"),
        &flags("m/ok"),
        "r".into(),
        "j".into(),
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("[pools.lanes] repo is required to dispatch"),
        "{err}"
    );
    // The self-test brings its own source tree and needs no repository.
    c.embedded = Some(crate::pool::config::EmbeddedSource {
        remote: "file:///work/remote.git".into(),
        ..Default::default()
    });
    plan_job(
        &c,
        "lanes",
        ("t", "s", "b"),
        &flags("m/ok"),
        "r".into(),
        "j".into(),
    )
    .unwrap();
}

fn read_only(from: Option<&str>, branch: Option<&str>) -> DispatchFlags {
    DispatchFlags {
        model: "m/ok".into(),
        from: from.map(str::to_string),
        branch: branch.map(str::to_string),
        read_only: true,
        ..DispatchFlags::default()
    }
}

#[test]
fn a_read_only_job_needs_a_from_and_has_no_branch() {
    let p = plan(&read_only(Some("main"), None)).unwrap();
    assert!(p.read_only);
    assert_eq!(p.refs.from, "main");
    assert_eq!(p.refs.branch, "");
    assert!(!plan(&flags("m/ok")).unwrap().read_only);
    let err = plan(&read_only(None, None)).unwrap_err().to_string();
    assert!(err.contains("no from"), "{err}");
    let err = plan(&read_only(Some("-bad"), None))
        .unwrap_err()
        .to_string();
    assert!(err.contains("not a plain commit or branch"), "{err}");
}

#[test]
fn a_read_only_job_takes_from_from_the_body_and_ignores_a_body_branch() {
    let p = plan_job(
        &config(),
        "p",
        ("t", "s", "review it\nfrom: main\nbranch: factory/body"),
        &read_only(None, None),
        "r".into(),
        "j".into(),
    )
    .unwrap();
    assert_eq!(p.refs.from, "main");
    assert_eq!(p.refs.branch, "");
}

#[test]
fn a_branch_flag_with_read_only_is_refused() {
    let err = plan(&read_only(Some("main"), Some("factory/a")))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("--branch cannot be used with --read-only"),
        "{err}"
    );
}

fn config_of(provider: &str, extra: &str) -> PoolConfig {
    toml::from_str(&format!(
        "provider = \"{provider}\"\nrepo = \"o/r\"\nslots = 1\n{extra}"
    ))
    .unwrap()
}

#[test]
fn confined_providers_get_no_read_only_warning_and_the_others_say_what_is_not_promised() {
    for provider in ["docker", "fly"] {
        assert_eq!(
            read_only_warning(&config_of(provider, "")),
            None,
            "{provider}"
        );
    }
    let ssh = read_only_warning(&config_of("ssh", "")).unwrap();
    assert!(
        ssh.starts_with("warning: --read-only on the ssh provider"),
        "{ssh}"
    );
    assert!(ssh.contains("real user account"), "{ssh}");
    assert!(ssh.contains("Vivi cannot stop it"), "{ssh}");
    let local = read_only_warning(&config_of("local", "")).unwrap();
    assert!(local.contains("on the local provider"), "{local}");
    assert!(local.contains("real user account"), "{local}");
}

#[test]
fn a_worktree_pool_warns_that_the_worker_could_still_commit_with_plain_git() {
    let worktree = config_of(
        "local",
        "[local]\nroot = \"/tmp/lanes\"\nworktree_of = \"/tmp/project\"\n",
    );
    let warning = read_only_warning(&worktree).unwrap();
    assert!(warning.contains("worktree pool"), "{warning}");
    assert!(
        warning.contains("could still commit to it with plain Git"),
        "{warning}"
    );
    assert!(warning.contains("there is no token"), "{warning}");
}

#[test]
fn a_ready_timeout_override_is_for_per_dispatch_pools_only_and_in_range() {
    use crate::pool::provider::SlotLifetime;
    assert_eq!(
        check_ready_timeout(SlotLifetime::Resting, None).unwrap(),
        None
    );
    assert_eq!(
        check_ready_timeout(SlotLifetime::PerDispatch, None).unwrap(),
        None
    );
    assert_eq!(
        check_ready_timeout(SlotLifetime::PerDispatch, Some(30)).unwrap(),
        Some(30)
    );
    assert_eq!(
        check_ready_timeout(SlotLifetime::PerDispatch, Some(1800)).unwrap(),
        Some(1800)
    );
    let resting = check_ready_timeout(SlotLifetime::Resting, Some(60))
        .unwrap_err()
        .to_string();
    assert!(
        resting.contains("--ready-timeout")
            && resting.contains("per dispatch")
            && resting.contains("resting slots"),
        "{resting}"
    );
    for out in [0, 29, 1801] {
        let said = check_ready_timeout(SlotLifetime::PerDispatch, Some(out))
            .unwrap_err()
            .to_string();
        assert!(said.contains("between 30 and 1800"), "{said}");
    }
}
