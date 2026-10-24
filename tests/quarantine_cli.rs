//! `vivi sync quarantine` at the CLI: list, retry, and drop held letters;
//! an unknown or malformed id exits non-zero naming it.

use std::path::Path;
use std::process::{Command, Output};

use sha2::{Digest, Sha256};

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

/// A mailspace with `[sync]` configured, as the clearing verbs require; the
/// server is never contacted.
fn synced() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let vivi_dir = dir.path().join(".vivi");
    std::fs::create_dir_all(&vivi_dir).unwrap();
    std::fs::write(
        vivi_dir.join("mailspace.toml"),
        "name = \"alice\"\n\n[[identities]]\nname = \"mind\"\n\n\
         [sync]\nserver = \"http://127.0.0.1:9\"\n\
         project = \"abcde-fghjk-mnpqr-stvwz-23456\"\nname = \"alice\"\n",
    )
    .unwrap();
    dir
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// A receipt the way a pull writes it; `id` is the file name stem, not a
/// JSON field.
fn receipt(message_id: &str, reason: &str) -> String {
    format!(
        "{{\"message_id\": \"{message_id}\", \"from\": \"bob\", \"kind\": \"need\", \
         \"sequence\": 3, \"reason\": \"{reason}\", \
         \"at\": \"2026-10-24T12:00:00+00:00\"}}"
    )
}

/// Hold one letter the way a pull leaves it and return its id.
fn hold(project: &Path, bytes: &[u8], receipt: &str) -> String {
    let id = sha256(bytes);
    let root = project.join(".vivi").join("quarantine");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join(format!("{id}.eml")), bytes).unwrap();
    std::fs::write(root.join(format!("{id}.json")), receipt).unwrap();
    id
}

fn letter(from: &str, to: &str, subject: &str, extra_headers: &str, body: &str) -> Vec<u8> {
    format!(
        "From: {from}\r\nTo: {to}\r\nSubject: {subject}\r\nMessage-ID: <{subject}@x.test>\r\n\
         Date: Sun, 11 Oct 2026 12:00:00 +0000\r\n{extra_headers}\r\n{body}"
    )
    .into_bytes()
}

/// A format-1 need this vivi reads: what an upgrade heals.
fn good_need(from: &str, to: &str) -> Vec<u8> {
    letter(
        from,
        to,
        "healed need",
        "X-Vivi-Kind: need\r\nX-Vivi-Format: 1\r\n",
        "+++\ndone_when = [\"it works\"]\n+++\n\nThe work.\n",
    )
}

/// A format-1 need whose front matter carries a key this vivi does not know.
fn future_need(from: &str, to: &str) -> Vec<u8> {
    letter(
        from,
        to,
        "future need",
        "X-Vivi-Kind: need\r\nX-Vivi-Format: 1\r\n",
        "+++\ndone_when = [\"it works\"]\nflavor = \"future\"\n+++\n\nThe work.\n",
    )
}

/// This mailspace's own `mind` address; the id is minted on the first open.
fn mind_address(project: &Path) -> String {
    let config = std::fs::read_to_string(project.join(".vivi/mailspace.toml")).unwrap();
    let line = config
        .lines()
        .find(|line| line.starts_with("id = \""))
        .expect("discover mints the mailspace id");
    let id = line.trim_start_matches("id = \"").trim_end_matches('"');
    format!("mind@{}", &id[..8])
}

#[test]
fn list_on_a_clean_mailspace_is_quiet_and_green() {
    let project = synced();

    let output = vivi(project.path(), &["sync", "quarantine", "list"]);

    assert!(output.status.success(), "{}", text(&output));
    assert!(
        text(&output).contains("no quarantined letters"),
        "{}",
        text(&output)
    );
}

#[test]
fn a_held_letter_is_listed_with_its_id_reason_and_time() {
    let project = synced();
    let bytes = future_need("mind@00112233.local", "mind@alice.local");
    let id = hold(
        project.path(),
        &bytes,
        &receipt("msg_0000000000000000000000aa", "unknown field `flavor`"),
    );

    let output = vivi(project.path(), &["sync", "quarantine", "list"]);

    assert!(output.status.success(), "{}", text(&output));
    let printed = text(&output);
    assert!(printed.contains(&id), "{printed}");
    assert!(printed.contains("unknown field `flavor`"), "{printed}");
    assert!(printed.contains("2026-10-24T12:00:00+00:00"), "{printed}");
    assert!(printed.contains("bob"), "{printed}");
    assert!(
        printed.contains("msg_0000000000000000000000aa"),
        "{printed}"
    );
}

