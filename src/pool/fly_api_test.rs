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
