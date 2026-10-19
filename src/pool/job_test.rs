use super::*;

use crate::pool::bridge::Grant;

const SECRET: &str = "ghs_SUPERsecretTOKEN0123456789";
const BRIDGE_KEY: &str = "vivi_bridge_SECRET0123456789abcdef";

fn committer() -> PoolCommitter {
    PoolCommitter {
        name: "Bot Name".into(),
        email: "bot@example.com".into(),
        unknown: std::collections::BTreeMap::new(),
    }
}

#[test]
fn privileged_publish_controls_are_runner_only_job_parameters() {
    let mut p = plan();
    p.publish_ref = Some("refs/heads/next".into());
    p.publish_expect = Some("a".repeat(40));
    p.publish_remote = Some("file:///tmp/pump-target.git".into());
    let bundle = build_bundle(&p, "s", &Secret::new(SECRET.into()));
    let params = text(&bundle, "params.env");

    assert!(params.contains("PUBLISH_REF='refs/heads/next'"));
    assert!(params.contains(&format!("PUBLISH_EXPECT='{}'", "a".repeat(40))));
    assert!(params.contains("PUBLISH_REMOTE='file:///tmp/pump-target.git'"));
    assert!(
        !bundle
            .agent_visible()
            .iter()
            .any(|file| { String::from_utf8_lossy(&file.content).contains("PUBLISH_REMOTE") })
    );
}

fn plan() -> JobPlan {
    JobPlan {
        target: None,
        prepared_cache_key: None,
        prepared_local_source: None,
        ramdisk_size_gib: None,
        ramdisk_env: Vec::new(),
        job: "0123456789ab".into(),
        remote: "https://github.com/acme/widgets.git".into(),
        checkout: None,
        task: "abcd1234".into(),
        need: None,
        kind: None,
        subject: "Fix the thing's edge case\nsecond line".into(),
        brief: "do the work\nfrom: main\nbranch: factory/x\n".into(),
        rules: DEFAULT_RULES.into(),
        refs: Refs {
            from: "main".into(),
            branch: "factory/x".into(),
        },
        publish_ref: None,
        publish_expect: None,
        publish_remote: None,
        with: Vec::new(),
        model: "zai/glm-5.3-flash".into(),
        budget: 900,
        depth: 1,
        harness_cmd: render_harness(
            "pi -p --model {model} --append-system-prompt {rules} @{brief}",
            "zai/glm-5.3-flash",
            None,
        ),
        env_names: vec!["ZAI_API_KEY".into()],
        committer: committer(),
        read_only: false,
        facts: Vec::new(),
        restore: Vec::new(),
        bridge: None,
    }
}

fn bridge_grant() -> Grant {
    Grant {
        key: Secret::new(BRIDGE_KEY.to_string()),
        key_id: "key_bridge01".into(),
        label: "pool-wt-0123456789ab".into(),
        server: "http://127.0.0.1:8080".into(),
        project_id: "abcde-fghjk".into(),
    }
}

fn text(bundle: &Bundle, name: &str) -> String {
    let file = bundle.files.iter().find(|f| f.name == name).unwrap();
    String::from_utf8(file.content.clone()).unwrap()
}

#[test]
fn the_bundle_carries_the_pool_fetch_depth() {
    let mut p = plan();
    p.depth = 50;
    let params = text(
        &build_bundle(&p, "p-slot-2", &Secret::new(SECRET.into())),
        "params.env",
    );
    assert!(params.contains("DEPTH='50'\n"));
}

