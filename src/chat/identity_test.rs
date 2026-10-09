use super::*;

#[test]
fn mint_persists_a_deterministic_fingerprint() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();
    let first = ChatIdentity::ensure(dir).expect("mint identity");
    let reloaded = ChatIdentity::load(&dir.join(IDENTITY_FILE)).expect("reload identity");
    let fingerprint = first.fingerprint();
    assert_eq!(fingerprint, reloaded.fingerprint());
    assert_eq!(fingerprint.len(), 16);
    assert!(fingerprint.bytes().all(|b| b.is_ascii_hexdigit()));
    assert!(dir.join(".gitignore").exists());
}

#[test]
#[cfg(unix)]
fn identity_file_mode_is_600() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();
    let _ = ChatIdentity::ensure(dir).expect("mint identity");
    let meta = fs::metadata(dir.join(IDENTITY_FILE)).expect("stat identity");
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
}
