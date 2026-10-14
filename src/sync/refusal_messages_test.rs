//! The refusal messages name the vivi verbs: the join refusal prints the key
//! and vouch steps with this mailspace's own name, a missing project names
//! `vivi sync create`, the 401s give the whole admission path, and none of
//! them sends the user off to write raw HTTP.

use super::*;
use crate::mailspace::Mailspace;
use crate::sync::SendFilter;

fn error_body(code: &str, message: &str) -> Value {
    json!({ "error": { "code": code, "message": message } })
}

/// Settings whose server carries no scheme, so the 404 message (which embeds
/// it) stays out of the raw-HTTP scan's way.
fn refusal_settings() -> SyncSettings {
    SyncSettings {
        server: "sync.example:8080".into(),
        project: "abcde-fghjk".into(),
        name: "alice".into(),
        send: SendFilter::All,
        mailspace_id: "3f9a1c7e-5b2d-4e8f-9a01-6c4d7e8f9a0b".into(),
    }
}

/// A mailspace whose `mailspace.toml` ends with `sync_table`.
fn mailspace_with(sync_table: &str) -> (tempfile::TempDir, Mailspace) {
    let tmp = tempfile::tempdir().unwrap();
    Mailspace::init(Some(tmp.path())).unwrap();
    let path = tmp.path().join(".vivi").join("mailspace.toml");
    let mut raw = std::fs::read_to_string(&path).unwrap();
    raw.push_str(sync_table);
    std::fs::write(&path, raw).unwrap();
    let mailspace = Mailspace::open_root(tmp.path().to_path_buf()).unwrap();
    (tmp, mailspace)
}

fn resolver_refusals() -> Vec<String> {
    let no_table = mailspace_with("");
    let no_project = mailspace_with("\n[sync]\nserver = \"sync.example:8080\"\nname = \"alice\"\n");
    vec![
        SyncSettings::resolve(&no_table.1).unwrap_err().to_string(),
        SyncSettings::resolve(&no_project.1)
            .unwrap_err()
            .to_string(),
    ]
}

/// Answer `replies` in order on a loopback port, one JSON document per
/// request, and return the port.
fn serve(replies: Vec<(u16, String)>) -> u16 {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for (status, reply) in replies {
            let (mut stream, _) = listener.accept().unwrap();
            let mut raw = Vec::new();
            let mut chunk = [0u8; 4096];
            let (head_end, length) = loop {
                let read = stream.read(&mut chunk).unwrap();
                raw.extend_from_slice(&chunk[..read]);
                if let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&raw[..end]).to_lowercase();
                    let length = head
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .map_or(0, |v| v.trim().parse::<usize>().unwrap());
                    break (end + 4, length);
                }
            };
            while raw.len() < head_end + length {
                let read = stream.read(&mut chunk).unwrap();
                raw.extend_from_slice(&chunk[..read]);
            }
            let response = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
        }
    });
    port
}

#[test]
fn the_join_admission_refusal_prints_the_key_and_vouch_steps() {
    let body = error_body("admission_required", "not admitted");
    let settings = refusal_settings();
    let dir = tempfile::tempdir().unwrap();
    let identity = SyncIdentity::ensure(dir.path()).unwrap();

    let error = join_error(409, &body, &settings, &identity).to_string();

    assert!(error.contains("vivi sync key"), "{error}");
    assert!(
        error.contains("vivi sync vouch alice --key-file"),
        "this mailspace's own name is filled in: {error}"
    );
    assert!(error.contains(&identity.wire_fingerprint()), "{error}");
    assert!(error.contains("then run vivi sync join again"), "{error}");
    assert!(error.contains("has no way to re-key"), "{error}");
    assert!(error.contains("project creator"), "{error}");
}

#[test]
fn a_missing_project_names_the_create_verb() {
    let (_tmp, mailspace) =
        mailspace_with("\n[sync]\nserver = \"sync.example:8080\"\nname = \"alice\"\n");

    let error = SyncSettings::resolve(&mailspace).unwrap_err().to_string();

    assert!(error.contains("[sync] project"), "{error}");
    assert!(error.contains("vivi sync create"), "{error}");
    assert!(error.contains("vivi sync join"), "{error}");
}

#[test]
fn a_missing_sync_table_names_the_create_verb_too() {
    let (_tmp, mailspace) = mailspace_with("");

    let error = SyncSettings::resolve(&mailspace).unwrap_err().to_string();

    assert!(error.contains("[sync]"), "{error}");
    assert!(error.contains("server"), "{error}");
    assert!(error.contains("vivi sync create"), "{error}");
}

