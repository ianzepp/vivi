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
        reset_base: None,
    };
    dispatch(provider, id, &launch, now, &pacing())
}

fn flags(model: &str) -> DispatchFlags {
    DispatchFlags {
        model: model.into(),
        need: None,
        kind: None,
        from: Some("main".into()),
        branch: Some("factory/a".into()),
        publish_ref: None,
        publish_expect: None,
        with: Vec::new(),
        budget: None,
        allow_default_branch: false,
        read_only: false,
        reasoning: None,
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

#[test]
fn monitor_need_and_kind_metadata_survive_job_planning_without_changing_permissions() {
    let flags = DispatchFlags {
        need: Some("need1234".into()),
        kind: Some("fix".into()),
        ..flags("m/ok")
    };
    let plan = plan(&flags).unwrap();
    assert_eq!(plan.need.as_deref(), Some("need1234"));
    assert_eq!(plan.kind.as_deref(), Some("fix"));
    assert!(!plan.read_only);
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
fn the_brief_is_a_pointer_with_the_bridge_and_the_body_without_it() {
    let without = plan(&flags("m/ok")).unwrap();
    assert_eq!(without.brief, "body text");
    let mut config = config();
    config.bridge.enabled = true;
    let with = plan_job(
        &config,
        "p",
        ("abcd1234", "Subject line", "body text"),
        &flags("m/ok"),
        "rules".into(),
        "job000000001".into(),
    )
    .unwrap();
    assert!(with.brief.contains("abcd1234"), "{}", with.brief);
    assert!(with.brief.contains("Subject line"), "{}", with.brief);
    assert!(!with.brief.contains("body text"), "{}", with.brief);
    for tool in ["show", "thread", "reply", "done"] {
        assert!(with.brief.contains(tool), "{tool}: {}", with.brief);
    }
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

fn reasoning_config(run: &str) -> PoolConfig {
    let mut c = config();
    c.harness.as_mut().unwrap().run = run.into();
    c
}

#[test]
fn a_template_using_reasoning_is_refused_without_a_level() {
    let c = reasoning_config("fake --model {model} --thinking {reasoning}");
    let err = plan_job(
        &c,
        "p",
        ("t", "s", "b"),
        &flags("m/ok"),
        "r".into(),
        "j".into(),
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains(
            "harness.run uses {reasoning} but no reasoning level was chosen for this job; \
             dispatch by band (a `band:` line in the task, or a role with a band) or remove \
             {reasoning} from [pools.p.harness].run"
        ),
        "{err}"
    );
}

#[test]
fn a_chosen_reasoning_level_reaches_the_harness_command() {
    let mut f = flags("m/ok");
    f.reasoning = Some("xhigh".into());
    let p = plan_job(
        &reasoning_config("fake --model {model} --thinking {reasoning}"),
        "p",
        ("t", "s", "b"),
        &f,
        "r".into(),
        "j".into(),
    )
    .unwrap();
    assert_eq!(p.harness_cmd, "fake --model m/ok --thinking xhigh");
}

#[test]
fn an_unsafe_reasoning_level_is_refused_by_name() {
    for bad in [
        "x; rm -rf",
        "",
        "a b",
        "$(x)",
        "thirty-three-characters-long-thing!!",
    ] {
        let mut f = flags("m/ok");
        f.reasoning = Some(bad.into());
        let err = plan_job(
            &reasoning_config("fake --model {model} --thinking {reasoning}"),
            "p",
            ("t", "s", "b"),
            &f,
            "r".into(),
            "j".into(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains(bad), "{err}");
        assert!(err.contains("reasoning"), "{err}");
    }
}

#[test]
fn a_template_without_the_reasoning_placeholder_plans_as_before() {
    let mut f = flags("m/ok");
    f.reasoning = Some("xhigh".into());
    let p = plan(&f).unwrap();
    assert_eq!(p.harness_cmd, "fake --model m/ok @/home/hand/brief.md");
    assert_eq!(plan(&flags("m/ok")).unwrap().harness_cmd, p.harness_cmd);
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
fn publish_flags_require_a_full_heads_ref_and_full_candidate_sha() {
    let mut f = flags("m/ok");
    f.publish_ref = Some("refs/heads/next".into());
    let planned = plan(&f).unwrap();
    assert_eq!(planned.publish_ref.as_deref(), Some("refs/heads/next"));

    f.publish_ref = Some("main".into());
    assert!(plan(&f).is_err());
    f.publish_ref = Some("refs/heads/next.lock".into());
    assert!(plan(&f).is_err());
    f.publish_ref = Some("refs/heads/main".into());
    f.publish_expect = Some("a".repeat(39));
    assert!(plan(&f).is_err());
    f.publish_ref = None;
    assert!(plan(&f).is_err());
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

const FULL: &str = "bb034af1000000000000000000000000000000ff";

#[test]
fn an_abbreviated_lowercase_hash_becomes_the_full_commit() {
    assert_eq!(full_from("bb034af1", FULL), FULL);
}

#[test]
fn an_abbreviated_uppercase_hash_becomes_the_full_commit() {
    assert_eq!(full_from("BB034AF1", FULL), FULL);
}

#[test]
fn a_full_hash_is_left_alone() {
    assert_eq!(full_from(FULL, FULL), FULL);
}

#[test]
fn a_branch_name_is_left_alone() {
    assert_eq!(full_from("main", FULL), "main");
    assert_eq!(full_from("factory/a", FULL), "factory/a");
}

#[test]
fn a_hexadecimal_looking_branch_that_is_no_prefix_is_left_alone() {
    assert_eq!(full_from("deadbeef", FULL), "deadbeef");
}

#[test]
fn a_three_character_value_is_left_alone() {
    assert_eq!(full_from("abc", FULL), "abc");
}

#[test]
fn the_bundle_carries_whatever_from_the_plan_was_given() {
    let mut p = plan(&flags("m/ok")).unwrap();
    p.refs.from = "bb034af1".into();
    // What remote_start does on a pool that fetches from the remote: the
    // abbreviated hash is replaced by the resolved commit before the job is
    // built, so params.env carries the full hash.
    p.refs.from = full_from(&p.refs.from, FULL);
    let bundle = build_bundle(&p, "p-slot-1", &Secret::new("t".into()));
    let params = bundle
        .files
        .iter()
        .find(|f| f.name == "params.env")
        .unwrap();
    let params = String::from_utf8_lossy(&params.content);
    assert!(params.contains(&format!("FROM='{FULL}'\n")), "{params}");
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
fn the_plan_carries_the_with_names_from_flags_or_body() {
    let mut f = flags("m/ok");
    f.with = vec!["review/aaa".into(), "review/bbb".into()];
    let p = plan(&f).unwrap();
    assert_eq!(
        p.with
            .iter()
            .map(|w| (w.name.as_str(), w.commit.as_str()))
            .collect::<Vec<_>>(),
        [("review/aaa", ""), ("review/bbb", "")],
        "the commit is filled later, before a slot is claimed"
    );
    let body = "work\nwith: review/ccc\n- with: review/ddd\n";
    let p = plan_job(
        &config(),
        "p",
        ("t", "s", body),
        &flags("m/ok"),
        "r".into(),
        "j".into(),
    )
    .unwrap();
    let names: Vec<&str> = p.with.iter().map(|w| w.name.as_str()).collect();
    assert_eq!(names, ["review/ccc", "review/ddd"]);
    let mut f = flags("m/ok");
    f.with = vec!["review/eee".into()];
    let p = plan_job(&config(), "p", ("t", "s", body), &f, "r".into(), "j".into()).unwrap();
    let names: Vec<&str> = p.with.iter().map(|w| w.name.as_str()).collect();
    assert_eq!(names, ["review/eee"], "flags beat the body");
    let mut f = flags("m/ok");
    f.with = vec!["main".into()];
    let err = plan(&f).unwrap_err().to_string();
    assert!(err.contains("main") && err.contains("start ref"), "{err}");
}

#[test]
fn a_read_only_plan_accepts_with_refs() {
    let mut f = read_only(Some("main"), None);
    f.with = vec!["review/aaa".into()];
    let p = plan(&f).unwrap();
    assert!(p.read_only);
    assert_eq!(p.with[0].name, "review/aaa");
}

#[test]
fn the_dispatch_record_carries_the_extra_refs() {
    let fake = Fake::default();
    fake.add(slot("m1", "p-slot-1", "stopped", "p", 1));
    let mut p = plan(&flags("m/ok")).unwrap();
    p.with = vec![
        job::WithRef {
            name: "review/aaa".into(),
            commit: "a".repeat(40),
        },
        job::WithRef {
            name: "bb034af1".into(),
            commit: FULL.to_string(),
        },
    ];
    let made = go(
        &fake,
        &pid("p"),
        &p,
        &Secret::new("t".into()),
        &"c".repeat(40),
        5000,
    )
    .unwrap();
    assert_eq!(
        made.with,
        [
            format!("review/aaa@{}", "a".repeat(12)),
            format!("bb034af1@{}", &FULL[..12]),
        ]
    );
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

#[test]
fn workspace_target_overrides_the_shared_pools_legacy_repository() {
    let pump: crate::pump::config::PumpConfig = toml::from_str(
        r#"
[targets.gradus]
path = "gradus"
remote_url = "git@github.com:faberlang/gradus.git"
github_repo = "faberlang/gradus"
branch = "next"
"#,
    )
    .unwrap();
    let shared = config();
    let resolved =
        target::resolve(&shared, &pump, std::path::Path::new("/workspace"), "gradus").unwrap();
    assert_eq!(resolved.config.repo.as_deref(), Some("faberlang/gradus"));
    assert_eq!(shared.repo.as_deref(), Some("o/r"));
    assert_eq!(
        resolved.target.source,
        "https://github.com/faberlang/gradus.git"
    );
    assert!(
        target::resolve(
            &shared,
            &pump,
            std::path::Path::new("/workspace"),
            "missing"
        )
        .is_err()
    );
}

#[test]
fn a_target_with_mismatched_github_identity_is_refused() {
    let pump: crate::pump::config::PumpConfig = toml::from_str(
        r#"
[targets.radix]
path = "radix"
remote_url = "https://github.com/faberlang/gradus.git"
github_repo = "faberlang/radix"
"#,
    )
    .unwrap();
    let result = target::resolve(
        &config(),
        &pump,
        std::path::Path::new("/workspace"),
        "radix",
    );
    assert!(result.err().unwrap().to_string().contains("does not match"));
}
