use super::*;

const SECRET: &str = "ghs_SUPERsecretTOKEN0123456789";

fn committer() -> PoolCommitter {
    PoolCommitter {
        name: "Bot Name".into(),
        email: "bot@example.com".into(),
        unknown: std::collections::BTreeMap::new(),
    }
}

fn plan() -> JobPlan {
    JobPlan {
        job: "0123456789ab".into(),
        remote: "https://github.com/acme/widgets.git".into(),
        task: "abcd1234".into(),
        subject: "Fix the thing's edge case\nsecond line".into(),
        brief: "do the work\nfrom: main\nbranch: factory/x\n".into(),
        rules: DEFAULT_RULES.into(),
        refs: Refs {
            from: "main".into(),
            branch: "factory/x".into(),
        },
        model: "zai/glm-5.3-flash".into(),
        budget: 900,
        depth: 1,
        harness_cmd: render_harness(
            "pi -p --model {model} --append-system-prompt {rules} @{brief}",
            "zai/glm-5.3-flash",
        ),
        env_names: vec!["ZAI_API_KEY".into()],
        committer: committer(),
    }
}

fn text(bundle: &Bundle, name: &str) -> String {
    let file = bundle.files.iter().find(|f| f.name == name).unwrap();
    String::from_utf8(file.content.clone()).unwrap()
}

#[test]
fn harness_template_fills_model_brief_and_rules() {
    let cmd = render_harness("h --model {model} --rules {rules} @{brief}", "a/b-1");
    assert_eq!(
        cmd,
        "h --model a/b-1 --rules /home/hand/rules.md @/home/hand/brief.md"
    );
}

#[test]
fn unknown_placeholders_are_found() {
    assert_eq!(unknown_placeholder("x {model} {brief} {rules}"), None);
    assert_eq!(unknown_placeholder("x {mdoel}").as_deref(), Some("mdoel"));
    assert_eq!(unknown_placeholder("x ${home} {Upper} {} {a b}"), None);
    assert_eq!(unknown_placeholder("awk '{print $1}'"), None);
    assert_eq!(unknown_placeholder("trailing {open"), None);
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
