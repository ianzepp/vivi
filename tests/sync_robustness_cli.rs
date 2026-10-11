//! Recovery and robustness at the CLI: an unreadable sync state, two syncs at
//! once, a rejected chat message, `memo delete` on work, and an import between
//! two checkouts of one project.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use vivi::storage::{RemoteBindingInput, Storage};

const PROJECT_ID: &str = "abcde-fghjk-mnpqr-stvwz-23456";
const ROOM: &str = "abcde-fghjk-mnpqr-stvwz-23456";

fn vivi(project: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args(args)
        .arg("--project")
        .arg(project)
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn ok(output: &Output) -> String {
    assert!(
        output.status.success(),
        "status: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        stdout(output),
        stderr(output)
    );
    stdout(output)
}

fn failed(output: &Output) -> String {
    assert!(
        !output.status.success(),
        "expected failure\nstdout:\n{}\nstderr:\n{}",
        stdout(output),
        stderr(output)
    );
    format!("{}{}", stdout(output), stderr(output))
}

/// A mailspace with a `[sync]` table pointing at a port nothing listens on.
fn synced() -> tempfile::TempDir {
    let project = tempfile::tempdir().unwrap();
    let dir = project.path().join(".vivi");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("mailspace.toml"),
        format!(
            "name = \"cli\"\n\n[[identities]]\nname = \"mind\"\n\n[sync]\n\
             server = \"http://127.0.0.1:9\"\nproject = \"{PROJECT_ID}\"\nname = \"alice\"\n"
        ),
    )
    .unwrap();
    project
}

#[test]
fn a_corrupt_state_file_fails_with_the_file_and_the_repair_never_a_parse_error() {
    let project = synced();
    let sync = project.path().join(".vivi/sync");
    std::fs::create_dir_all(&sync).unwrap();
    std::fs::write(
        sync.join("state.json"),
        format!("{{\"project\":\"{PROJECT_ID}\",\"sinc"),
    )
    .unwrap();

    for args in [
        &["sync", "status"][..],
        &["sync", "pull"],
        &["sync", "fetch", "000000a1"],
    ] {
        let output = failed(&vivi(project.path(), args));
        assert!(output.contains("state.json"), "{args:?}: {output}");
        assert!(output.contains("vivi sync resync"), "{args:?}: {output}");
    }

    // The repair itself gets past the file; it only stops at the dead server.
    let resync = failed(&vivi(project.path(), &["sync", "resync"]));
    assert!(!resync.contains("corrupt"), "{resync}");
}

#[test]
fn a_sync_command_waits_for_the_lock_and_runs_once_it_is_free() {
    let project = synced();
    let sync = project.path().join(".vivi/sync");
    std::fs::create_dir_all(&sync).unwrap();
    let lock = std::fs::File::create(sync.join("sync.lock")).unwrap();
    lock.lock().unwrap();
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1500));
        drop(lock);
    });

    let started = Instant::now();
    let output = vivi(project.path(), &["sync", "prune", "--older-than", "30d"]);

    releaser.join().unwrap();
    ok(&output);
    assert!(
        started.elapsed() >= Duration::from_millis(1400),
        "prune ran while another sync held the lock"
    );
}

/// A chat server that accepts the bearer handshake and refuses every message
/// with a 400, as for an unknown recipient.
fn refusing_chat_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                if stream.read(&mut byte).unwrap_or(0) == 0 {
                    break;
                }
                head.push(byte[0]);
            }
            let head = String::from_utf8_lossy(&head).into_owned();
            let length = head
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(key, _)| key.trim().eq_ignore_ascii_case("content-length"))
                .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            let mut body = vec![0u8; length];
            let _ = stream.read_exact(&mut body);
            let path = head.split_whitespace().nth(1).unwrap_or_default();
            let (status, payload) = if path.ends_with("/challenge") {
                (
                    200,
                    r#"{"challenge_id":"c1","challenge":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8="}"#,
                )
            } else if path.ends_with("/auth/token") {
                (200, r#"{"token":"tok"}"#)
            } else {
                (
                    400,
                    r#"{"error":{"code":"invalid_request","message":"unknown recipient label: nobody"}}"#,
                )
            };
            let response = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                payload.len()
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    format!("http://{addr}")
}

#[test]
fn a_rejected_chat_message_is_listed_fails_the_flush_and_blocks_nothing() {
    let project = tempfile::tempdir().unwrap();
    let dir = project.path().join(".vivi");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("mailspace.toml"),
        format!(
            "name = \"cli\"\n\n[[identities]]\nname = \"mind\"\n\n[chat]\nserver = \"{}\"\n",
            refusing_chat_server()
        ),
    )
    .unwrap();
    let chat = vivi::chat::chat_dir(&dir);
    vivi::chat::ensure_chat_dir(&chat).unwrap();
    let state = vivi::chat::state::ChatState::open(&chat).unwrap();
    state.join_room(ROOM, 2, "me.cli").unwrap();
    state
        .enqueue(
            ROOM,
            "msg_000000000000000000000001",
            r#"{"to":"nobody","subject":"s","body":"b"}"#,
        )
        .unwrap();
    drop(state);

    let flush = failed(&vivi(project.path(), &["chat", "flush"]));
    assert!(flush.contains("1 rejected"), "{flush}");
    assert!(flush.contains("msg_000000000000000000000001"), "{flush}");
    assert!(
        flush.contains("unknown recipient label: nobody"),
        "the server's reason is shown: {flush}"
    );

    let status = ok(&vivi(project.path(), &["chat", "status"]));
    assert!(status.contains("chat outbox depth: 0"), "{status}");
    assert!(status.contains("chat outbox rejected: 1"), "{status}");
    assert!(status.contains("unknown recipient label"), "{status}");

    // Still failing, still kept, and not retried into a different state.
    let again = failed(&vivi(project.path(), &["chat", "flush"]));
    assert!(again.contains("1 rejected"), "{again}");
}

