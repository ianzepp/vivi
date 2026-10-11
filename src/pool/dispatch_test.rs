use super::*;
use crate::pool::fake_provider::{Fake, claimed, slot};
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

fn flags(model: &str) -> DispatchFlags {
    DispatchFlags {
        model: model.into(),
        from: Some("main".into()),
        branch: Some("factory/a".into()),
        budget: None,
        allow_default_branch: false,
    }
}

fn plan(flags: &DispatchFlags) -> Result<JobPlan, VivariumError> {
    plan_job(
        &config(),
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
fn an_unlisted_model_is_refused() {
    let err = plan(&flags("m/nope")).unwrap_err().to_string();
    assert!(err.contains("not in [pool.harness].models"), "{err}");
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
    let err = plan_job(&c, ("t", "s", "b"), &flags("m/ok"), "r".into(), "j".into());
    assert!(err.unwrap_err().to_string().contains("[pool.harness]"));
    let mut c = config();
    c.committer = None;
    let err = plan_job(&c, ("t", "s", "b"), &flags("m/ok"), "r".into(), "j".into());
    assert!(err.unwrap_err().to_string().contains("[pool.committer]"));
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
    let record = dispatch(&fake, "p", &p, &token, &"c".repeat(40), 5000, &pacing()).unwrap();
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
    let err = dispatch(&fake, "p", &p, &Secret::new("t".into()), "s", 1, &pacing()).unwrap_err();
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
    let err = dispatch(&fake, "p", &p, &Secret::new("t".into()), "s", 1, &pacing());
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
