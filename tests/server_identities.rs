//! Integration tests for the phase-03 identity registry (I1): flat
//! key-bound names unique per project, the reserved `postmaster` name, name
//! charset/length caps, and bearer-token round-trip failing closed.

use std::path::PathBuf;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::{Signer, SigningKey};

use vivi::server::auth::{ChallengeBook, canonical_spki_hex, key_fingerprint, parse_verifying_key};
use vivi::server::identities::{IdentityRegistry, MAX_IDENTITY_LEN, RefreshError, RegisterError};

fn scratch_data_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "vivi-server-identities-i1-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn test_key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

/// Build the SPKI PEM (openssl `BEGIN PUBLIC KEY` shape) for a test key.
fn spki_pem(key: &SigningKey) -> String {
    const ED25519_SPKI_PREFIX: &[u8] = &[
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
    ];
    let mut der = ED25519_SPKI_PREFIX.to_vec();
    der.extend_from_slice(key.verifying_key().as_bytes());
    format!(
        "-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----",
        STANDARD.encode(der)
    )
}

fn registry(name: &str) -> IdentityRegistry {
    IdentityRegistry::new(&scratch_data_dir(name))
}

/// A canonical mailspace id with a distinct 8-char prefix per `n`.
fn ms(n: u8) -> String {
    format!("{n:08x}-1111-4222-8333-444455556666")
}

#[test]
fn register_binds_name_to_fingerprint_and_spki() {
    let reg = registry("bind");
    let key = test_key(1);
    let out = reg
        .register("p1", "mind-on-pharos", &spki_pem(&key), &ms(1))
        .unwrap();
    assert_eq!(out.name, "mind-on-pharos");
    let parsed = parse_verifying_key(&spki_pem(&key)).unwrap();
    assert_eq!(out.fingerprint, key_fingerprint(&parsed).unwrap());
    assert_eq!(out.fingerprint.len(), 64);
    let record = reg.verify_bearer("p1", &out.token).unwrap();
    assert_eq!(record.name, "mind-on-pharos");
    assert_eq!(record.fingerprint, out.fingerprint);
    assert_eq!(record.spki_der, canonical_spki_hex(&parsed).unwrap());
    assert_eq!(out.mailspace_id, ms(1));
    assert_eq!(record.mailspace_id.as_deref(), Some(ms(1).as_str()));
    assert_eq!(record.mailspace_prefix(), Some("00000001"));
}

#[test]
fn malformed_mailspace_ids_are_refused() {
    let reg = registry("bad-ms-id");
    let good = "3f9a1c7e-1111-4222-8333-44445555aaaa".to_owned();
    let bad = [
        String::new(),
        good.to_uppercase(),
        good.replace('-', ""),
        good[..35].to_owned(),
        format!("{good}0"),
        good.replace('1', "g"),
    ];
    for id in &bad {
        match reg.register("p1", "hand", &spki_pem(&test_key(9)), id) {
            Err(RegisterError::InvalidMailspaceId) => {}
            other => panic!("expected InvalidMailspaceId for {id:?}, got {other:?}"),
        }
    }
    assert!(reg.list("p1").is_empty(), "nothing was registered");
}

#[test]
fn a_taken_prefix_is_refused_and_the_name_is_checked_first() {
    let reg = registry("prefix-taken");
    reg.register("p1", "hand", &spki_pem(&test_key(1)), &ms(1))
        .unwrap();
    // Same prefix, different rest of the UUID, different name: refused.
    let same_prefix = "00000001-aaaa-4bbb-8ccc-ddddeeeeffff";
    match reg.register("p1", "other", &spki_pem(&test_key(2)), same_prefix) {
        Err(RegisterError::PrefixTaken) => {}
        other => panic!("expected PrefixTaken, got {other:?}"),
    }
    assert_eq!(reg.list("p1").len(), 1, "nothing was registered");
    // Name already registered wins over the taken prefix.
    match reg.register("p1", "hand", &spki_pem(&test_key(3)), same_prefix) {
        Err(RegisterError::NameTaken) => {}
        other => panic!("expected NameTaken, got {other:?}"),
    }
    // A free prefix registers, and the same prefix in another project is fine.
    reg.register("p1", "other", &spki_pem(&test_key(2)), &ms(2))
        .unwrap();
    reg.register("p2", "other", &spki_pem(&test_key(2)), &ms(1))
        .unwrap();
}

#[test]
fn a_legacy_identities_file_loads_without_a_mailspace_id() {
    let dir = scratch_data_dir("legacy");
    let reg = IdentityRegistry::new(&dir);
    let out = reg
        .register("p1", "old", &spki_pem(&test_key(1)), &ms(1))
        .unwrap();
    let path = dir.join("projects").join("p1").join("identities.json");
    let mut file: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    file["identities"][0]
        .as_object_mut()
        .unwrap()
        .remove("mailspace_id")
        .unwrap();
    std::fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();

    let record = reg.verify_bearer("p1", &out.token).unwrap();
    assert_eq!(record.mailspace_id, None);
    assert_eq!(record.mailspace_prefix(), None);
    let members = reg.list("p1");
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].mailspace_id, None);
    // A legacy identity holds no prefix, so its old prefix is free to claim.
    reg.register("p1", "new", &spki_pem(&test_key(2)), &ms(1))
        .unwrap();
}

