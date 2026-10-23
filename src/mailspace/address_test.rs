use std::collections::BTreeSet;
use std::fs;

use super::*;
use crate::mailspace::{Mailspace, SendRequest};

const REMOTE: &str = "mind@3f9a1c7e.local";

fn space(roles: &[&str]) -> (tempfile::TempDir, Mailspace) {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    for role in roles {
        mailspace.add_identity(role).unwrap();
    }
    (tmp, mailspace)
}

fn config_path(tmp: &tempfile::TempDir) -> std::path::PathBuf {
    tmp.path().join(".vivi/mailspace.toml")
}

fn send(mailspace: &Mailspace, to: &[&str], role: &str, kind: Option<&str>) -> SendRequest {
    let _ = mailspace;
    SendRequest {
        from: "mind".into(),
        to: to.iter().map(ToString::to_string).collect(),
        cc: Vec::new(),
        bcc: Vec::new(),
        subject: "subject".into(),
        body: crate::letter::fixture::body_for(kind.unwrap_or(""), "body\n"),
        role: role.into(),
        kind: kind.map(String::from),
        reply_to: None,
        depends_on: Vec::new(),
    }
}

fn id_lines(raw: &str) -> usize {
    raw.lines().filter(|line| line.starts_with("id = ")).count()
}

#[test]
fn a_new_mailspace_mints_a_uuid_v4_once_and_keeps_it() {
    let (tmp, mut mailspace) = space(&["mind"]);
    let id = mailspace.mailspace_id().to_string();

    assert_eq!(id.len(), 36);
    assert!(crate::mailspace::config_file::is_canonical_id(&id), "{id}");
    assert_eq!(id.as_bytes()[14], b'4', "version 4: {id}");
    assert!(
        matches!(id.as_bytes()[19], b'8' | b'9' | b'a' | b'b'),
        "{id}"
    );
    assert_eq!(mailspace.address_prefix(), &id[..8]);
    assert_eq!(mailspace.address_domain(), format!("{}.local", &id[..8]));

    mailspace.add_identity("hand").unwrap();
    mailspace.set_description(Some("rewritten".into())).unwrap();
    for _ in 0..3 {
        let reopened = Mailspace::open_root(tmp.path().to_path_buf()).unwrap();
        assert_eq!(reopened.mailspace_id(), id);
    }
    let raw = fs::read_to_string(config_path(&tmp)).unwrap();
    assert_eq!(id_lines(&raw), 1, "{raw}");
    assert!(!raw.contains(".tmp"));
    assert!(!config_path(&tmp).with_extension("toml.tmp").exists());
}

#[test]
fn an_existing_config_without_an_id_gets_one_and_is_otherwise_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir_all(tmp.path().join(".vivi")).unwrap();
    let original = "# hand written\nname = \"legacy\"\n\n[[identities]]\nname = \"mind\"\n\n\
                    [sync]\nserver = \"https://s\"\nrecipient = \"mind\"\n";
    fs::write(config_path(&tmp), original).unwrap();

    let first = Mailspace::open_root(tmp.path().to_path_buf()).unwrap();
    let second = Mailspace::open_root(tmp.path().to_path_buf()).unwrap();

    assert_eq!(first.mailspace_id(), second.mailspace_id());
    let raw = fs::read_to_string(config_path(&tmp)).unwrap();
    assert_eq!(
        raw,
        format!("id = \"{}\"\n{original}", first.mailspace_id()),
        "only the id line was added"
    );
}

#[test]
fn an_existing_id_is_never_overwritten() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir_all(tmp.path().join(".vivi")).unwrap();
    let id = "00000000-0000-4000-8000-000000000001";
    fs::write(config_path(&tmp), format!("id = \"{id}\"\nname = \"x\"\n")).unwrap();

    let mailspace = Mailspace::open_root(tmp.path().to_path_buf()).unwrap();

    assert_eq!(mailspace.mailspace_id(), id);
    assert_eq!(mailspace.address_for("mind"), "mind@00000000.local");
}

