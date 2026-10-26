//! `validate` runs each pool table's own checks, so a table that every
//! `vivi pool` command would refuse is reported by `config validate` and at
//! pump startup instead of stalling dispatch.

use crate::mailspace::MailspaceConfig;
use crate::routing::{Issue, Severity, validate};

fn config(models: &str) -> MailspaceConfig {
    let text = format!(
        "name = \"t\"\n\
         [pools.p]\nprovider = \"local\"\nslots = 2\n\
         [pools.p.local]\nworktree_of = \"/tmp/t\"\n\
         [pools.p.harness]\nname = \"fake\"\nrun = \"fake --model {{model}} @{{brief}}\"\n\
         models = {models}\n"
    );
    toml::from_str(&text).expect("mailspace toml")
}

fn pool_errors(issues: &[Issue]) -> Vec<&str> {
    issues
        .iter()
        .filter(|issue| issue.severity == Severity::Error)
        .map(|issue| issue.message.as_str())
        .filter(|message| message.starts_with("[pools."))
        .collect()
}

#[test]
fn an_empty_harness_model_slug_is_an_error() {
    let issues = validate(&config("[\"\"]"));
    let found = pool_errors(&issues);
    assert_eq!(found.len(), 1, "{issues:?}");
    assert!(
        found[0].starts_with("[pools.p] harness.models holds an empty slug"),
        "{found:?}"
    );
}

#[test]
fn a_tilde_harness_model_slug_is_an_error() {
    let issues = validate(&config("[\"~deepseek/flash\"]"));
    assert_eq!(pool_errors(&issues).len(), 1, "{issues:?}");
}

#[test]
fn a_plain_harness_model_slug_adds_no_pool_error() {
    let issues = validate(&config("[\"deepseek/flash\"]"));
    assert!(pool_errors(&issues).is_empty(), "{issues:?}");
}

fn fly_without_an_app(models: &str) -> MailspaceConfig {
    let text = format!(
        "name = \"t\"\n\
         [pools.f]\nprovider = \"fly\"\nslots = 1\nimage = \"debian:13\"\n\
         [pools.f.harness]\nname = \"fake\"\nrun = \"fake --model {{model}} @{{brief}}\"\n\
         models = {models}\n"
    );
    toml::from_str(&text).expect("mailspace toml")
}

#[test]
fn a_setting_the_provider_needs_is_not_ready_not_an_error() {
    let issues = validate(&fly_without_an_app("[\"deepseek/flash\"]"));
    assert!(pool_errors(&issues).is_empty(), "{issues:?}");
    let missing: Vec<&Issue> = issues
        .iter()
        .filter(|issue| issue.severity == Severity::NotReady)
        .filter(|issue| issue.item.as_deref() == Some("pools.f.fly.app"))
        .collect();
    assert_eq!(missing.len(), 1, "{issues:?}");
    assert!(
        missing[0]
            .message
            .starts_with("[pools.f] fly.app is required"),
        "{missing:?}"
    );
}

#[test]
fn an_incomplete_pool_still_reports_a_bad_value() {
    let issues = validate(&fly_without_an_app("[\"\"]"));
    assert_eq!(pool_errors(&issues).len(), 1, "{issues:?}");
    assert!(
        issues
            .iter()
            .any(|issue| issue.item.as_deref() == Some("pools.f.fly.app")),
        "{issues:?}"
    );
}
