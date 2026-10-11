use serde_json::json;

use super::*;

#[test]
fn upsert_file_adds_then_replaces_by_path() {
    let mut config = json!({"files": [{"guest_path": "/work/boot.sh", "raw_value": "Ym9vdA=="}]});
    upsert_file(&mut config, "/work/job.tgz", b"one");
    upsert_file(&mut config, "/work/job.tgz", b"two");
    let files = config["files"].as_array().unwrap();
    assert_eq!(files.len(), 2);
    assert_eq!(files[0]["guest_path"], "/work/boot.sh");
    assert_eq!(files[1]["guest_path"], "/work/job.tgz");
    assert_eq!(files[1]["raw_value"], b64::encode(b"two"));
}

#[test]
fn upsert_file_creates_the_list() {
    let mut config = json!({"env": {}});
    upsert_file(&mut config, "/work/job.tgz", b"x");
    assert_eq!(config["files"].as_array().unwrap().len(), 1);
}

#[test]
fn drop_env_removes_only_the_named_keys() {
    let mut config = json!({"env": {"GH_TOKEN": "t", "REPO": "o/r"}});
    drop_env_names(&mut config, &["GH_TOKEN", "ABSENT"]);
    assert_eq!(config["env"], json!({"REPO": "o/r"}));
    let mut none = json!({});
    drop_env_names(&mut none, &["GH_TOKEN"]);
    assert_eq!(none, json!({}));
}

#[test]
fn reply_messages_come_from_either_field() {
    assert_eq!(reply_message(&json!({"message": "m"})), "m");
    assert_eq!(reply_message(&json!({"error": "e"})), "e");
    assert_eq!(reply_message(&Value::Null), "no message");
}

#[test]
fn set_config_metadata_adds_to_existing_or_new_maps() {
    let mut config = json!({"metadata": {"a": "1"}});
    set_config_metadata(&mut config, "state", "ready");
    assert_eq!(config["metadata"], json!({"a": "1", "state": "ready"}));
    let mut bare = json!({"env": {}});
    set_config_metadata(&mut bare, "state", "ready");
    assert_eq!(bare["metadata"], json!({"state": "ready"}));
}

mod refresh {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use super::*;

    /// A one-thread HTTP stub: answers each request with the next status in
    /// `statuses` and records the Authorization header it saw.
    fn stub(statuses: Vec<u16>) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        std::thread::spawn(move || {
            for status in statuses {
                let Ok((mut conn, _)) = listener.accept() else {
                    return;
                };
                let mut buf = [0u8; 4096];
                let n = conn.read(&mut buf).unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                let auth = head
                    .lines()
                    .find_map(|l| l.strip_prefix("authorization: "))
                    .unwrap_or_default()
                    .to_string();
                record.lock().unwrap().push(auth);
                let body = r#"{"message":"stub"}"#;
                let _ = write!(
                    conn,
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        (base, seen)
    }

    static REFUSED: AtomicUsize = AtomicUsize::new(0);
    #[allow(clippy::unnecessary_wraps)]
    fn numbered_refused() -> Result<Secret, VivariumError> {
        Ok(Secret::new(format!(
            "tok{}",
            REFUSED.fetch_add(1, Ordering::SeqCst)
        )))
    }

    #[test]
    fn a_refused_token_is_replaced_and_the_call_repeated() {
        let (base, seen) = stub(vec![401, 200]);
        let api = FlyApi::with_parts("app", &base, numbered_refused);
        let reply = api
            .call("GET", "/machines", None, None, Duration::from_secs(5))
            .unwrap();
        assert_eq!(reply.status, 200);
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_ne!(seen[0], seen[1], "the retry carries a new token");
    }

    static STILL: AtomicUsize = AtomicUsize::new(0);
    #[allow(clippy::unnecessary_wraps)]
    fn numbered_still() -> Result<Secret, VivariumError> {
        Ok(Secret::new(format!(
            "tok{}",
            STILL.fetch_add(1, Ordering::SeqCst)
        )))
    }

    #[test]
    fn a_token_refused_twice_is_an_error_not_a_loop() {
        let (base, seen) = stub(vec![401, 401, 200]);
        let api = FlyApi::with_parts("app", &base, numbered_still);
        let err = api
            .require("GET", "/machines", None, None, &[200])
            .unwrap_err()
            .to_string();
        assert!(err.contains("HTTP 401"), "{err}");
        assert_eq!(seen.lock().unwrap().len(), 2);
    }

    static AGED: AtomicUsize = AtomicUsize::new(0);
    #[allow(clippy::unnecessary_wraps)]
    fn numbered_aged() -> Result<Secret, VivariumError> {
        Ok(Secret::new(format!(
            "tok{}",
            AGED.fetch_add(1, Ordering::SeqCst)
        )))
    }

    #[test]
    fn an_old_token_is_not_reused_across_calls() {
        let (base, seen) = stub(vec![200, 200]);
        let mut api = FlyApi::with_parts("app", &base, numbered_aged);
        api.tokens = Arc::new(TokenCache::new(Duration::ZERO, numbered_aged));
        for _ in 0..2 {
            api.call("GET", "/machines", None, None, Duration::from_secs(5))
                .unwrap();
        }
        let seen = seen.lock().unwrap();
        assert_ne!(seen[0], seen[1]);
    }

    static GONE: AtomicUsize = AtomicUsize::new(0);
    #[allow(clippy::unnecessary_wraps)]
    fn numbered_gone() -> Result<Secret, VivariumError> {
        Ok(Secret::new(format!(
            "tok{}",
            GONE.fetch_add(1, Ordering::SeqCst)
        )))
    }

    #[test]
    fn a_machine_the_api_does_not_know_reads_as_destroyed_other_failures_stay_errors() {
        let (base, _) = stub(vec![404, 500]);
        let api = FlyApi::with_parts("app", &base, numbered_gone);
        let gone = api.machine("m-1").unwrap();
        assert_eq!(gone.state, "destroyed");
        assert_eq!(gone.id, "m-1");
        assert!(gone.metadata.is_empty());
        let err = api.machine("m-2").unwrap_err().to_string();
        assert!(err.contains("HTTP 500"), "{err}");
    }
}
