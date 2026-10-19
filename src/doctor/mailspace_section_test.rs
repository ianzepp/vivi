use super::*;
use crate::doctor::Status;
use crate::mailspace::SendRequest;

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
fn healthy_mailspace_is_all_ok() {
    let (_mailspace, tmp) = open_mailspace();
    let findings = findings(Some(tmp.path()), Utc::now());
    let names: Vec<&str> = findings.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["config", "store", "unabsorbed", "done_when"]);
    for finding in &findings {
        assert_eq!(finding.status, Status::Ok, "{finding:#?}");
    }
}

#[test]
fn broken_config_fails() {
    let (_mailspace, tmp) = open_mailspace();
    let config = tmp.path().join(".vivi/mailspace.toml");
    std::fs::write(&config, "not = [valid").unwrap();
    let findings = findings(Some(tmp.path()), Utc::now());
    assert_eq!(findings.len(), 1, "{findings:#?}");
    assert_eq!(findings[0].name, "config");
    assert_eq!(findings[0].status, Status::Fail, "{findings:#?}");
    assert!(findings[0].fix.is_some(), "{findings:#?}");
    // The broken config's contents never reach the finding.
    assert!(!findings[0].detail.contains("[valid"), "{findings:#?}");
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
    drop(conn);
    let findings = findings(Some(tmp.path()), Utc::now());
    let store = status_of(&findings, "store");
    assert_eq!(store.status, Status::Warn, "{store:#?}");
}

#[test]
fn need_without_done_when_warns() {
    let (mailspace, tmp) = with_roster();
    let handle = mailspace
        .send(SendRequest {
            from: "ceo".into(),
            to: vec!["cto".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "vague need".into(),
            body: "no clause here".into(),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
            depends_on: Vec::new(),
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle;
    let findings = findings(Some(tmp.path()), Utc::now());
    let done_when = status_of(&findings, "done_when");
    assert_eq!(done_when.status, Status::Warn, "{done_when:#?}");
    assert!(done_when.detail.contains(&handle), "{done_when:#?}");
}
