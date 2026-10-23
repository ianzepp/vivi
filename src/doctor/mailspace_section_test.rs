use super::*;
use crate::doctor::Status;
use crate::letter::fixture::{task_body, task_body_with};
use crate::letter::work::TaskFront;
use crate::mailspace::{Mailspace, SendRequest};

fn open_mailspace() -> (Mailspace, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    (mailspace, tmp)
}

fn with_roster() -> (Mailspace, tempfile::TempDir) {
    let (mut mailspace, tmp) = open_mailspace();
    mailspace.add_identity("ceo").unwrap();
    mailspace.add_identity("cto").unwrap();
    (mailspace, tmp)
}

fn status_of<'a>(findings: &'a [Finding], name: &str) -> &'a Finding {
    findings
        .iter()
        .find(|finding| finding.name == name)
        .unwrap_or_else(|| panic!("no {name} finding in {findings:#?}"))
}

#[test]
fn mailspace_without_wal_sidecar_skips_store_checks() {
    let (_mailspace, tmp) = open_mailspace();
    let findings = findings(Some(tmp.path()));
    let names: Vec<&str> = findings.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["config", "store", "unabsorbed", "done_when"]);
    assert_eq!(status_of(&findings, "config").status, Status::Ok);
    for name in ["store", "unabsorbed", "done_when"] {
        assert_eq!(status_of(&findings, name).status, Status::Skipped);
    }
}

#[test]
fn broken_config_fails() {
    let (_mailspace, tmp) = open_mailspace();
    let config = tmp.path().join(".vivi/mailspace.toml");
    std::fs::write(&config, "not = [valid\napi_key = 'SECRETTOKEN123'\n").unwrap();
    let findings = findings(Some(tmp.path()));
    assert_eq!(findings.len(), 1, "{findings:#?}");
    assert_eq!(findings[0].name, "config");
    assert_eq!(findings[0].status, Status::Fail, "{findings:#?}");
    assert!(findings[0].fix.is_some(), "{findings:#?}");
    // The broken config's contents never reach the finding.
    let output = serde_json::to_string(&findings).unwrap();
    assert!(!output.contains("SECRETTOKEN123"), "{output}");
}

#[test]
fn legacy_mailspace_check_does_not_create_identity_store_or_gitignore() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join(".vivi");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("mailspace.toml"), "name = 'legacy'\n").unwrap();
    let report = findings(Some(tmp.path()));
    assert_eq!(report[0].status, Status::Ok, "{report:#?}");
    assert!(!dir.join("mailspace.id").exists());
    assert!(
        !dir.join("mailspace.toml")
            .with_extension("toml.lock")
            .exists()
    );
    assert!(!dir.join(".gitignore").exists());
    assert!(!dir.join("mail.sqlite").exists());
}

#[test]
fn readonly_wal_inspection_skips_without_creating_a_missing_shm_sidecar() {
    let tmp = tempfile::tempdir().unwrap();
    Mailspace::init(Some(tmp.path())).unwrap();
    let db = tmp.path().join(".vivi/mail.sqlite");
    let shm = db.with_extension("sqlite-shm");
    let wal = db.with_extension("sqlite-wal");
    let connection = rusqlite::Connection::open(&db).unwrap();
    connection
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
    drop(connection);
    let _ = std::fs::remove_file(&shm);
    let _ = std::fs::remove_file(&wal);
    assert!(!shm.exists(), "test setup requires no -shm sidecar");
    let database_before = std::fs::read(&db).unwrap();

    let report = findings(Some(tmp.path()));
    for name in ["store", "unabsorbed", "done_when"] {
        assert_eq!(status_of(&report, name).status, Status::Skipped);
    }
    assert!(!shm.exists(), "doctor must not create a -shm sidecar");
    assert_eq!(std::fs::read(&db).unwrap(), database_before);
}

#[test]
fn older_schema_warns() {
    let (mailspace, tmp) = open_mailspace();
    let conn = rusqlite::Connection::open(mailspace.store_path()).unwrap();
    conn.execute(
        "UPDATE storage_metadata SET value = '8' WHERE key = 'schema_version'",
        [],
    )
    .unwrap();
    let findings = findings(Some(tmp.path()));
    let store = status_of(&findings, "store");
    assert_eq!(store.status, Status::Warn, "{store:#?}");
    drop(conn);
}

#[test]
fn ready_item_without_done_when_warns() {
    let (mailspace, tmp) = with_roster();
    let handle = mailspace
        .send(SendRequest {
            from: "ceo".into(),
            to: vec!["cto".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "vague task".into(),
            body: task_body("no clause here"),
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle;
    let findings = findings(Some(tmp.path()));
    let done_when = status_of(&findings, "done_when");
    assert_eq!(done_when.status, Status::Warn, "{done_when:#?}");
    assert!(done_when.detail.contains(&handle), "{done_when:#?}");
}

#[test]
fn done_when_matches_step_and_ignores_blocked_items() {
    let (mailspace, tmp) = with_roster();
    let ready = mailspace
        .send(SendRequest {
            from: "ceo".into(),
            to: vec!["cto".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "ready task".into(),
            body: task_body("no clause here"),
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle;
    let blocked = mailspace
        .send(SendRequest {
            from: "ceo".into(),
            to: vec!["cto".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "blocked task".into(),
            body: task_body_with(
                &TaskFront {
                    depends_on: vec![ready.clone()],
                    ..TaskFront::default()
                },
                "also no clause here",
            ),
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle;

    let report = findings(Some(tmp.path()));
    let done_when = status_of(&report, "done_when");
    assert_eq!(done_when.status, Status::Warn, "{done_when:#?}");
    assert!(done_when.detail.contains(&ready), "{done_when:#?}");
    assert!(!done_when.detail.contains(&blocked), "{done_when:#?}");
}

#[test]
fn letter_invalid_item_is_reported() {
    let (mailspace, tmp) = with_roster();
    let domain = mailspace.address_domain();
    let bytes = format!(
        "From: ceo@{domain}\r\nTo: cto@{domain}\r\nSubject: broken\r\n\
         Message-ID: <broken@x.test>\r\nDate: Sun, 11 Oct 2026 12:00:00 +0000\r\n\
         X-Vivi-Kind: need\r\n\r\n+++\ndone_when = 5\n+++\nbody\n"
    )
    .into_bytes();
    let storage = mailspace.storage().unwrap();
    let stored = storage
        .ingest_message(
            &crate::storage::MessageIngestRequest {
                account: "cto".into(),
                local_role: "needs".into(),
                read_state: false,
                starred: false,
                message_id_hint: None,
                presented_id: None,
                seed_hint: "cto\0broken".into(),
                remote: None,
            },
            &bytes,
        )
        .unwrap();
    let handle = storage.display_handle(&stored.message_id).unwrap();
    drop(storage);
    mailspace
        .backlog_attach_letter("need", &stored.message_id, "broken", &bytes)
        .unwrap();
    mailspace
        .backlog_settle_pending_dependencies(&[(stored.message_id.clone(), stored.content_id)])
        .unwrap();

    let findings = findings(Some(tmp.path()));
    let done_when = status_of(&findings, "done_when");
    assert_eq!(done_when.status, Status::Warn, "{done_when:#?}");
    assert!(
        done_when.detail.contains("letter_invalid"),
        "{done_when:#?}"
    );
    assert!(done_when.detail.contains(&handle), "{done_when:#?}");
}