#[test]
fn a_ramdisk_job_gets_its_size_and_quoted_environment_in_params_only() {
    let mut p = plan();
    p.ramdisk_size_gib = Some(8);
    p.ramdisk_env = vec![
        ("CARGO_INCREMENTAL".into(), "0".into()),
        ("RAMDISK_TEST".into(), "a 'value'\nsecond line".into()),
    ];

    let bundle = build_bundle(&p, "p-slot-2", &Secret::new(SECRET.into()));
    let params = text(&bundle, "params.env");

    assert!(params.contains("RAMDISK_SIZE_GIB='8'\n"), "{params}");
    assert!(params.contains("RAMDISK_ENV_COUNT='2'\n"), "{params}");
    assert!(
        params.contains("RAMDISK_ENV_0_NAME='CARGO_INCREMENTAL'\n"),
        "{params}"
    );
    assert!(params.contains("RAMDISK_ENV_0_VALUE='0'\n"), "{params}");
    assert!(
        params.contains("RAMDISK_ENV_1_VALUE='a '\\''value'\\''\nsecond line'\n"),
        "{params}"
    );
    assert!(
        bundle
            .agent_visible()
            .iter()
            .all(|file| file.name != "params.env"),
        "RAM settings stay runner-only"
    );
}

#[test]
fn a_prepared_local_source_is_delivered_as_a_target_worktree() {
    let mut p = plan();
    p.remote.clear();
    p.prepared_cache_key = Some("b".repeat(64));
    p.prepared_local_source = Some("/repos/widgets".into());
    p.checkout = Some(Checkout {
        action: BranchAction::Create,
        start: "a".repeat(40),
    });

    let params = text(
        &build_bundle(&p, "p-slot-2", &Secret::new(SECRET.into())),
        "params.env",
    );

    assert!(params.contains(&format!("CHECKOUT_KEY='{}'\n", "b".repeat(64))));
    assert!(params.contains("TARGET_LOCAL_SOURCE='/repos/widgets'\n"));
    assert!(params.contains("LOCAL_ACTION='create'\n"));
    assert!(params.contains(&format!("LOCAL_START='{}'\n", "a".repeat(40))));
}

#[test]
fn harness_template_fills_model_brief_and_rules() {
    let cmd = render_harness("h --model {model} --rules {rules} @{brief}", "a/b-1", None);
    assert_eq!(
        cmd,
        "h --model a/b-1 --rules /home/hand/rules.md @/home/hand/brief.md"
    );
}

#[test]
fn harness_template_fills_the_mcp_config_path() {
    let cmd = render_harness("h --mcp-config {mcp_config} @{brief}", "a/b-1", None);
    assert_eq!(
        cmd,
        "h --mcp-config /home/hand/mcp.json @/home/hand/brief.md"
    );
    assert_eq!(
        render_harness("h {mcp_config} {brief} {rules} {model}", "m", None),
        "h /home/hand/mcp.json /home/hand/brief.md /home/hand/rules.md m"
    );
}

#[test]
fn unknown_placeholders_are_found() {
    assert_eq!(
        unknown_placeholder("x {model} {brief} {rules} {reasoning} {mcp_config}"),
        None
    );
    assert_eq!(unknown_placeholder("x {mdoel}").as_deref(), Some("mdoel"));
    assert_eq!(unknown_placeholder("x {foo}").as_deref(), Some("foo"));
    assert_eq!(unknown_placeholder("x ${home} {Upper} {} {a b}"), None);
    assert_eq!(unknown_placeholder("awk '{print $1}'"), None);
    assert_eq!(unknown_placeholder("trailing {open"), None);
}

#[test]
fn the_reasoning_level_renders_into_the_harness_command_when_given() {
    let cmd = render_harness(
        "pi --model {model} --thinking {reasoning}",
        "a/b-1",
        Some("xhigh"),
    );
    assert_eq!(cmd, "pi --model a/b-1 --thinking xhigh");
}

#[test]
fn a_template_without_the_placeholder_is_unchanged_without_a_reasoning() {
    let cmd = render_harness("h --model {model} @{brief}", "a/b-1", None);
    assert_eq!(cmd, "h --model a/b-1 @/home/hand/brief.md");
}

#[test]
fn uses_reasoning_follows_the_placeholder() {
    assert!(uses_reasoning("pi --thinking {reasoning}"));
    assert!(uses_reasoning(
        "pi --model {model} --thinking {reasoning} @{brief}"
    ));
    assert!(!uses_reasoning("pi --model {model}"));
    assert!(!uses_reasoning(""));
    assert!(!uses_reasoning("echo ${reasoning}"));
}

