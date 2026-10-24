//! Integration tests for phase-03 admission by introduction (decided item 8):
//! the creator is admitted at creation, a member's vouch to postmaster admits
//! a new name+key and lands as a sequenced item, and unauthorized, malformed,
//! reserved, and conflicting vouches are refused with nothing appended. A
//! vouch is a letter to postmaster whose body (after the first blank line)
//! is the vouch JSON.

use std::path::PathBuf;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::SigningKey;

use vivi_server::admissions::AdmissionsFile;
use vivi_server::projects::{AppendItem, ProjectError, ProjectStore, Viewer};

/// The sender of every letter in this file; it has no mailspace prefix.
fn alice() -> Viewer<'static> {
    Viewer {
        name: "alice",
        prefix: None,
    }
}

fn scratch_data_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "vivi-server-admissions-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
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

/// A governance letter: headers, a blank line, then `body`. The letter is a
/// format-1 mail letter; the vouch JSON rides as its prose body.
fn item(id: &str, from: &str, to: Option<&str>, body: &str) -> AppendItem {
    let raw = format!(
        "From: {from}@p.local\nTo: postmaster@p.local\nSubject: governance\n\
         X-Vivi-Kind: mail\nX-Vivi-Format: 1\n\n{body}"
    );
    AppendItem::new(id, from, to, None, raw.into_bytes()).unwrap()
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
            .read_after_scoped(&meta.project_id, 0, &alice())
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
                &meta.project_id,
                &item(&traveling(1), "alice", Some("postmaster"), body),
            )
            .unwrap_err();
        assert!(matches!(err, ProjectError::BadRequest(_)), "got {err:?}");
    }
    assert_eq!(
        store
            .read_after_scoped(&meta.project_id, 0, &alice())
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
            &meta.project_id,
            &item(&traveling(1), "alice", Some("postmaster"), &body),
        )
        .unwrap();
    // A replayed vouch (new item id, same name+key) succeeds and does not
    // duplicate the admission.
    store
        .append(
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
            .read_after_scoped(&meta.project_id, 0, &alice())
            .unwrap()
            .records
            .len(),
        1
    );
    assert_eq!(admissions(&data_dir, &meta.project_id).admissions.len(), 2);
}

fn raw_letter(id: &str, raw: Vec<u8>) -> AppendItem {
    AppendItem::new(id, "alice", Some("postmaster"), None, raw).unwrap()
}

/// The vouch body is the bytes after the first blank line, whether the
/// letter uses CRLF or LF line ends, and the letter is stored byte for byte.
#[test]
fn vouch_body_is_read_after_the_first_blank_line_in_either_line_ending() {
    let data_dir = scratch_data_dir("line-endings");
    let store = ProjectStore::new(&data_dir);
    let meta = store
        .create_project_with_creator(&spki_pem(1), "alice")
        .unwrap();
    let crlf = format!(
        "From: alice@p.local\r\nSubject: vouch\r\nX-Vivi-Kind: mail\r\nX-Vivi-Format: 1\r\n\r\n{}\r\n",
        vouch_body("bob", 2)
    );
    let lf = format!(
        "From: alice@p.local\nSubject: vouch\nX-Vivi-Kind: mail\nX-Vivi-Format: 1\n\n{}",
        vouch_body("carol", 3)
    );
    for (n, raw) in [(1, crlf), (2, lf)] {
        store
            .append(
                &meta.project_id,
                &raw_letter(&traveling(n), raw.clone().into_bytes()),
            )
            .unwrap();
    }
    let names: Vec<String> = admissions(&data_dir, &meta.project_id)
        .admissions
        .into_iter()
        .map(|a| a.name)
        .collect();
    assert!(names.contains(&"bob".to_owned()) && names.contains(&"carol".to_owned()));
}

#[test]
fn vouch_letters_without_a_usable_body_are_refused_with_nothing_appended() {
    let data_dir = scratch_data_dir("bad-letters");
    let store = ProjectStore::new(&data_dir);
    let meta = store
        .create_project_with_creator(&spki_pem(1), "alice")
        .unwrap();
    let json = vouch_body("bob", 2);
    let mut non_utf8 = b"Subject: vouch\nX-Vivi-Kind: mail\nX-Vivi-Format: 1\n\n".to_vec();
    non_utf8.extend_from_slice(&[0xff, 0xfe]);
    non_utf8.extend_from_slice(json.as_bytes());
    let cases: Vec<Vec<u8>> = vec![
        // No blank line at all: headers only, JSON as the last header line.
        format!("Subject: vouch\nX-Vivi-Kind: mail\nX-Vivi-Format: 1\n{json}").into_bytes(),
        non_utf8,
        // The first blank line is the LF-LF; the body is not the JSON.
        format!("Subject: vouch\nX-Vivi-Kind: mail\nX-Vivi-Format: 1\n\nnote\r\n\r\n{json}")
            .into_bytes(),
    ];
    for (n, raw) in cases.into_iter().enumerate() {
        let err = store
            .append(&meta.project_id, &raw_letter(&traveling(n as u64 + 1), raw))
            .unwrap_err();
        assert!(matches!(err, ProjectError::BadRequest(_)), "got {err:?}");
    }
    assert_eq!(admissions(&data_dir, &meta.project_id).admissions.len(), 1);
    let page = store
        .read_after_scoped(&meta.project_id, 0, &alice())
        .unwrap();
    assert!(page.records.is_empty());
}

/// A replay of a vouch whose admission landed but whose letter row did not
/// (crash between the two writes) converges: the admission is a no-op and
/// the letter stores.
#[test]
fn vouch_replay_after_admission_only_converges() {
    let data_dir = scratch_data_dir("vouch-crash");
    let store = ProjectStore::new(&data_dir);
    let meta = store
        .create_project_with_creator(&spki_pem(1), "alice")
        .unwrap();
    store
        .admissions()
        .admit(&meta.project_id, "bob", &spki_pem(2), "alice")
        .unwrap();
    let outcome = store
        .append(
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
    assert_eq!(admissions(&data_dir, &meta.project_id).admissions.len(), 2);
}
