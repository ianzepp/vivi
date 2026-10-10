//! Integration tests for phase-03 admission by introduction (decided item 8):
//! the creator is admitted at creation, a member's vouch to postmaster admits
//! a new name+key and lands as a sequenced item, and unauthorized, malformed,
//! reserved, and conflicting vouches are refused with nothing appended.

use std::path::PathBuf;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::SigningKey;

use vivi_server::admissions::AdmissionsFile;
use vivi_server::crypto::StoreCipher;
use vivi_server::projects::{AppendItem, ProjectError, ProjectStore};

fn scratch_data_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "vivi-server-admissions-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn cipher() -> StoreCipher {
    StoreCipher::from_base64(&STANDARD.encode([0x42u8; 32])).unwrap()
}

fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn spki_pem(seed: u8) -> String {
    const ED25519_SPKI_PREFIX: &[u8] = &[
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
    ];
    let mut der = ED25519_SPKI_PREFIX.to_vec();
    der.extend_from_slice(key(seed).verifying_key().as_bytes());
    format!(
        "-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----",
        STANDARD.encode(der)
    )
}

/// Traveling id shape: `msg_` plus 24 lowercase hex.
fn traveling(n: u64) -> String {
    format!("msg_{n:08x}{:016x}", 0)
}

fn item(id: &str, from: &str, to: Option<&str>, body: &str) -> AppendItem {
    AppendItem {
        id: id.to_owned(),
        from: from.to_owned(),
        to: to.map(|t| t.to_owned()),
        subject: "governance".to_owned(),
        body: body.to_owned(),
        kind: None,
        in_reply_to: None,
        refs: Vec::new(),
    }
}

fn vouch_body(name: &str, seed: u8) -> String {
    format!(
        r#"{{"vouch":{{"name":"{name}","public_key_pem":{:?}}}}}"#,
        spki_pem(seed)
    )
}