#[test]
fn many_openers_of_a_fresh_config_agree_on_one_id() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir_all(tmp.path().join(".vivi")).unwrap();
    fs::write(
        config_path(&tmp),
        "name = \"shared\"\n\n[[identities]]\nname = \"mind\"\n",
    )
    .unwrap();
    let root = tmp.path().to_path_buf();

    let handles: Vec<_> = (0..12)
        .map(|_| {
            let root = root.clone();
            std::thread::spawn(move || {
                Mailspace::open_root(root)
                    .unwrap()
                    .mailspace_id()
                    .to_string()
            })
        })
        .collect();
    let ids: BTreeSet<String> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    assert_eq!(ids.len(), 1, "{ids:?}");
    let raw = fs::read_to_string(config_path(&tmp)).unwrap();
    assert_eq!(id_lines(&raw), 1, "{raw}");
    assert!(raw.contains("[[identities]]"));
    let sidecar = fs::read_to_string(tmp.path().join(".vivi/mailspace.id")).unwrap();
    assert_eq!(sidecar.trim(), ids.iter().next().unwrap().as_str());
}

#[test]
fn a_malformed_id_is_a_config_error_naming_the_file() {
    for bad in [
        "not-a-uuid",
        "3F9A1C7E-5B2D-4E8F-9A01-6C4D7E8F9A0B",
        "3f9a1c7e5b2d4e8f9a016c4d7e8f9a0b",
        "",
    ] {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join(".vivi")).unwrap();
        fs::write(config_path(&tmp), format!("id = \"{bad}\"\nname = \"x\"\n")).unwrap();

        let error = Mailspace::open_root(tmp.path().to_path_buf())
            .unwrap_err()
            .to_string();

        assert!(error.contains("mailspace.toml"), "{bad}: {error}");
        assert!(error.contains("UUID"), "{bad}: {error}");
        let raw = fs::read_to_string(config_path(&tmp)).unwrap();
        assert!(raw.starts_with("id = \""), "the file is not rewritten");
    }
}

#[test]
fn an_id_that_cannot_be_persisted_is_an_error_not_a_guess() {
    let tmp = tempfile::tempdir().unwrap();
    let vivi = tmp.path().join(".vivi");
    fs::create_dir_all(&vivi).unwrap();
    fs::write(config_path(&tmp), "name = \"x\"\n").unwrap();
    let mut permissions = fs::metadata(&vivi).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o500);
    fs::set_permissions(&vivi, permissions.clone()).unwrap();
    // A superuser ignores the mode; the property only holds otherwise.
    let writable = fs::write(vivi.join("probe"), "").is_ok();

    let opened = Mailspace::open_root(tmp.path().to_path_buf());

    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o700);
    fs::set_permissions(&vivi, permissions).unwrap();
    if !writable {
        let error = opened.unwrap_err().to_string();
        assert!(
            error.contains("mailspace id") || error.contains("lock"),
            "{error}"
        );
    }
}

#[test]
fn addresses_carry_the_prefix_and_old_forms_still_resolve() {
    let (_tmp, mut mailspace) = space(&["mind", "hand"]);
    mailspace.rename_identity("hand", "arm").unwrap();
    let prefix = mailspace.address_prefix().to_string();
    let legacy = mailspace.legacy_domain();

    assert_eq!(
        mailspace.address_for("mind"),
        format!("mind@{prefix}.local")
    );
    for value in [
        "mind".to_string(),
        "MIND".to_string(),
        "mind@local".to_string(),
        format!("mind@{prefix}.local"),
        format!("mind@{legacy}"),
        format!("mind@{}", legacy.to_ascii_uppercase()),
    ] {
        assert_eq!(
            mailspace.resolve_identity(&value).unwrap(),
            "mind",
            "{value}"
        );
    }
    assert_eq!(mailspace.resolve_identity("hand").unwrap(), "arm", "alias");
    assert_eq!(
        mailspace
            .resolve_identity(&format!("hand@{prefix}.local"))
            .unwrap(),
        "arm"
    );
    for value in [REMOTE, "mind@example.com"] {
        let error = mailspace.resolve_identity(value).unwrap_err().to_string();
        assert!(error.contains("external"), "{value}: {error}");
    }
}