fn init_roster(project: &Path) {
    ok(&vivi(project, &["mailspace", "init"]));
    for identity in ["ceo", "cto"] {
        ok(&vivi(project, &["mailspace", "identity", "add", identity]));
    }
}

fn handle_after(output: &str, prefix: &str) -> String {
    output
        .lines()
        .find_map(|line| line.strip_prefix(prefix))
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_else(|| panic!("missing '{prefix}' in output:\n{output}"))
        .to_string()
}

#[test]
fn memo_delete_refuses_work_and_mail_but_deletes_memos() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let task = handle_after(
        &ok(&vivi(
            project.path(),
            &[
                "task",
                "send",
                "--from",
                "ceo",
                "--to",
                "cto",
                "--subject",
                "t",
                "--body",
                "b",
            ],
        )),
        "created cto ",
    );
    let mail = handle_after(
        &ok(&vivi(
            project.path(),
            &[
                "mail",
                "send",
                "--from",
                "ceo",
                "--to",
                "cto",
                "--subject",
                "m",
                "--body",
                "b",
            ],
        )),
        "delivered cto ",
    );
    let memo = handle_after(
        &ok(&vivi(
            project.path(),
            &[
                "memo",
                "save",
                "--for",
                "cto",
                "--subject",
                "note",
                "--body",
                "remember",
            ],
        )),
        "saved ",
    );

    let refused = failed(&vivi(
        project.path(),
        &["memo", "delete", &task, "--for", "cto"],
    ));
    assert!(refused.contains("is task, not memo"), "{refused}");
    let refused = failed(&vivi(
        project.path(),
        &["memo", "delete", &mail, "--for", "cto"],
    ));
    assert!(refused.contains("is mail, not memo"), "{refused}");
    let tasks = ok(&vivi(project.path(), &["task", "list", "--for", "cto"]));
    assert!(tasks.contains(&task), "the task was not trashed: {tasks}");

    let deleted = ok(&vivi(
        project.path(),
        &["memo", "delete", &memo, "--for", "cto"],
    ));
    assert!(deleted.contains(&format!("deleted {memo}")), "{deleted}");
    let memos = ok(&vivi(project.path(), &["memo", "list", "--for", "cto"]));
    assert!(!memos.contains(&memo), "{memos}");
}

/// Send a letter in `project` and bind its delivered copy to the project
/// item `uid`, as a sync would. Returns the delivered copy's handle.
fn send_and_bind(project: &Path, subject: &str, uid: u32) -> String {
    let sent = ok(&vivi(
        project,
        &[
            "mail",
            "send",
            "--from",
            "ceo",
            "--to",
            "cto",
            "--subject",
            subject,
            "--body",
            "b",
        ],
    ));
    let handle = handle_after(&sent, "delivered cto ");
    let mut storage = Storage::open_mailspace(&project.join(".vivi")).unwrap();
    let id = storage.resolve_message_token(&handle).unwrap();
    let view = storage.message_by_id(&id).unwrap().unwrap();
    let bound = storage
        .bind_remote(
            &id,
            &RemoteBindingInput {
                account: view.account,
                provider: "vivi-project".into(),
                remote_mailbox: PROJECT_ID.into(),
                remote_uid: uid,
                remote_uidvalidity: 1,
            },
        )
        .unwrap();
    assert!(bound);
    handle
}

fn message_count(project: &Path) -> usize {
    Storage::open_mailspace(&project.join(".vivi"))
        .unwrap()
        .list_messages()
        .unwrap()
        .len()
}

#[test]
fn an_import_that_collides_on_a_project_position_changes_nothing_and_says_why() {
    let target = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    init_roster(target.path());
    init_roster(source.path());
    send_and_bind(target.path(), "from the target", 18);
    send_and_bind(source.path(), "from the source", 18);
    // A letter that would import cleanly, so a partial import would show.
    ok(&vivi(
        source.path(),
        &[
            "mail",
            "send",
            "--from",
            "ceo",
            "--to",
            "cto",
            "--subject",
            "innocent",
            "--body",
            "b",
        ],
    ));
    let before = message_count(target.path());
    let from = source.path().to_str().unwrap();

    let dry = failed(&vivi(
        target.path(),
        &["mailspace", "import", "--from", from, "--dry-run"],
    ));
    assert!(dry.contains("import conflict"), "{dry}");
    assert!(dry.contains("already holds"), "{dry}");
    assert!(!dry.contains("UNIQUE"), "{dry}");
    assert_eq!(message_count(target.path()), before);

    let real = failed(&vivi(
        target.path(),
        &["mailspace", "import", "--from", from],
    ));
    assert!(real.contains("import conflict"), "{real}");
    assert!(real.contains("nothing was imported"), "{real}");
    assert!(!real.contains("UNIQUE"), "{real}");
    assert_eq!(
        message_count(target.path()),
        before,
        "no orphan row, and the innocent letter did not land"
    );
}

#[test]
fn an_import_without_a_collision_still_lands_whole() {
    let target = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    init_roster(target.path());
    init_roster(source.path());
    send_and_bind(target.path(), "from the target", 18);
    send_and_bind(source.path(), "from the source", 19);
    let before = message_count(target.path());

    ok(&vivi(
        target.path(),
        &[
            "mailspace",
            "import",
            "--from",
            source.path().to_str().unwrap(),
        ],
    ));

    assert!(message_count(target.path()) > before);
}