#[test]
fn body_lines_supply_from_and_branch() {
    let refs = body_refs("Do it.\n\nfrom: abc123\n- branch: factory/y\nfrom: ignored\n");
    assert_eq!(refs.from.as_deref(), Some("abc123"));
    assert_eq!(refs.branch.as_deref(), Some("factory/y"));
    assert_eq!(body_refs("nothing here"), BodyRefs::default());
    assert_eq!(body_refs("from: has spaces here").from, None);
    assert_eq!(body_refs("FROM:   Main  ").from.as_deref(), Some("Main"));
}

#[test]
fn flags_beat_body_lines_and_both_are_required() {
    let body = "from: main\nbranch: factory/body\n";
    let r = resolve_refs(Some("abc"), Some("factory/flag"), body).unwrap();
    assert_eq!(
        (r.from.as_str(), r.branch.as_str()),
        ("abc", "factory/flag")
    );
    let r = resolve_refs(None, None, body).unwrap();
    assert_eq!(
        (r.from.as_str(), r.branch.as_str()),
        ("main", "factory/body")
    );
    let r = resolve_refs(Some("abc"), None, body).unwrap();
    assert_eq!(
        (r.from.as_str(), r.branch.as_str()),
        ("abc", "factory/body")
    );
    assert!(
        resolve_refs(None, Some("b"), "")
            .unwrap_err()
            .to_string()
            .contains("no from")
    );
    assert!(
        resolve_refs(Some("a"), None, "")
            .unwrap_err()
            .to_string()
            .contains("no branch")
    );
}

#[test]
fn the_three_cases_use_one_rule() {
    let new = resolve_refs(Some(&"a".repeat(40)), Some("factory/new"), "").unwrap();
    assert_eq!(new.branch, "factory/new");
    let cont = resolve_refs(Some("factory/x"), Some("factory/x"), "").unwrap();
    assert_eq!(cont.from, cont.branch);
    let stack = resolve_refs(Some("factory/x"), Some("factory/y"), "").unwrap();
    assert_ne!(stack.from, stack.branch);
}

#[test]
fn refs_must_be_plain_names() {
    for bad in [
        "", "-x", "a..b", "a b", "/abs", "tail/", "a;b", "a//b", "x$(y)",
    ] {
        assert!(!plain_ref(bad), "{bad:?}");
    }
    assert!(plain_ref("factory/vivi-pool-smoke-1.a_b"));
    assert!(resolve_refs(Some("main"), Some("b.lock"), "").is_err());
    assert!(resolve_refs(Some("-evil"), Some("b"), "").is_err());
}

#[test]
fn with_flags_beat_the_body_and_body_lines_keep_their_order() {
    let body = "do it\n- with: review/bbb\n* WITH: review/aaa\n\
        with: ignored, flags won\nwith:\nwith: two words\nfrom: main\n";
    assert_eq!(
        resolve_with(&[], body, "main", "factory/x").unwrap(),
        ["review/bbb", "review/aaa"]
    );
    assert_eq!(
        resolve_with(&["review/zzz".into()], body, "main", "factory/x").unwrap(),
        ["review/zzz"]
    );
    assert!(
        resolve_with(&[], "nothing here", "main", "factory/x")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn with_refs_drop_duplicates_and_refuse_the_rest() {
    assert_eq!(
        resolve_with(
            &["review/a".into(), "review/b".into(), "review/a".into()],
            "",
            "main",
            "factory/x"
        )
        .unwrap(),
        ["review/a", "review/b"]
    );
    assert_eq!(
        resolve_with(&[], "with: r/a\nwith: r/a\n", "main", "b").unwrap(),
        ["r/a"]
    );
    for (value, needle) in [
        ("main", "start ref"),
        ("factory/x", "push target"),
        ("-evil", "not a plain"),
        ("a..b", "not a plain"),
    ] {
        let err = resolve_with(&[value.into()], "", "main", "factory/x")
            .unwrap_err()
            .to_string();
        assert!(err.contains(value) && err.contains(needle), "{err}");
    }
    let nine: Vec<String> = (1..=9).map(|i| format!("review/{i}")).collect();
    let err = resolve_with(&nine, "", "main", "factory/x")
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("at most 8") && err.contains("9 given"),
        "{err}"
    );
    let mut nine_and_a_repeat = nine;
    nine_and_a_repeat.push("review/1".into());
    let err = resolve_with(&nine_and_a_repeat, "", "main", "factory/x")
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("9 given") && !err.contains("10 given"),
        "the count is of distinct refs, not raw values: {err}"
    );
    let eight: Vec<String> = (1..=8).map(|i| format!("review/{i}")).collect();
    assert_eq!(
        resolve_with(&eight, "", "main", "factory/x").unwrap().len(),
        8
    );
}

