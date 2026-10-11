//! Mailspace ids and addresses at the CLI: one id however many processes
//! open a fresh mailspace at once, and rows held under accounts that are not
//! local roles (letters pulled for another mailspace, or for a role this one
//! does not have) leave every read path working.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use vivi::mailspace::Mailspace;
use vivi::storage::MessageIngestRequest;

fn vivi<I, S>(args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args(args)
        .output()
        .unwrap()
}

fn run(project: &Path, args: &[&str]) -> Output {
    let mut full: Vec<&str> = args.to_vec();
    let project = project.to_str().unwrap();
    full.extend(["--project", project]);
    vivi(full)
}

fn ok(output: &Output) -> String {
    assert!(
        output.status.success(),
        "status: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn processes_opening_a_fresh_mailspace_together_agree_on_one_id() {
    let project = tempfile::tempdir().unwrap();
    let vivi_dir = project.path().join(".vivi");
    std::fs::create_dir_all(&vivi_dir).unwrap();
    let config = vivi_dir.join("mailspace.toml");
    std::fs::write(
        &config,
        "name = \"shared\"\n\n[[identities]]\nname = \"mind\"\n",
    )
    .unwrap();

    let children: Vec<_> = (0..8)
        .map(|_| {
            Command::new(env!("CARGO_BIN_EXE_vivi"))
                .args(["mailspace", "identity", "list", "--project"])
                .arg(project.path())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let outputs: BTreeSet<String> = children
        .into_iter()
        .map(|child| ok(&child.wait_with_output().unwrap()))
        .collect();

    assert_eq!(
        outputs.len(),
        1,
        "every process saw one address: {outputs:?}"
    );
    let raw = std::fs::read_to_string(&config).unwrap();
    assert_eq!(
        raw.lines().filter(|line| line.starts_with("id = ")).count(),
        1,
        "{raw}"
    );
    let line = outputs.into_iter().next().unwrap();
    assert!(line.starts_with("mind mind@"), "{line}");
    assert!(line.trim_end().ends_with(".local"), "{line}");
}

fn ingest(mailspace: &Mailspace, account: &str, role: &str, subject: &str, kind: &str) -> String {
    let letter = format!(
        "From: alice@a1b2c3d4.local\r\nTo: {account}\r\nSubject: {subject}\r\n\
         Message-ID: <{subject}@a1b2c3d4.local>\r\nX-Vivi-Kind: {kind}\r\n\
         Date: Sun, 11 Oct 2026 12:00:00 +0000\r\n\r\nbody\r\n\
         done_when: it is done\r\n"
    );
    let mut storage = mailspace.storage().unwrap();
    let stored = storage
        .ingest_message(
            &MessageIngestRequest {
                account: account.into(),
                local_role: role.into(),
                read_state: false,
                starred: false,
                message_id_hint: None,
                presented_id: None,
                seed_hint: format!("{account}\0{role}\0{subject}"),
                remote: None,
            },
            letter.as_bytes(),
        )
        .unwrap();
    storage.display_handle(&stored.message_id).unwrap()
}

#[test]
fn rows_under_accounts_that_are_not_local_roles_do_not_break_the_read_paths() {
    let project = tempfile::tempdir().unwrap();
    let path = project.path();
    ok(&run(path, &["mailspace", "init"]));
    ok(&run(path, &["mailspace", "identity", "add", "mind"]));
    let mailspace = Mailspace::discover(Some(path)).unwrap();
    let foreign = format!("hand@{}", "a1b2c3d4.local");
    let mut tracked = Vec::new();
    for account in ["ghost", foreign.as_str(), "mind"] {
        for (role, kind) in [("needs", "need"), ("tasks", "task"), ("wants", "want")] {
            let subject = format!("{account}-{kind}").replace(['@', '.'], "-");
            let handle = ingest(&mailspace, account, role, &subject, kind);
            mailspace
                .backlog_attach(kind, &handle, &subject, &[])
                .unwrap();
            tracked.push(handle);
        }
        ingest(
            &mailspace,
            account,
            "inbox",
            &format!("mail-{account}").replace(['@', '.'], "-"),
            "mail",
        );
        ingest(
            &mailspace,
            account,
            "sent",
            &format!("sent-{account}").replace(['@', '.'], "-"),
            "mail",
        );
        ingest(
            &mailspace,
            account,
            "done",
            &format!("done-{account}").replace(['@', '.'], "-"),
            "need",
        );
    }

    let board = ok(&run(path, &["board"]));
    assert!(board.contains("mind-need"), "{board}");
    for args in [
        vec!["board", "--json"],
        vec!["board", "--graph"],
        vec!["board", "--wants", "50"],
        vec!["boot"],
        vec!["mailspace", "status"],
        vec!["mailspace", "identity", "list"],
        vec!["mail", "list", "--for", "mind"],
        vec!["need", "list", "--for", "mind"],
        vec!["task", "list", "--for", "mind"],
        vec!["want", "list", "--for", "mind"],
        vec!["task", "dump"],
        vec!["mail", "dump"],
        vec!["graph", "audit"],
        vec!["graph", "ready"],
        vec!["step"],
        vec!["trace", tracked[0].as_str()],
    ] {
        let output = run(path, &args);
        assert!(
            output.status.success(),
            "{args:?}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let audit = ok(&run(path, &["graph", "audit", "--repair"]));
    assert!(audit.contains("backlog audit"), "{audit}");
}

#[test]
fn list_filters_match_mail_written_under_the_legacy_domain() {
    let project = tempfile::tempdir().unwrap();
    let path = project.path();
    ok(&run(path, &["mailspace", "init"]));
    ok(&run(path, &["mailspace", "identity", "add", "mind"]));
    ok(&run(path, &["mailspace", "identity", "add", "hand"]));
    let mailspace = Mailspace::discover(Some(path)).unwrap();
    let legacy = mailspace.legacy_domain();
    let letter = format!(
        "From: mind@{legacy}\r\nTo: hand@{legacy}\r\nSubject: written before ids\r\n\
         Message-ID: <before-ids@{legacy}>\r\nDate: Sun, 11 Oct 2026 12:00:00 +0000\r\n\
         \r\nold body\r\n"
    );
    mailspace
        .storage()
        .unwrap()
        .ingest_message(
            &MessageIngestRequest {
                account: "hand".into(),
                local_role: "inbox".into(),
                read_state: false,
                starred: false,
                message_id_hint: None,
                presented_id: None,
                seed_hint: "legacy-letter".into(),
                remote: None,
            },
            letter.as_bytes(),
        )
        .unwrap();
    ok(&run(
        path,
        &[
            "mail",
            "send",
            "--from",
            "mind",
            "--to",
            "hand",
            "--subject",
            "written after ids",
            "--body",
            "new body",
        ],
    ));

    let listed = ok(&run(
        path,
        &["mail", "list", "--for", "hand", "--from", "mind"],
    ));
    assert!(listed.contains("written before ids"), "{listed}");
    assert!(listed.contains("written after ids"), "{listed}");
    let listed = ok(&run(
        path,
        &["mail", "list", "--for", "hand", "--to", "hand"],
    ));
    assert!(listed.contains("written before ids"), "{listed}");
    assert!(listed.contains("written after ids"), "{listed}");
}

#[test]
fn a_config_an_older_binary_rewrote_without_the_id_gets_the_same_id_back() {
    let project = tempfile::tempdir().unwrap();
    let path = project.path();
    ok(&run(path, &["mailspace", "init"]));
    ok(&run(path, &["mailspace", "identity", "add", "mind"]));
    let before = ok(&run(path, &["mailspace", "identity", "list"]));
    let config = path.join(".vivi/mailspace.toml");
    let sidecar = path.join(".vivi/mailspace.id");
    let original = std::fs::read_to_string(&config).unwrap();
    let id = original
        .lines()
        .find_map(|line| line.strip_prefix("id = "))
        .unwrap()
        .trim_matches('"')
        .to_string();
    assert_eq!(
        std::fs::read_to_string(&sidecar).unwrap(),
        format!("{id}\n")
    );

    // An older vivi rewrites the config from a struct with no `id` field.
    let dropped: String = original
        .lines()
        .filter(|line| !line.starts_with("id = "))
        .map(|line| format!("{line}\n"))
        .collect();
    std::fs::write(&config, &dropped).unwrap();

    let output = run(path, &["mailspace", "identity", "list"]);
    let after = ok(&output);

    assert_eq!(after, before, "addresses are unchanged");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("restored"), "{stderr}");
    assert!(stderr.contains(&id), "{stderr}");
    let restored = std::fs::read_to_string(&config).unwrap();
    assert_eq!(restored, format!("id = \"{id}\"\n{dropped}"));
    // Quiet once restored, and a rewriting command keeps it.
    let quiet = run(path, &["role", "add", "hand"]);
    ok(&quiet);
    assert!(
        quiet.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&quiet.stderr)
    );
    let kept = std::fs::read_to_string(&config).unwrap();
    assert!(kept.contains(&format!("id = \"{id}\"")), "{kept}");
    assert_eq!(
        std::fs::read_to_string(&sidecar).unwrap(),
        format!("{id}\n")
    );
}