#[test]
fn recipients_are_classified_local_or_remote() {
    let (_tmp, mailspace) = space(&["mind"]);
    let own = mailspace.address_for("mind");
    let local = |value: &str| mailspace.classify_recipient(value).unwrap();

    assert_eq!(local("mind"), Recipient::Local("mind".into()));
    assert_eq!(local(&own), Recipient::Local("mind".into()));
    assert_eq!(local(REMOTE), Recipient::Remote(REMOTE.into()));
    assert_eq!(
        local(" Hand.Two@3F9A1C7E.LOCAL "),
        Recipient::Remote("hand.two@3f9a1c7e.local".into())
    );
    for bad in [
        "nobody",
        "mind@example.com",
        "mind@3f9a1c7.local",
        "mind@3f9a1c7ez.local",
        "mind@zzzzzzzz.local",
        "mi nd@3f9a1c7e.local",
        &format!("{}@3f9a1c7e.local", "x".repeat(65)),
        &format!("ghost@{}", mailspace.address_domain()),
    ] {
        assert!(mailspace.classify_recipient(bad).is_err(), "{bad}");
    }
}

#[test]
fn a_remote_only_send_writes_one_sent_copy_and_no_delivery() {
    let (_tmp, mailspace) = space(&["mind", "hand"]);
    let request = send(&mailspace, &["MIND@3F9A1C7E.local"], "needs", Some("need"));

    let result = mailspace.send(request).unwrap();

    assert!(result.delivered.is_empty());
    assert_eq!(result.remote, [REMOTE]);
    let note = mailspace.remote_line(&result.remote).unwrap();
    assert!(!note.contains("queued for sync"), "{note}");
    assert!(note.contains("no [sync] is configured"), "{note}");
    assert!(note.contains(REMOTE), "{note}");
    let storage = mailspace.storage().unwrap();
    let rows = storage.list_messages().unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].local_role, "sent");
    assert_eq!(rows[0].account, "mind");
    assert_eq!(rows[0].handle, result.sent);
    let bytes = storage.read_message(&rows[0].message_id).unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains(&format!("To: <{REMOTE}>")), "{text}");
    assert!(
        text.contains(&format!("From: <{}>", mailspace.address_for("mind"))),
        "{text}"
    );
    assert!(
        mailspace.backlog_item_tracked(&result.sent).unwrap(),
        "the sent copy is the backlog item"
    );
    assert!(mailspace.list("mind", "inbox").unwrap().is_empty());
    assert!(mailspace.list("hand", "needs").unwrap().is_empty());
}

#[test]
fn a_local_only_send_is_unchanged_and_lists_no_remote() {
    let (_tmp, mailspace) = space(&["mind", "hand"]);

    let result = mailspace
        .send(send(&mailspace, &["hand"], "inbox", Some("mail")))
        .unwrap();

    assert_eq!(result.delivered.len(), 1);
    assert!(result.remote.is_empty());
    assert!(mailspace.remote_line(&result.remote).is_none());
    assert_eq!(
        mailspace.storage().unwrap().list_messages().unwrap().len(),
        2
    );
}

#[test]
fn a_send_to_nobody_and_to_an_unknown_address_are_errors() {
    let (_tmp, mailspace) = space(&["mind"]);

    let none = mailspace
        .send(send(&mailspace, &[], "inbox", None))
        .unwrap_err();
    let external = mailspace
        .send(send(&mailspace, &["x@example.com"], "inbox", None))
        .unwrap_err();
    let unknown = mailspace
        .send(send(&mailspace, &["ghost"], "inbox", None))
        .unwrap_err();

    assert!(none.to_string().contains("at least one"), "{none}");
    assert!(external.to_string().contains("external"), "{external}");
    assert!(
        unknown.to_string().contains("unknown local role"),
        "{unknown}"
    );
    assert!(
        mailspace
            .storage()
            .unwrap()
            .list_messages()
            .unwrap()
            .is_empty()
    );
}

fn pulled_need(mailspace: &Mailspace, to: &str) -> String {
    let letter = format!(
        "From: alice@a1b2c3d4.local\r\nTo: {to}\r\nSubject: please\r\n\
         Message-ID: <please@a1b2c3d4.local>\r\nX-Vivi-Kind: need\r\n\
         X-Vivi-Format: 1\r\n\
         Date: Sun, 11 Oct 2026 12:00:00 +0000\r\n\r\n{}",
        crate::letter::fixture::body_for("need", "body\n").replace('\n', "\r\n"),
    );
    mailspace
        .deliver_raw(letter.as_bytes(), "needs")
        .unwrap()
        .delivered
        .remove(0)
        .handle
}

