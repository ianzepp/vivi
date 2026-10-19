use super::*;
use crate::doctor::Status;

#[test]
fn missing_config_skips() {
    let temp = tempfile::tempdir().expect("tempdir");
    let report = findings(Some(&temp.path().join("missing-proxy.toml")));
    assert_eq!(report.len(), 1);
    assert_eq!(report[0].status, Status::Skipped);
    assert_eq!(report[0].detail, "proxy not configured");
}

fn provider(path: &std::path::Path) -> ProviderConfig {
    let text = format!(
        "[providers.codex]\nkind = \"native\"\nadapter = \"chatgpt-codex\"\ncredentials = {:?}\n",
        path.display().to_string()
    );
    config::parse_at(std::path::PathBuf::from("proxy.toml"), &text)
        .expect("valid proxy config")
        .providers
        .remove("codex")
        .expect("provider")
}

#[test]
fn credential_file_warns_when_mode_is_too_open() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("auth.json");
    std::fs::write(&path, "secret-shaped contents are never read").expect("write test file");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("set test mode");
    let finding = credential_finding("codex", &provider(&path));
    assert_eq!(finding.status, Status::Warn);
    assert!(finding.detail.contains("0644"));
    assert!(finding.detail.contains(&path.display().to_string()));
    assert!(!finding.detail.contains("secret-shaped"));
}

#[test]
fn credential_file_accepts_private_mode_and_key_command_skips() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("auth.json");
    std::fs::write(&path, "{}").expect("write test file");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("set test mode");
    let finding = credential_finding("codex", &provider(&path));
    assert_eq!(finding.status, Status::Ok);

    let key_cmd = config::parse_at(
        std::path::PathBuf::from("proxy.toml"),
        "[providers.codex]\nkind = \"native\"\nadapter = \"chatgpt-codex\"\nkey_cmd = \"read-secret\"\n",
    )
    .expect("valid proxy config")
    .providers
    .remove("codex")
    .expect("provider");
    assert_eq!(
        credential_finding("codex", &key_cmd).status,
        Status::Skipped
    );
}
