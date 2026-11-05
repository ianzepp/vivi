use super::*;
use crate::pool::dispatch::{DispatchFlags, plan_job};
use crate::pool::slot::PoolId;

/// A ticket pool config: the refusal happens before any server setting is
/// read, so none is given.
fn config() -> PoolConfig {
    toml::from_str("provider = \"ticket\"\nslots = 2\n").unwrap()
}

fn context(dir: &std::path::Path) -> HostContext {
    HostContext {
        dir: dir.join(".vivi"),
        mailspace: "m".to_string(),
        pool: "p".to_string(),
        pools: vec!["p".to_string()],
    }
}

fn task_body() -> String {
    vivi_mail::letter::fixture::task_body("body")
}

#[test]
fn no_exact_intake_plan_is_submitted_as_a_ticket() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = context(tmp.path());
    let config = config();
    let flags = DispatchFlags {
        model: String::new(),
        command: Some("sh -c 'true'".into()),
        from: Some("main".into()),
        branch: Some("factory/a".into()),
        publish_ref: Some("refs/heads/vivi/intake/abc123".into()),
        publish_expect: Some("a".repeat(40)),
        ..DispatchFlags::default()
    };
    let plan = plan_job(
        &config,
        "p",
        ("abcd1234", "Subject line", &task_body()),
        &flags,
        "rules".into(),
        "job000000001".into(),
    )
    .unwrap();
    assert!(
        plan.exact_intake(),
        "the fixture is a validated intake plan"
    );
    let provider = TicketProvider::new(&config, &ctx);
    let id = PoolId {
        mailspace: "m".into(),
        pool: "p".into(),
    };
    let error = provider
        .submit(&TicketRequest {
            pool: &id,
            plan: &plan,
            now: 1,
        })
        .unwrap_err()
        .to_string();
    assert!(error.contains("exact-intake"), "{error}");
    assert!(provider.entries().is_empty(), "no ticket entry was written");
    assert!(
        !provider.home().exists(),
        "no ticket home was made: nothing was submitted"
    );
}