#[test]
fn the_model_must_be_on_the_list() {
    let allowed = vec!["a/x".to_string(), "b/y".to_string()];
    assert!(check_model("a/x", &allowed).is_ok());
    let err = check_model("c/z", &allowed).unwrap_err().to_string();
    assert!(err.contains("a/x, b/y"), "{err}");
    assert!(
        check_model("a/x", &[])
            .unwrap_err()
            .to_string()
            .contains("none configured")
    );
}

#[test]
fn shell_quoting_survives_quotes() {
    assert_eq!(sh_quote("it's"), r"'it'\''s'");
    assert_eq!(sh_quote("plain"), "'plain'");
}

#[test]
fn the_token_lives_in_exactly_one_file_and_not_where_the_agent_looks() {
    let token = Secret::new(SECRET.to_string());
    let bundle = build_bundle(&plan(), "p-slot-2", &token);
    let holders: Vec<&str> = bundle
        .files
        .iter()
        .filter(|f| String::from_utf8_lossy(&f.content).contains(SECRET))
        .map(|f| f.name)
        .collect();
    assert_eq!(holders, ["token"]);
    let token_file = bundle.files.iter().find(|f| f.name == "token").unwrap();
    assert_eq!(token_file.mode, 0o600);
    for visible in bundle.agent_visible() {
        assert!(!String::from_utf8_lossy(&visible.content).contains(SECRET));
    }
    let names: Vec<&str> = bundle.agent_visible().iter().map(|f| f.name).collect();
    assert_eq!(names, ["brief.md", "rules.md", "harness.cmd"]);
}

#[test]
fn the_bridge_key_is_delivered_only_as_bridge_key_and_not_where_the_agent_looks() {
    let mut p = plan();
    p.bridge = Some(bridge_grant());
    let bundle = build_bundle(&p, "p-slot-2", &Secret::new(SECRET.to_string()));
    let holders: Vec<&str> = bundle
        .files
        .iter()
        .filter(|f| String::from_utf8_lossy(&f.content).contains(BRIDGE_KEY))
        .map(|f| f.name)
        .collect();
    assert_eq!(holders, ["bridge.key"]);
    let file = bundle
        .files
        .iter()
        .find(|f| f.name == "bridge.key")
        .unwrap();
    assert_eq!(file.mode, 0o600);
    assert_eq!(String::from_utf8_lossy(&file.content), BRIDGE_KEY);
    let names: Vec<&str> = bundle.agent_visible().iter().map(|f| f.name).collect();
    assert_eq!(names, ["brief.md", "rules.md", "harness.cmd", "mcp.json"]);
    for visible in bundle.agent_visible() {
        assert!(!String::from_utf8_lossy(&visible.content).contains(BRIDGE_KEY));
    }
}

