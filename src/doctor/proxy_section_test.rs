use std::path::{Path, PathBuf};

use super::*;
use crate::doctor::Status;

fn write_config(dir: &Path, providers: &str) -> PathBuf {
    let path = dir.join("proxy.toml");
    let log_dir = dir.join("logs");
    let text = format!("log_dir = \"{}\"\n{providers}", log_dir.display());
    std::fs::write(&path, text).unwrap();
    path
}

fn credential_block(name: &str, credentials: &Path) -> String {
    format!(
        "[providers.{name}]\nkind = \"upstream\"\nbackend = \"http://127.0.0.1:18173\"\ncredentials = \"{}\"\n",
        credentials.display()
    )
}

fn finding<'a>(findings: &'a [Finding], name: &str) -> &'a Finding {
    findings
        .iter()
        .find(|finding| finding.name == name)
        .unwrap_or_else(|| panic!("no {name} finding in {findings:#?}"))
}

fn write_credential(path: &Path, mode: u32) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, "{}").unwrap();
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, mode);
    std::fs::set_permissions(path, permissions).unwrap();
}

#[test]
fn missing_config_skips() {
    let tmp = tempfile::tempdir().unwrap();
    let findings = findings(Some(&tmp.path().join("proxy.toml")));
    assert_eq!(findings.len(), 1, "{findings:#?}");
    assert_eq!(findings[0].name, "config");
    assert_eq!(findings[0].status, Status::Skipped);
    assert_eq!(findings[0].fix, None);
}

#[test]
fn free_lock_warns() {
    let tmp = tempfile::tempdir().unwrap();
    let config = write_config(
        tmp.path(),
        "[providers.llama]\nkind = \"upstream\"\nbackend = \"http://127.0.0.1:18173\"\n",
    );
    let findings = findings(Some(&config));
    assert_eq!(finding(&findings, "config").status, Status::Ok);
    let lock = finding(&findings, "lock");
    assert_eq!(lock.status, Status::Warn, "{lock:#?}");
    assert_eq!(lock.fix.as_deref(), Some(LOCK_FIX));
    let health = finding(&findings, "health");
    assert_eq!(health.status, Status::Skipped);
    assert_eq!(health.fix.as_deref(), Some(HEALTH_FIX));
}

#[test]
fn missing_credential_file_fails_paths_only() {
    let tmp = tempfile::tempdir().unwrap();
    let credentials = tmp.path().join("creds/kimi.json");
    let config = write_config(tmp.path(), &credential_block("kimi", &credentials));
    let findings = findings(Some(&config));
    let credential = finding(&findings, "credential kimi");
    assert_eq!(credential.status, Status::Fail, "{credential:#?}");
    assert!(
        credential.detail.contains(&credentials.display().to_string()),
        "{credential:#?}"
    );
    assert_eq!(credential.fix.as_deref(), Some(CREDENTIAL_FIX));
    // Only the path is reported: never any would-be file contents.
    assert!(!credential.detail.contains("token"), "{credential:#?}");
    assert!(!credential.detail.contains('{'), "{credential:#?}");
}

#[test]
fn loose_credential_perms_warn() {
    let tmp = tempfile::tempdir().unwrap();
    let credentials = tmp.path().join("creds/kimi.json");
    write_credential(&credentials, 0o644);
    let config = write_config(tmp.path(), &credential_block("kimi", &credentials));
    let findings = findings(Some(&config));
    let credential = finding(&findings, "credential kimi");
    assert_eq!(credential.status, Status::Warn, "{credential:#?}");
    assert!(credential.detail.contains("644"), "{credential:#?}");
    assert!(credential.detail.contains("0600"), "{credential:#?}");
    let fix = format!("chmod 600 {}", credentials.display());
    assert_eq!(credential.fix.as_deref(), Some(fix.as_str()));
}

#[test]
fn tight_credential_perms_ok() {
    let tmp = tempfile::tempdir().unwrap();
    let credentials = tmp.path().join("creds/kimi.json");
    write_credential(&credentials, 0o600);
    let config = write_config(tmp.path(), &credential_block("kimi", &credentials));
    let findings = findings(Some(&config));
    let credential = finding(&findings, "credential kimi");
    assert_eq!(credential.status, Status::Ok, "{credential:#?}");
    assert_eq!(credential.fix, None);
    assert!(credential.detail.contains("0600"), "{credential:#?}");
}

#[test]
fn provider_without_a_file_skips() {
    let tmp = tempfile::tempdir().unwrap();
    let config = write_config(
        tmp.path(),
        "[providers.llama]\nkind = \"upstream\"\nbackend = \"http://127.0.0.1:18173\"\n",
    );
    let findings = findings(Some(&config));
    let credential = finding(&findings, "credential llama");
    assert_eq!(credential.status, Status::Skipped);
    assert_eq!(credential.detail, ENV_SKIP);
}