#[test]
fn the_missing_server_and_name_texts_are_unchanged() {
    let (_tmp, mailspace) = mailspace_with("\n[sync]\nname = \"alice\"\n");
    let error = SyncSettings::resolve(&mailspace).unwrap_err().to_string();
    assert!(
        error.ends_with(
            "[sync] server is not set; set server = \"...\" in mailspace.toml \
         (the project server URL)"
        ),
        "{error}"
    );

    let (_tmp, mailspace) =
        mailspace_with("\n[sync]\nserver = \"sync.example:8080\"\nproject = \"abcde-fghjk\"\n");
    let error = SyncSettings::resolve(&mailspace).unwrap_err().to_string();
    assert!(
        error.ends_with(
            "[sync] name is not set; set name = \"...\" in mailspace.toml \
         (this checkout's identity name)"
        ),
        "{error}"
    );
}

#[test]
fn the_404_names_the_create_verb() {
    let body = error_body("not_found", "no such project");

    let error = api_error(404, &body, &refusal_settings()).to_string();

    assert!(error.contains("was not found"), "{error}");
    assert!(error.contains("vivi sync create"), "{error}");
    assert!(error.contains("vivi sync join"), "{error}");
}

#[test]
fn the_append_401_gives_the_whole_admission_path() {
    let stopped = classify_append(401, &error_body("unauthorized", "no")).unwrap_err();
    let AppendFailure::Stop(reason) = stopped else {
        panic!("{stopped:?}")
    };

    assert!(reason.contains("vivi sync key"), "{reason}");
    assert!(
        reason.contains("vivi sync vouch <name> --key-file"),
        "{reason}"
    );
    assert!(reason.contains("vivi sync join"), "{reason}");
}

#[test]
fn the_api_401_gives_the_whole_admission_path_with_the_name() {
    let body = error_body("unauthorized", "no");

    let error = api_error(401, &body, &refusal_settings()).to_string();

    assert!(error.contains("vivi sync key"), "{error}");
    assert!(
        error.contains("vivi sync vouch alice --key-file"),
        "{error}"
    );
    assert!(error.contains("vivi sync join"), "{error}");
}

#[test]
fn the_bearer_mint_401_gives_the_whole_admission_path_with_the_name() {
    let challenge = crate::b64::encode(&[7u8; 32]);
    let port = serve(vec![
        (
            200,
            format!(r#"{{"challenge_id":"c1","challenge":"{challenge}","expires_in":60}}"#),
        ),
        (401, error_body("unauthorized", "not admitted").to_string()),
    ]);
    let settings = SyncSettings {
        server: format!("http://127.0.0.1:{port}"),
        ..refusal_settings()
    };
    let dir = tempfile::tempdir().unwrap();
    let identity = SyncIdentity::ensure(dir.path()).unwrap();

    let error = mint_bearer(&settings, &identity).unwrap_err().to_string();

    assert!(error.contains("vivi sync key"), "{error}");
    assert!(
        error.contains("vivi sync vouch alice --key-file"),
        "{error}"
    );
    assert!(error.contains("vivi sync join"), "{error}");
    assert!(error.contains("project creator"), "{error}");
}

#[test]
fn no_refusal_message_sends_the_user_to_raw_http() {
    let body = error_body("admission_required", "not admitted");
    let settings = refusal_settings();
    let dir = tempfile::tempdir().unwrap();
    let identity = SyncIdentity::ensure(dir.path()).unwrap();
    let prefix = error_body("mailspace_prefix_taken", "taken");
    let generic = error_body("boom", "exploded");
    let stopped = |status| match classify_append(status, &error_body("x", "y")) {
        Err(AppendFailure::Stop(reason)) => reason,
        other => panic!("{status}: {other:?}"),
    };

    let messages = vec![
        join_error(409, &body, &settings, &identity).to_string(),
        join_error(409, &error_body("conflict", "taken"), &settings, &identity).to_string(),
        join_error(409, &prefix, &settings, &identity).to_string(),
        api_error(401, &error_body("unauthorized", "no"), &settings).to_string(),
        api_error(404, &error_body("not_found", "no"), &settings).to_string(),
        api_error(500, &generic, &settings).to_string(),
        stopped(401),
        stopped(500),
    ];
    let messages = [messages, resolver_refusals()].concat();

    assert!(!messages.is_empty());
    for message in &messages {
        for banned in ["curl", "POST ", "http://", "https://"] {
            assert!(
                !message.contains(banned),
                "'{banned}' in a refusal message: {message}"
            );
        }
    }
}
