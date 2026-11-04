use super::credentials_problem;
use crate::proxy::config::{self, ProviderConfig};

fn provider(extra: &str) -> ProviderConfig {
    let text = format!(
        "[providers.p]\nkind = \"upstream\"\nbackend = \"http://127.0.0.1:18173\"\n{extra}\n"
    );
    config::parse_at(std::path::PathBuf::from("/tmp/proxy.toml"), &text)
        .expect("parse")
        .providers
        .remove("p")
        .expect("provider p")
}

#[test]
fn a_provider_with_no_credentials_has_no_problem() {
    assert_eq!(credentials_problem(&provider("")), None);
}

#[test]
fn a_key_command_that_prints_a_secret_is_fine() {
    assert_eq!(
        credentials_problem(&provider("key_cmd = \"printf secret\"")),
        None
    );
}

#[test]
fn a_failing_key_command_is_a_problem_and_never_prints_the_secret() {
    let problem = credentials_problem(&provider("key_cmd = \"echo oops >&2; exit 3\"")).unwrap();
    assert!(problem.contains("key_cmd failed: oops"), "{problem}");
    let empty = credentials_problem(&provider("key_cmd = \"true\"")).unwrap();
    assert!(empty.contains("empty secret"), "{empty}");
}

#[test]
fn a_missing_credentials_file_is_a_problem_and_a_present_one_is_not() {
    let dir = tempfile::tempdir().unwrap();
    let gone = dir.path().join("key.json");
    let missing = provider(&format!("credentials = \"{}\"", gone.display()));
    let problem = credentials_problem(&missing).unwrap();
    assert!(problem.contains("no credential file found"), "{problem}");
    std::fs::write(&gone, "{}").unwrap();
    assert_eq!(credentials_problem(&missing), None);
}

#[test]
fn the_fallback_credentials_file_counts() {
    let dir = tempfile::tempdir().unwrap();
    let primary = dir.path().join("primary.json");
    let fallback = dir.path().join("fallback.json");
    std::fs::write(&fallback, "{}").unwrap();
    let both = provider(&format!(
        "credentials = \"{}\"\nfallback_credentials = \"{}\"",
        primary.display(),
        fallback.display()
    ));
    assert_eq!(credentials_problem(&both), None);
}