#[test]
fn the_mcp_json_travels_only_with_a_grant_and_names_the_key_by_reference() {
    let without = build_bundle(&plan(), "s", &Secret::new("t".into()));
    assert!(!without.files.iter().any(|f| f.name == "mcp.json"));
    let mut p = plan();
    p.bridge = Some(bridge_grant());
    let bundle = build_bundle(&p, "s", &Secret::new("t".into()));
    let file = bundle.files.iter().find(|f| f.name == "mcp.json").unwrap();
    assert_eq!(file.mode, 0o644);
    let text = String::from_utf8_lossy(&file.content);
    assert!(text.contains("${VIVI_MCP_API_KEY}"), "{text}");
    assert!(text.contains("http://127.0.0.1:8080"), "{text}");
    assert!(text.contains("abcde-fghjk"), "{text}");
    assert!(!text.contains(BRIDGE_KEY), "{text}");
}

#[test]
fn the_bridge_params_appear_only_when_a_grant_exists_and_never_hold_the_key() {
    let without = text(
        &build_bundle(&plan(), "s", &Secret::new("t".into())),
        "params.env",
    );
    assert!(!without.contains("BRIDGE_"), "{without}");
    assert!(
        !build_bundle(&plan(), "s", &Secret::new("t".into()))
            .files
            .iter()
            .any(|f| f.name == "bridge.key")
    );
    let mut p = plan();
    p.bridge = Some(bridge_grant());
    let params = text(
        &build_bundle(&p, "s", &Secret::new("t".into())),
        "params.env",
    );
    assert!(
        params.contains("BRIDGE_SERVER='http://127.0.0.1:8080'\n"),
        "{params}"
    );
    assert!(
        params.contains("BRIDGE_PROJECT='abcde-fghjk'\n"),
        "{params}"
    );
    assert!(!params.contains(BRIDGE_KEY), "{params}");
}

#[test]
fn the_bundle_carries_what_the_runner_needs() {
    let bundle = build_bundle(&plan(), "p-slot-2", &Secret::new(SECRET.into()));
    let params = text(&bundle, "params.env");
    assert!(params.contains("REMOTE='https://github.com/acme/widgets.git'\n"));
    assert!(params.contains("FROM='main'\n"));
    assert!(params.contains("BRANCH='factory/x'\n"));
    assert!(params.contains("DEPTH='1'\n"));
    assert!(params.contains("BUDGET='900'\n"));
    assert!(params.contains("MODEL='zai/glm-5.3-flash'\n"));
    assert!(params.contains("SLOT='p-slot-2'\n"));
    assert!(params.contains(r"SUBJECT='Fix the thing'\''s edge case'"));
    assert!(params.contains("COMMIT_NAME='Bot Name'\n"));
    assert!(params.contains("ENV_NAMES='ZAI_API_KEY'\n"));
    assert_eq!(text(&bundle, "id"), "0123456789ab");
    assert_eq!(text(&bundle, "limit"), "1500");
    assert_eq!(text(&bundle, "brief.md"), plan().brief);
    assert!(text(&bundle, "harness.cmd").contains("--model zai/glm-5.3-flash"));
    assert!(text(&bundle, "run.sh").starts_with("#!/bin/bash"));
    assert!(text(&bundle, "rules.md").contains("Do not commit"));
}

