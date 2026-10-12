use super::*;

#[test]
fn mint_persists_a_deterministic_fingerprint() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();
    let first = SyncIdentity::ensure(dir).expect("mint identity");
    let reloaded = SyncIdentity::load(&identity_path(dir)).expect("reload identity");
    let fingerprint = first.fingerprint();
    assert_eq!(fingerprint, reloaded.fingerprint());
    assert_eq!(fingerprint.len(), 16);
    assert!(fingerprint.bytes().all(|b| b.is_ascii_hexdigit()));
    assert!(dir.join(IDENTITY_DIR).join(".gitignore").exists());
}

#[test]
fn the_key_stays_where_an_existing_mailspace_minted_it() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();
    SyncIdentity::ensure(dir).expect("mint identity");

    assert!(dir.join("chat").join("identity.ed25519").is_file());
}

#[test]
#[cfg(unix)]
fn identity_file_mode_is_600() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();
    let _ = SyncIdentity::ensure(dir).expect("mint identity");
    let meta = fs::metadata(identity_path(dir)).expect("stat identity");
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
}

#[test]
fn public_key_pem_matches_spki_shape() {
    const ED25519_SPKI_PREFIX: &[u8] = &[
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
    ];
    let tmp = tempfile::tempdir().expect("tempdir");
    let identity = SyncIdentity::ensure(tmp.path()).expect("mint identity");
    let reloaded = SyncIdentity::load(&identity_path(tmp.path())).expect("reload");
    let pem = identity.public_key_pem();
    assert!(pem.starts_with("-----BEGIN PUBLIC KEY-----\n"));
    assert!(pem.ends_with("\n-----END PUBLIC KEY-----"));
    assert_eq!(pem, reloaded.public_key_pem());
    let body: String = pem.lines().filter(|l| !l.contains("-----")).collect();
    let der = crate::b64::decode(&body).expect("der decodes");
    assert_eq!(der.len(), 44, "12-byte SPKI prefix + 32-byte key");
    assert_eq!(&der[..12], ED25519_SPKI_PREFIX);
}
