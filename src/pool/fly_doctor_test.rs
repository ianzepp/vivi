use super::*;
use crate::pool::doctor::Status;

fn facts() -> FlyFacts {
    FlyFacts {
        app: "lanes".into(),
        harness_env: vec!["KEY_A".into(), "KEY_B".into()],
        cli: Ok("fly v0.4".into()),
        login: Some(Ok(())),
        token: Some(Ok(())),
        app_found: Some(Ok(true)),
        secrets: Some(Ok(vec!["KEY_A".into(), "KEY_B".into(), "OTHER".into()])),
    }
}

fn status(checks: &[Check], name: &str) -> Status {
    checks.iter().find(|c| c.name == name).unwrap().status
}

fn fix<'a>(checks: &'a [Check], name: &str) -> &'a str {
    checks
        .iter()
        .find(|c| c.name == name)
        .and_then(|c| c.fix.as_deref())
        .unwrap_or_default()
}

#[test]
fn everything_present_is_ok_under_the_fly_heading() {
    let out = checks(&facts());
    assert!(out.iter().all(|c| c.status == Status::Ok), "{out:#?}");
    assert!(out.iter().all(|c| c.group == "Provider (Fly)"));
}

#[test]
fn a_missing_cli_fails_and_skips_everything_after_it() {
    let mut f = facts();
    f.cli = Err("failed to run `fly`".into());
    f.login = None;
    f.token = None;
    f.app_found = None;
    f.secrets = None;
    let out = checks(&f);
    assert_eq!(status(&out, "flyctl"), Status::Fail);
    assert!(fix(&out, "flyctl").contains("install flyctl"));
    for name in ["login", "api token", "app", "secrets"] {
        assert_eq!(status(&out, name), Status::Skipped, "{name}");
    }
}

#[test]
fn no_login_fails_with_the_login_command() {
    let mut f = facts();
    f.login = Some(Err("not logged in".into()));
    f.token = None;
    f.app_found = None;
    f.secrets = None;
    let out = checks(&f);
    assert_eq!(status(&out, "login"), Status::Fail);
    assert_eq!(fix(&out, "login"), "fly auth login");
    assert_eq!(status(&out, "api token"), Status::Skipped);
    assert_eq!(status(&out, "app"), Status::Skipped);
}

#[test]
fn no_api_token_fails() {
    let mut f = facts();
    f.token = Some(Err("no Fly API token".into()));
    let out = checks(&f);
    assert_eq!(status(&out, "api token"), Status::Fail);
    assert!(fix(&out, "api token").contains("fly auth login"));
}

#[test]
fn a_missing_app_fails_with_create_and_skips_secrets() {
    let mut f = facts();
    f.app_found = Some(Ok(false));
    f.secrets = None;
    let out = checks(&f);
    assert_eq!(status(&out, "app"), Status::Fail);
    assert!(fix(&out, "app").starts_with("vivi pool create"));
    assert_eq!(status(&out, "secrets"), Status::Skipped);
}

#[test]
fn missing_secret_names_warn_with_the_set_command_and_never_show_values() {
    let mut f = facts();
    f.secrets = Some(Ok(vec!["KEY_A".into()]));
    let out = checks(&f);
    let check = out.iter().find(|c| c.name == "secrets").unwrap();
    assert_eq!(check.status, Status::Warn);
    assert!(check.detail.contains("KEY_B") && !check.detail.contains("KEY_A"));
    assert!(check.detail.contains("image itself provides"));
    assert_eq!(
        check.fix.as_deref(),
        Some("fly secrets set -a lanes KEY_B=<value>")
    );
}

#[test]
fn no_harness_env_names_means_nothing_to_check() {
    let mut f = facts();
    f.harness_env.clear();
    f.secrets = None;
    assert_eq!(status(&checks(&f), "secrets"), Status::Ok);
}

#[test]
fn a_secrets_listing_failure_fails() {
    let mut f = facts();
    f.secrets = Some(Err("HTTP 500".into()));
    let out = checks(&f);
    assert_eq!(status(&out, "secrets"), Status::Fail);
    assert!(fix(&out, "secrets").contains("fly secrets list -a lanes"));
}
