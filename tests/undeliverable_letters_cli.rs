//! A letter that can never be delivered is refused or reported where the
//! user is, never accepted as "queued for sync" and then skipped forever.

use std::path::Path;
use std::process::{Command, Output};

const REMOTE: &str = "hand@3f9a1c7e.local";

fn vivi(project: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args(args)
        .arg("--project")
        .arg(project)
        .output()
        .unwrap()
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn project(synced: bool) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let vivi_dir = dir.path().join(".vivi");
    std::fs::create_dir_all(&vivi_dir).unwrap();
    let mut config = String::from(
        "name = \"alice\"\n\n[[identities]]\nname = \"mind\"\n\n[[identities]]\nname = \"hand\"\n",
    );
    if synced {
        config.push_str(
            "\n[sync]\nserver = \"http://127.0.0.1:9\"\n\
             project = \"abcde-fghjk-mnpqr-stvwz-23456\"\nname = \"alice\"\n",
        );
    }
    std::fs::write(vivi_dir.join("mailspace.toml"), config).unwrap();
    dir
}

fn send(project: &Path, to: &str, body: &str) -> Output {
    vivi(
        project,
        &[
            "mail",
            "send",
            "--from",
            "mind",
            "--to",
            to,
            "--subject",
            "s",
            "--body",
            body,
        ],
    )
}

fn deliver(project: &Path, eml: &str) -> Output {
    let file = project.join("letter.eml");
    std::fs::write(&file, eml).unwrap();
    vivi(project, &["mail", "deliver", file.to_str().unwrap()])
}

#[test]
fn an_oversize_letter_is_refused_at_compose_and_nothing_is_stored() {
    let dir = project(true);
    let big = "x".repeat(70_000);

    let out = send(dir.path(), REMOTE, &big);

    assert!(!out.status.success(), "{}", text(&out));
    let said = text(&out);
    assert!(said.contains("over the 65536-byte"), "{said}");
    let status = stdout(&vivi(dir.path(), &["sync", "status"]));
    assert!(status.contains("sync unsent: 0 letter(s)"), "{status}");
    assert!(!status.contains("not uploaded"), "{status}");
}

#[test]
fn without_sync_an_oversize_letter_is_stored_and_the_note_says_it_stays() {
    let dir = project(false);

    let out = send(dir.path(), REMOTE, &"x".repeat(70_000));

    assert!(out.status.success(), "{}", text(&out));
    let said = text(&out);
    assert!(!said.contains("queued for sync"), "{said}");
    assert!(said.contains("no [sync] is configured"), "{said}");
    assert!(said.contains("will not leave this checkout"), "{said}");
}

#[test]
fn with_sync_a_remote_recipient_is_queued() {
    let dir = project(true);

    let out = send(dir.path(), REMOTE, "hello");

    assert!(out.status.success(), "{}", text(&out));
    assert!(
        stdout(&out).contains(&format!("queued for sync: {REMOTE}")),
        "{}",
        text(&out)
    );
}

#[test]
fn deliver_accepts_a_remote_recipient_and_queues_it() {
    let dir = project(true);
    let eml = format!(
        "From: mind@local\r\nTo: hand@local, {REMOTE}\r\nSubject: r\r\n\
         Message-ID: <r@audit.local>\r\n\r\nbody\r\n"
    );

    let out = deliver(dir.path(), &eml);

    assert!(out.status.success(), "{}", text(&out));
    let said = stdout(&out);
    assert!(said.contains("delivered hand "), "{said}");
    assert!(
        said.contains(&format!("queued for sync: {REMOTE}")),
        "{said}"
    );
    let status = stdout(&vivi(dir.path(), &["sync", "status"]));
    assert!(status.contains("sync unsent: 1 letter(s)"), "{status}");
}

#[test]
fn a_forged_from_is_listed_at_push_and_counted_in_status() {
    let dir = project(true);
    let eml = "From: mind@3f9a1c7e.local\r\nTo: hand@local\r\nSubject: forged\r\n\
               Message-ID: <f@audit.local>\r\n\r\nx\r\n";
    let delivered = stdout(&deliver(dir.path(), eml));
    let handle = delivered.split_whitespace().last().unwrap().to_string();

    let push = vivi(dir.path(), &["sync", "push"]);

    assert!(!push.status.success(), "{}", text(&push));
    let said = text(&push);
    assert!(said.contains(&format!("skipped {handle}")), "{said}");
    assert!(said.contains("claims mailspace 3f9a1c7e"), "{said}");
    let status = stdout(&vivi(dir.path(), &["sync", "status"]));
    assert!(status.contains("sync unsent: 1 letter(s)"), "{status}");
    assert!(
        status.contains("sync not uploaded: 1 with a From that claims another mailspace"),
        "{status}"
    );
}
