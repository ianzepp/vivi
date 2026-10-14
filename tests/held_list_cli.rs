//! Held work is listable: a need, task or want this mailspace holds under
//! another mailspace's address shows under `--held` and `--for <address>`,
//! never under `--for <own role>` (the ruling: a role's own folder lists
//! what is addressed to that role of this mailspace).

use std::path::Path;
use std::process::{Command, Output};

use vivi::mailspace::Mailspace;
use vivi::storage::MessageIngestRequest;

/// Another member's `mind`: work addressed to it is held under this address.
const THEIRS: &str = "mind@bd371d96.local";
/// Another member's `hand`: a second holder, so `--held` shows more than one.
const OTHERS: &str = "hand@bd371d96.local";
const AUTHOR: &str = "hand@3f9a1c7e.local";
const BODY: &str = "done_when: it works";

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

fn ok(project: &Path, args: &[&str]) -> String {
    let mut full: Vec<&str> = args.to_vec();
    full.extend(["--project", project.to_str().unwrap()]);
    let output = vivi(full);
    assert!(
        output.status.success(),
        "{args:?}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn fails(project: &Path, args: &[&str]) -> String {
    let mut full: Vec<&str> = args.to_vec();
    full.extend(["--project", project.to_str().unwrap()]);
    let output = vivi(full);
    assert!(!output.status.success(), "{args:?} should fail");
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn project() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    ok(tmp.path(), &["mailspace", "init"]);
    ok(tmp.path(), &["role", "add", "mind"]);
    tmp
}

/// File one letter under `account`, as `vivi sync` files work pulled for a
/// role of another mailspace: the row is held under the raw address.
fn ingest_held(project: &Path, account: &str, role: &str, subject: &str, kind: &str) -> String {
    let letter = format!(
        "From: {AUTHOR}\r\nTo: {account}\r\nSubject: {subject}\r\n\
         Message-ID: <{subject}@3f9a1c7e.local>\r\nX-Vivi-Kind: {kind}\r\n\
         Date: Sun, 11 Oct 2026 12:00:00 +0000\r\n\r\n{BODY}\r\n"
    );
    let mailspace = Mailspace::discover(Some(project)).unwrap();
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

/// The first handle a `created`/`sent` receipt names.
fn delivered_handle(stdout: &str) -> String {
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("created "))
        .unwrap_or_else(|| panic!("no created line in:\n{stdout}"))
        .split_whitespace()
        .last()
        .unwrap()
        .to_string()
}

#[test]
fn a_need_held_for_another_member_lists_under_held_and_for_address() {
    let tmp = project();
    let p = tmp.path();
    let held = ingest_held(p, THEIRS, "needs", "pulled need", "need");
    let own = delivered_handle(&ok(
        p,
        &[
            "need",
            "send",
            "--from",
            "mind",
            "--to",
            "mind",
            "--subject",
            "home need",
            "--body",
            BODY,
        ],
    ));

    // Held: shown, with the address it is held under.
    let listed = ok(p, &["need", "list", "--held"]);
    assert!(listed.contains(&held), "{listed}");
    assert!(listed.contains("held_for"), "{listed}");
    assert!(listed.contains(THEIRS), "{listed}");
    assert!(!listed.contains(&own), "{listed}");

    // The exact address, case-insensitively.
    let upper = ok(p, &["need", "list", "--for", &THEIRS.to_uppercase()]);
    assert!(upper.contains(&held), "{upper}");
    assert!(!upper.contains(&own), "{upper}");

    // The JSON row names the holder.
    let json = ok(p, &["need", "list", "--held", "--json"]);
    assert!(
        json.contains(&format!("\"held_for\": \"{THEIRS}\"")),
        "{json}"
    );

    // A second holder's need shows under --held but not under the first.
    let other = ingest_held(p, OTHERS, "needs", "other need", "need");
    let all = ok(p, &["need", "list", "--held"]);
    assert!(all.contains(&held) && all.contains(&other), "{all}");
    let one = ok(p, &["need", "list", "--for", THEIRS]);
    assert!(one.contains(&held) && !one.contains(&other), "{one}");
}

#[test]
fn a_held_need_is_not_listed_under_this_mailspaces_own_role() {
    let tmp = project();
    let p = tmp.path();
    let held = ingest_held(p, THEIRS, "needs", "pulled need", "need");
    let own = delivered_handle(&ok(
        p,
        &[
            "need",
            "send",
            "--from",
            "mind",
            "--to",
            "mind",
            "--subject",
            "home need",
            "--body",
            BODY,
        ],
    ));
    let mine = ok(p, &["need", "list", "--for", "mind"]);
    assert!(mine.contains(&own), "{mine}");
    assert!(!mine.contains(&held), "{mine}");
    assert!(!mine.contains("held_for"), "{mine}");
}

#[test]
fn held_status_and_marks_filters_apply() {
    let tmp = project();
    let p = tmp.path();
    let open = ingest_held(p, THEIRS, "needs", "pulled open", "need");
    let done = ingest_held(p, THEIRS, "done", "pulled done", "need");

    let open_list = ok(p, &["need", "list", "--held", "--status", "open"]);
    assert!(open_list.contains(&open), "{open_list}");
    assert!(!open_list.contains(&done), "{open_list}");
    let done_list = ok(p, &["need", "list", "--held", "--status", "done"]);
    assert!(done_list.contains(&done), "{done_list}");
    assert!(!done_list.contains(&open), "{done_list}");
    let all = ok(p, &["need", "list", "--held", "--status", "all"]);
    assert!(all.contains(&open) && all.contains(&done), "{all}");

    let from_author = ok(p, &["need", "list", "--held", "--from", AUTHOR]);
    assert!(from_author.contains(&open), "{from_author}");
    let from_elsewhere = ok(
        p,
        &["need", "list", "--held", "--from", "ghost@00000000.local"],
    );
    assert!(!from_elsewhere.contains(&open), "{from_elsewhere}");
}

#[test]
fn tasks_and_wants_held_for_another_member_list_the_same_way() {
    let tmp = project();
    let p = tmp.path();
    let task = ingest_held(p, THEIRS, "tasks", "pulled task", "task");
    let want = ingest_held(p, THEIRS, "wants", "pulled want", "want");

    let tasks = ok(p, &["task", "list", "--held"]);
    assert!(tasks.contains(&task) && tasks.contains(THEIRS), "{tasks}");
    assert!(!ok(p, &["task", "list", "--for", "mind"]).contains(&task));
    let wants = ok(p, &["want", "list", "--held"]);
    assert!(wants.contains(&want) && wants.contains(THEIRS), "{wants}");
    assert!(!ok(p, &["want", "list", "--for", "mind"]).contains(&want));
}

#[test]
fn held_with_for_is_refused() {
    let tmp = project();
    let p = tmp.path();
    for kind in ["need", "task", "want"] {
        let refused = fails(p, &[kind, "list", "--held", "--for", "mind"]);
        assert!(
            refused.contains("--held") && refused.contains("--for"),
            "{kind}: {refused}"
        );
    }
}