#[test]
fn the_archive_unpacks_to_a_job_directory() {
    let bundle = build_bundle(&plan(), "p-slot-2", &Secret::new(SECRET.into()));
    let archive = pack(&bundle).unwrap();
    assert!(archive.len() < MAX_ARCHIVE_BYTES);
    let dir = tempfile::tempdir().unwrap();
    let tgz = dir.path().join("job.tgz");
    std::fs::write(&tgz, &archive).unwrap();
    let out = Command::new("tar")
        .arg("-xzf")
        .arg(&tgz)
        .arg("-C")
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    let token = std::fs::read_to_string(dir.path().join("job/token")).unwrap();
    assert_eq!(token, SECRET);
    let mode = std::fs::metadata(dir.path().join("job/token"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    assert!(dir.path().join("job/run.sh").exists());
}

#[test]
fn an_oversized_brief_is_refused_before_delivery() {
    let mut big = plan();
    let mut seed = 0x9e37_79b9_u32;
    big.brief = (0..MAX_ARCHIVE_BYTES * 2)
        .map(|_| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            char::from(b'a' + u8::try_from((seed >> 24) % 26).unwrap())
        })
        .collect();
    let err = pack(&build_bundle(&big, "s", &Secret::new("t".into()))).unwrap_err();
    assert!(err.to_string().contains("shorten the brief"), "{err}");
}

#[test]
fn a_file_remote_with_no_token_travels_as_given() {
    let mut local = plan();
    local.remote = "file:///work/remote.git".into();
    let bundle = build_bundle(&local, "p-doctor-slot-1", &Secret::new(String::new()));
    assert!(text(&bundle, "params.env").contains("REMOTE='file:///work/remote.git'\n"));
    assert_eq!(text(&bundle, "token"), "");
}

#[test]
fn a_local_checkout_travels_as_an_action_and_a_start_and_a_fetch_job_has_neither() {
    let secret = Secret::new(String::new());
    let params = text(&build_bundle(&plan(), "p-slot-1", &secret), "params.env");
    assert!(
        !params.contains("LOCAL_ACTION") && !params.contains("LOCAL_START"),
        "{params}"
    );

    for (action, word) in [
        (BranchAction::Create, "create"),
        (BranchAction::Continue, "continue"),
    ] {
        let mut local = plan();
        local.checkout = Some(Checkout {
            action,
            start: "0123abcd".into(),
        });
        let params = text(&build_bundle(&local, "p-slot-1", &secret), "params.env");
        assert!(
            params.contains(&format!("LOCAL_ACTION='{word}'\n")),
            "{params}"
        );
        assert!(params.contains("LOCAL_START='0123abcd'\n"), "{params}");
    }
}

#[test]
fn from_resolves_without_a_branch() {
    assert_eq!(resolve_from(Some("main"), "").unwrap(), "main");
    assert_eq!(
        resolve_from(None, "x\nfrom: abc123\nbranch: b").unwrap(),
        "abc123"
    );
    assert_eq!(resolve_from(Some("flag"), "from: body").unwrap(), "flag");
    assert!(
        resolve_from(None, "no refs here")
            .unwrap_err()
            .to_string()
            .contains("no from")
    );
    assert!(resolve_from(Some("a..b"), "").is_err());
}

#[test]
fn only_a_read_only_bundle_says_so_and_a_detached_checkout_has_its_own_word() {
    let params = |p: &JobPlan| {
        text(
            &build_bundle(p, "s", &Secret::new("t".into())),
            "params.env",
        )
    };
    assert!(!params(&plan()).contains("READ_ONLY"));
    let ro = JobPlan {
        target: None,
        read_only: true,
        refs: Refs {
            from: "main".into(),
            branch: String::new(),
        },
        checkout: Some(Checkout {
            action: BranchAction::Detached,
            start: "c".repeat(40),
        }),
        ..plan()
    };
    let text = params(&ro);
    assert!(text.contains("READ_ONLY='1'\n"), "{text}");
    assert!(text.contains("BRANCH=''\n"), "{text}");
    assert!(text.contains("LOCAL_ACTION='detached'\n"), "{text}");
}

#[test]
fn the_brief_is_delivered_with_lf_line_endings_and_the_stored_task_is_not_touched() {
    let stored = "do the work\r\nfrom: main\r\n\r\nlast\rline\r\n";
    let p = JobPlan {
        brief: stored.into(),
        ..plan()
    };
    let bundle = build_bundle(&p, "s", &Secret::new("t".into()));
    assert_eq!(
        text(&bundle, "brief.md"),
        "do the work\nfrom: main\n\nlast\rline\n"
    );
    assert_eq!(p.brief, stored);
    assert_eq!(lf("a\r\nb\n"), "a\nb\n");
    assert_eq!(lf("a\r\r\nb\r\n"), "a\nb\n", "CRLF stored over CRLF");
    assert_eq!(lf("no line end\r"), "no line end");
    assert_eq!(lf(""), "");
}

#[test]
fn the_facts_travel_as_one_command_per_line_and_only_when_there_are_some() {
    let names = |p: &JobPlan| -> Vec<&'static str> {
        build_bundle(p, "s", &Secret::new("t".into()))
            .files
            .iter()
            .map(|f| f.name)
            .collect()
    };
    assert!(!names(&plan()).contains(&"facts.cmd"));
    let p = JobPlan {
        target: None,
        facts: vec!["tool --query".into(), "uname -srm".into()],
        ..plan()
    };
    let bundle = build_bundle(&p, "s", &Secret::new("tok".into()));
    assert_eq!(text(&bundle, "facts.cmd"), "tool --query\nuname -srm\n");
    assert!(
        !bundle.agent_visible().iter().any(|f| f.name == "facts.cmd"),
        "facts are the runner's, not the agent's"
    );
}