#[test]
fn retry_files_a_letter_the_parser_now_accepts_and_clears_the_pair() {
    let project = synced();
    // The operator flow: list first (this mints the mailspace id), then
    // hold what an older vivi refused, then retry under the new parser.
    let clean = vivi(project.path(), &["sync", "quarantine", "list"]);
    assert!(clean.status.success(), "{}", text(&clean));
    let bytes = good_need("mind@00112233.local", &mind_address(project.path()));
    let id = hold(
        project.path(),
        &bytes,
        &receipt("msg_0000000000000000000000cc", "unknown field `flavor`"),
    );

    let output = vivi(project.path(), &["sync", "quarantine", "retry", &id]);

    assert!(output.status.success(), "{}", text(&output));
    let printed = text(&output);
    assert!(printed.contains(&format!("filed {id}")), "{printed}");

    let after = vivi(project.path(), &["sync", "quarantine", "list"]);
    assert!(
        text(&after).contains("no quarantined letters"),
        "{}",
        text(&after)
    );
    let dropped = project
        .path()
        .join(".vivi")
        .join("quarantine")
        .join("dropped");
    assert!(dropped.join(format!("{id}.json")).exists());
    assert!(
        !project
            .path()
            .join(".vivi/quarantine")
            .join(format!("{id}.eml"))
            .exists()
    );
}

#[test]
fn retry_of_a_letter_the_parser_still_refuses_says_why() {
    let project = synced();
    let bytes = future_need("mind@00112233.local", "mind@alice.local");
    let id = hold(
        project.path(),
        &bytes,
        &receipt("msg_0000000000000000000000dd", "unknown field `flavor`"),
    );

    let output = vivi(project.path(), &["sync", "quarantine", "retry", &id]);

    assert!(output.status.success(), "{}", text(&output));
    let printed = text(&output);
    assert!(printed.contains(&format!("still held {id}")), "{printed}");
    assert!(printed.contains("flavor"), "{printed}");

    let after = vivi(project.path(), &["sync", "quarantine", "list"]);
    assert!(text(&after).contains(&id), "{}", text(&after));
}

#[test]
fn drop_moves_the_pair_under_dropped() {
    let project = synced();
    let bytes = future_need("mind@00112233.local", "mind@alice.local");
    let id = hold(
        project.path(),
        &bytes,
        &receipt("msg_0000000000000000000000bb", "unknown field `flavor`"),
    );

    let output = vivi(project.path(), &["sync", "quarantine", "drop", &id]);

    assert!(output.status.success(), "{}", text(&output));
    assert!(
        text(&output).contains(&format!("dropped {id}")),
        "{}",
        text(&output)
    );
    let dropped = project
        .path()
        .join(".vivi")
        .join("quarantine")
        .join("dropped");
    assert!(dropped.join(format!("{id}.json")).exists());
    assert_eq!(
        std::fs::read(dropped.join(format!("{id}.eml"))).unwrap(),
        bytes
    );

    let after = vivi(project.path(), &["sync", "quarantine", "list"]);
    assert!(
        text(&after).contains("no quarantined letters"),
        "{}",
        text(&after)
    );
}

#[test]
fn an_unknown_or_malformed_id_is_refused_and_exits_non_zero() {
    let project = synced();

    let malformed = vivi(project.path(), &["sync", "quarantine", "drop", "zz"]);
    assert!(!malformed.status.success(), "{}", text(&malformed));
    assert!(
        text(&malformed).contains("not a quarantined letter id"),
        "{}",
        text(&malformed)
    );

    let absent = vivi(
        project.path(),
        &["sync", "quarantine", "retry", &"a".repeat(64)],
    );
    assert!(!absent.status.success(), "{}", text(&absent));
    assert!(
        text(&absent).contains("no quarantined letter"),
        "{}",
        text(&absent)
    );
}