fn admissions(data_dir: &PathBuf, project_id: &str) -> AdmissionsFile {
    let bytes = std::fs::read(
        data_dir
            .join("projects")
            .join(project_id)
            .join("admissions.json"),
    )
    .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[test]
fn creator_bootstrap_admits_creator() {
    let data_dir = scratch_data_dir("creator");
    let store = ProjectStore::new(&data_dir);
    let meta = store
        .create_project_with_creator(&spki_pem(1), "alice")
        .unwrap();
    let file = admissions(&data_dir, &meta.project_id);
    assert_eq!(file.admissions.len(), 1);
    assert_eq!(file.admissions[0].name, "alice");
    assert_eq!(file.admissions[0].admitted_by, "creator");
    assert!(!file.admissions[0].spki_der.is_empty());
}

#[test]
fn member_vouch_admits_new_name_and_lands_as_sequenced_item() {
    let data_dir = scratch_data_dir("vouch");
    let store = ProjectStore::new(&data_dir);
    let meta = store
        .create_project_with_creator(&spki_pem(1), "alice")
        .unwrap();
    let outcome = store
        .append(
            &cipher(),
            &meta.project_id,
            &item(
                &traveling(1),
                "alice",
                Some("postmaster"),
                &vouch_body("bob", 2),
            ),
        )
        .unwrap();
    assert_eq!(outcome.sequence, 1);
    assert!(!outcome.duplicate);

    let file = admissions(&data_dir, &meta.project_id);
    assert_eq!(file.admissions.len(), 2);
    let bob = file.admissions.iter().find(|a| a.name == "bob").unwrap();
    assert_eq!(bob.admitted_by, "alice");

    // Trust is transitive one hop at a time: the newly admitted member can
    // vouch for the next key.
    let outcome = store
        .append(
            &cipher(),
            &meta.project_id,
            &item(
                &traveling(2),
                "bob",
                Some("postmaster"),
                &vouch_body("carol", 3),
            ),
        )
        .unwrap();
    assert_eq!(outcome.sequence, 2);
    assert!(
        admissions(&data_dir, &meta.project_id)
            .admissions
            .iter()
            .any(|a| a.name == "carol")
    );
}

#[test]
fn vouch_from_unknown_voucher_is_refused_with_nothing_appended() {
    let data_dir = scratch_data_dir("unknown-voucher");
    let store = ProjectStore::new(&data_dir);
    let meta = store
        .create_project_with_creator(&spki_pem(1), "alice")
        .unwrap();
    let err = store
        .append(
            &cipher(),
            &meta.project_id,
            &item(
                &traveling(1),
                "mallory",
                Some("postmaster"),
                &vouch_body("bob", 2),
            ),
        )
        .unwrap_err();
    assert!(matches!(err, ProjectError::BadRequest(_)), "got {err:?}");
    assert_eq!(
        store
            .read_after(&cipher(), &meta.project_id, 0)
            .unwrap()
            .records
            .len(),
        0
    );
    assert_eq!(admissions(&data_dir, &meta.project_id).admissions.len(), 1);
}

#[test]
fn malformed_vouch_body_is_refused_with_nothing_appended() {
    let data_dir = scratch_data_dir("malformed");
    let store = ProjectStore::new(&data_dir);
    let meta = store
        .create_project_with_creator(&spki_pem(1), "alice")
        .unwrap();
    for body in ["not json at all", r#"{"vouch":{"name":"bob"}}"#] {
        let err = store
            .append(
                &cipher(),
                &meta.project_id,
                &item(&traveling(1), "alice", Some("postmaster"), body),
            )
            .unwrap_err();
        assert!(matches!(err, ProjectError::BadRequest(_)), "got {err:?}");
    }
    assert_eq!(
        store
            .read_after(&cipher(), &meta.project_id, 0)
            .unwrap()
            .records
            .len(),
        0
    );
    assert_eq!(admissions(&data_dir, &meta.project_id).admissions.len(), 1);
}

#[test]
fn vouching_the_name_postmaster_is_refused() {
    let data_dir = scratch_data_dir("reserved");
    let store = ProjectStore::new(&data_dir);
    let meta = store
        .create_project_with_creator(&spki_pem(1), "alice")
        .unwrap();
    let err = store
        .append(
            &cipher(),
            &meta.project_id,
            &item(
                &traveling(1),
                "alice",
                Some("postmaster"),
                &vouch_body("postmaster", 2),
            ),
        )
        .unwrap_err();
    assert!(matches!(err, ProjectError::BadRequest(_)), "got {err:?}");
    assert_eq!(admissions(&data_dir, &meta.project_id).admissions.len(), 1);
}

#[test]
fn re_vouch_of_same_name_and_key_is_idempotent() {
    let data_dir = scratch_data_dir("idempotent");
    let store = ProjectStore::new(&data_dir);
    let meta = store
        .create_project_with_creator(&spki_pem(1), "alice")
        .unwrap();
    let body = vouch_body("bob", 2);
    store
        .append(
            &cipher(),
            &meta.project_id,
            &item(&traveling(1), "alice", Some("postmaster"), &body),
        )
        .unwrap();
    // A replayed vouch (new item id, same name+key) succeeds and does not
    // duplicate the admission.
    store
        .append(
            &cipher(),
            &meta.project_id,
            &item(&traveling(2), "alice", Some("postmaster"), &body),
        )
        .unwrap();
    let file = admissions(&data_dir, &meta.project_id);
    assert_eq!(
        file.admissions.iter().filter(|a| a.name == "bob").count(),
        1
    );
}

#[test]
fn same_name_with_different_key_conflicts() {
    let data_dir = scratch_data_dir("conflict");
    let store = ProjectStore::new(&data_dir);
    let meta = store
        .create_project_with_creator(&spki_pem(1), "alice")
        .unwrap();
    store
        .append(
            &cipher(),
            &meta.project_id,
            &item(
                &traveling(1),
                "alice",
                Some("postmaster"),
                &vouch_body("bob", 2),
            ),
        )
        .unwrap();
    let err = store
        .append(
            &cipher(),
            &meta.project_id,
            &item(
                &traveling(2),
                "alice",
                Some("postmaster"),
                &vouch_body("bob", 3),
            ),
        )
        .unwrap_err();
    assert!(matches!(err, ProjectError::Conflict(_)), "got {err:?}");
    // Only the first vouch was appended; bob still holds the first key.
    assert_eq!(
        store
            .read_after(&cipher(), &meta.project_id, 0)
            .unwrap()
            .records
            .len(),
        1
    );
    assert_eq!(admissions(&data_dir, &meta.project_id).admissions.len(), 2);
}