#[test]
fn the_extra_refs_travel_as_with_list_and_only_when_there_are_some() {
    let names = |p: &JobPlan| -> Vec<&'static str> {
        build_bundle(p, "s", &Secret::new("t".into()))
            .files
            .iter()
            .map(|f| f.name)
            .collect()
    };
    assert!(!names(&plan()).contains(&"with.list"));
    let mut p = plan();
    p.with = vec![
        WithRef {
            name: "bb034af1".into(),
            commit: "b".repeat(40),
        },
        WithRef {
            name: "review/aaa".into(),
            commit: "a".repeat(40),
        },
    ];
    let bundle = build_bundle(&p, "s", &Secret::new("t".into()));
    let file = bundle.files.iter().find(|f| f.name == "with.list").unwrap();
    assert_eq!(file.mode, 0o600);
    assert_eq!(
        String::from_utf8_lossy(&file.content),
        format!(
            "bb034af1 {}\nreview/aaa {}\n",
            "b".repeat(40),
            "a".repeat(40)
        )
    );
    let visible = bundle.agent_visible().iter().any(|f| f.name == "with.list");
    assert!(
        !visible,
        "the refs are the runner's to fetch, not the brief"
    );
}

#[test]
fn the_restore_list_travels_as_one_path_per_line_and_only_when_there_are_some() {
    let names = |p: &JobPlan| -> Vec<&'static str> {
        build_bundle(p, "s", &Secret::new("t".into()))
            .files
            .iter()
            .map(|f| f.name)
            .collect()
    };
    assert!(!names(&plan()).contains(&"restore.list"));
    let p = JobPlan {
        target: None,
        restore: vec!["runtime/llvm/Cargo.lock".into(), "a b.txt".into()],
        ..plan()
    };
    let bundle = build_bundle(&p, "s", &Secret::new("tok".into()));
    assert_eq!(
        text(&bundle, "restore.list"),
        "runtime/llvm/Cargo.lock\na b.txt\n"
    );
    let file = bundle
        .files
        .iter()
        .find(|f| f.name == "restore.list")
        .unwrap();
    assert_eq!(
        file.mode, 0o600,
        "the list is the runner's, not the agent's"
    );
    assert!(
        !bundle
            .agent_visible()
            .iter()
            .any(|f| f.name == "restore.list")
    );
}

#[test]
fn target_jobs_deliver_cache_identity_and_repository_preparation() {
    let mut p = plan();
    p.target = Some(crate::pool::dispatch::target::JobTarget {
        id: "radix".into(),
        source: p.remote.clone(),
        publish_remote: None,
        cache_key: "a".repeat(64),
        local_source: None,
        seed: vec!["cargo build".into()],
        requires: vec!["command -v cargo".into()],
        build_dir: Some("target".into()),
    });
    let bundle = build_bundle(&p, "s", &Secret::new(SECRET.into()));
    assert_eq!(text(&bundle, "checkout-key"), "a".repeat(64));
    assert_eq!(text(&bundle, "repository"), p.remote);
    assert!(text(&bundle, "params.env").contains("TARGET='radix'"));
    assert!(text(&bundle, "target-seed.sh").contains("cargo build"));
    assert_eq!(text(&bundle, "target-requires.txt"), "command -v cargo");
}