#[test]
fn a_reply_to_a_remote_sender_needs_no_flags() {
    let (_tmp, mailspace) = space(&["mind"]);
    let handle = pulled_need(&mailspace, &mailspace.address_for("mind"));

    let result = mailspace
        .reply(
            &handle,
            "mind",
            Vec::new(),
            Vec::new(),
            None,
            "on it".into(),
        )
        .unwrap();

    assert!(result.delivered.is_empty());
    assert_eq!(result.remote, ["alice@a1b2c3d4.local"]);
    let sent = mailspace.list("mind", "sent").unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].subject, "Re: please");
    let text = mailspace
        .storage()
        .unwrap()
        .read_message(&sent[0].message_id)
        .unwrap();
    let text = String::from_utf8_lossy(&text);
    assert!(text.contains("To: <alice@a1b2c3d4.local>"), "{text}");
    assert!(
        text.contains("In-Reply-To: please@a1b2c3d4.local"),
        "{text}"
    );
}

#[test]
fn closing_a_work_item_with_a_note_answers_a_remote_sender_by_sync() {
    let (_tmp, mailspace) = space(&["mind"]);
    let handle = pulled_need(&mailspace, &mailspace.address_for("mind"));

    mailspace
        .move_item("mind", &handle, "done", Some("finished"), "need done", None)
        .unwrap();

    let sent = mailspace.list("mind", "sent").unwrap();
    assert_eq!(sent.len(), 1, "the note's sent copy is the record");
    let bytes = mailspace
        .storage()
        .unwrap()
        .read_message(&sent[0].message_id)
        .unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("To: <alice@a1b2c3d4.local>"), "{text}");
    assert!(mailspace.list("mind", "inbox").unwrap().is_empty());
}

#[test]
fn a_roles_last_signal_counts_mail_sent_under_the_legacy_domain() {
    let (_tmp, mut mailspace) = space(&["mind"]);
    let update = crate::mailspace::RoleUpdate {
        cadence: Some(Some("1h".into())),
        ..Default::default()
    };
    mailspace.set_role("mind", update).unwrap();
    let legacy = format!("mind@{}", mailspace.legacy_domain());
    let letter = format!(
        "From: {legacy}\r\nTo: {legacy}\r\nSubject: old signal\r\n\
         Message-ID: <old@x>\r\nDate: Sun, 11 Oct 2026 12:00:00 +0000\r\n\r\nbody\r\n"
    );
    mailspace
        .storage()
        .unwrap()
        .ingest_message(
            &crate::storage::MessageIngestRequest {
                account: "mind".into(),
                local_role: "sent".into(),
                read_state: true,
                starred: false,
                message_id_hint: None,
                presented_id: None,
                seed_hint: "legacy".into(),
                remote: None,
            },
            letter.as_bytes(),
        )
        .unwrap();

    let report = mailspace.schedule_report("mind").unwrap();

    assert!(report.last_signal_handle.is_some(), "{report:?}");
}

fn sidecar_path(tmp: &tempfile::TempDir) -> std::path::PathBuf {
    tmp.path().join(".vivi/mailspace.id")
}

const ID_A: &str = "00000000-0000-4000-8000-00000000000a";
const ID_B: &str = "00000000-0000-4000-8000-00000000000b";

fn bare(tmp: &tempfile::TempDir, config: &str) {
    fs::create_dir_all(tmp.path().join(".vivi")).unwrap();
    fs::write(config_path(tmp), config).unwrap();
}

#[test]
fn a_mint_writes_the_config_and_the_sidecar_with_one_id() {
    let tmp = tempfile::tempdir().unwrap();
    bare(&tmp, "name = \"x\"\n");

    let mailspace = Mailspace::open_root(tmp.path().to_path_buf()).unwrap();

    let sidecar = fs::read_to_string(sidecar_path(&tmp)).unwrap();
    assert_eq!(sidecar, format!("{}\n", mailspace.mailspace_id()));
    assert!(!tmp.path().join(".vivi/mailspace.id.tmp").exists());
}