#[test]
fn duplicate_name_is_refused_even_for_same_key() {
    let reg = registry("dup");
    let pem = spki_pem(&test_key(1));
    reg.register("p1", "hand", &pem, &ms(1)).unwrap();
    for pem in [&pem, &spki_pem(&test_key(2))] {
        match reg.register("p1", "hand", pem, &ms(2)) {
            Err(RegisterError::NameTaken) => {}
            other => panic!("expected NameTaken, got {other:?}"),
        }
    }
}

#[test]
fn postmaster_is_reserved_any_case() {
    let reg = registry("postmaster");
    for name in ["postmaster", "Postmaster", "POSTMASTER", "PostMaster"] {
        match reg.register("p1", name, &spki_pem(&test_key(3)), &ms(1)) {
            Err(RegisterError::ReservedName) => {}
            other => panic!("expected ReservedName for {name}, got {other:?}"),
        }
    }
}

#[test]
fn invalid_names_are_refused() {
    let reg = registry("invalid-names");
    let pem = spki_pem(&test_key(4));
    let bad: [String; 6] = [
        "".to_owned(),
        "   ".to_owned(),
        "mind@pharos".to_owned(),
        "mind on pharos".to_owned(),
        "mind/onedot".to_owned(),
        "a".repeat(MAX_IDENTITY_LEN + 1),
    ];
    for name in &bad {
        match reg.register("p1", &name, &pem, &ms(1)) {
            Err(RegisterError::InvalidName) => {}
            other => panic!("expected InvalidName for {name:?}, got {other:?}"),
        }
    }
    let cap = "a".repeat(MAX_IDENTITY_LEN);
    reg.register("p1", &cap, &pem, &ms(1)).unwrap();
}

#[test]
fn bearer_round_trips_and_fails_closed() {
    let reg = registry("bearer");
    let out = reg
        .register("p1", "hand", &spki_pem(&test_key(5)), &ms(1))
        .unwrap();
    let record = reg.verify_bearer("p1", &out.token).unwrap();
    assert_eq!(record.name, "hand");
    assert!(reg.verify_bearer("p1", "not-a-real-token").is_none());
    // A token minted in one project does not open another.
    assert!(reg.verify_bearer("p2", &out.token).is_none());
}

/// Issue a challenge for `key` in `project` and sign it with `signer`.
fn signed_challenge(
    book: &ChallengeBook,
    project: &str,
    key: &SigningKey,
    signer: &SigningKey,
) -> (String, String) {
    let parsed = parse_verifying_key(&spki_pem(key)).unwrap();
    let issued = book
        .issue(project, &key_fingerprint(&parsed).unwrap())
        .unwrap();
    let bytes = STANDARD.decode(&issued.challenge).unwrap();
    (
        issued.challenge_id,
        STANDARD.encode(signer.sign(&bytes).to_bytes()),
    )
}

#[test]
fn refresh_mints_a_new_token_and_leaves_the_old_one_valid() {
    let reg = registry("refresh");
    let book = ChallengeBook::default();
    let key = test_key(6);
    let out = reg.register("p1", "hand", &spki_pem(&key), &ms(1)).unwrap();
    let (challenge_id, signature) = signed_challenge(&book, "p1", &key, &key);
    let (token, expires_at) = reg
        .refresh_token("p1", "hand", &challenge_id, &signature, &book)
        .unwrap();
    assert_ne!(token, out.token);
    assert!(!expires_at.is_empty());
    assert_eq!(reg.verify_bearer("p1", &token).unwrap().name, "hand");
    assert_eq!(reg.verify_bearer("p1", &out.token).unwrap().name, "hand");
    // The challenge was spent.
    assert!(matches!(
        reg.refresh_token("p1", "hand", &challenge_id, &signature, &book),
        Err(RefreshError::Unauthorized)
    ));
}

#[test]
fn refresh_refuses_unknown_name_wrong_key_and_other_project() {
    let reg = registry("refresh-refuse");
    let book = ChallengeBook::default();
    let key = test_key(7);
    let other = test_key(8);
    reg.register("p1", "hand", &spki_pem(&key), &ms(1)).unwrap();
    reg.register("p1", "other", &spki_pem(&other), &ms(2))
        .unwrap();
    let cases = [
        // (project to refresh in, name, challenge key, signer)
        ("p1", "nobody", &key, &key),
        ("p1", "hand", &key, &other),
        ("p1", "hand", &other, &other),
        ("p2", "hand", &key, &key),
    ];
    for (project, name, challenge_key, signer) in cases {
        let (challenge_id, signature) = signed_challenge(&book, "p1", challenge_key, signer);
        assert!(
            matches!(
                reg.refresh_token(project, name, &challenge_id, &signature, &book),
                Err(RefreshError::Unauthorized)
            ),
            "{project} {name}"
        );
    }
}