#[test]
fn a_config_and_sidecar_that_agree_change_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let config = format!("id = \"{ID_A}\"\nname = \"x\"\n");
    bare(&tmp, &config);
    fs::write(sidecar_path(&tmp), format!("{ID_A}\n")).unwrap();

    let mailspace = Mailspace::open_root(tmp.path().to_path_buf()).unwrap();

    assert_eq!(mailspace.mailspace_id(), ID_A);
    assert_eq!(fs::read_to_string(config_path(&tmp)).unwrap(), config);
    assert_eq!(
        fs::read_to_string(sidecar_path(&tmp)).unwrap(),
        format!("{ID_A}\n")
    );
}

#[test]
fn a_config_id_with_no_sidecar_gets_the_sidecar_written() {
    let tmp = tempfile::tempdir().unwrap();
    bare(&tmp, &format!("id = \"{ID_A}\"\nname = \"x\"\n"));

    let mailspace = Mailspace::open_root(tmp.path().to_path_buf()).unwrap();

    assert_eq!(mailspace.mailspace_id(), ID_A);
    assert_eq!(
        fs::read_to_string(sidecar_path(&tmp)).unwrap(),
        format!("{ID_A}\n")
    );
}

#[test]
fn a_sidecar_that_cannot_be_written_next_to_a_config_with_the_id_is_not_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    bare(&tmp, &format!("id = \"{ID_A}\"\nname = \"x\"\n"));
    let vivi = tmp.path().join(".vivi");
    let mut permissions = fs::metadata(&vivi).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o500);
    fs::set_permissions(&vivi, permissions.clone()).unwrap();

    let opened = Mailspace::open_root(tmp.path().to_path_buf());

    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o700);
    fs::set_permissions(&vivi, permissions).unwrap();
    assert_eq!(opened.unwrap().mailspace_id(), ID_A);
}

#[test]
fn a_config_and_sidecar_that_differ_are_an_error_naming_both_and_nothing_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let config = format!("id = \"{ID_A}\"\nname = \"x\"\n");
    bare(&tmp, &config);
    fs::write(sidecar_path(&tmp), format!("{ID_B}\n")).unwrap();

    let error = Mailspace::open_root(tmp.path().to_path_buf())
        .unwrap_err()
        .to_string();

    for needle in [ID_A, ID_B, "mailspace.toml", "mailspace.id", "keep"] {
        assert!(error.contains(needle), "{needle}: {error}");
    }
    assert_eq!(fs::read_to_string(config_path(&tmp)).unwrap(), config);
    assert_eq!(
        fs::read_to_string(sidecar_path(&tmp)).unwrap(),
        format!("{ID_B}\n")
    );
}

#[test]
fn a_config_that_lost_its_id_gets_the_sidecars_back_and_never_a_new_one() {
    let tmp = tempfile::tempdir().unwrap();
    let rest = "# kept\nname = \"x\"\n\n[[identities]]\nname = \"mind\"\n";
    bare(&tmp, rest);
    fs::write(sidecar_path(&tmp), format!("{ID_B}\n")).unwrap();

    let mailspace = Mailspace::open_root(tmp.path().to_path_buf()).unwrap();

    assert_eq!(mailspace.mailspace_id(), ID_B);
    assert_eq!(mailspace.address_for("mind"), "mind@00000000.local");
    assert_eq!(
        fs::read_to_string(config_path(&tmp)).unwrap(),
        format!("id = \"{ID_B}\"\n{rest}"),
        "the same first-line insert as a mint"
    );
    let again = Mailspace::open_root(tmp.path().to_path_buf()).unwrap();
    assert_eq!(again.mailspace_id(), ID_B);
}

#[test]
fn a_malformed_sidecar_is_a_config_error_naming_the_file() {
    for (config, remedy) in [
        (format!("id = \"{ID_A}\"\nname = \"x\"\n"), "re-derive"),
        ("name = \"x\"\n".to_string(), "mint a new id"),
    ] {
        for bad in ["garbage", "", "00000000-0000-4000-8000-00000000000A"] {
            let tmp = tempfile::tempdir().unwrap();
            bare(&tmp, &config);
            fs::write(sidecar_path(&tmp), format!("{bad}\n")).unwrap();

            let error = Mailspace::open_root(tmp.path().to_path_buf())
                .unwrap_err()
                .to_string();

            assert!(error.contains("mailspace.id"), "{bad}: {error}");
            assert!(error.contains(remedy), "{bad}: {error}");
            assert_eq!(fs::read_to_string(config_path(&tmp)).unwrap(), config);
        }
    }
}
