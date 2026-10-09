use super::*;

use crate::mailspace::MailspaceConfig;

#[test]
fn parses_chat_table() {
    let raw = r#"
        name = "demo"
        [chat]
        server = "https://chat.internal"
        name = "demo-seat"
    "#;
    let config: MailspaceConfig = toml::from_str(raw).expect("parse [chat]");
    let chat = config.chat.expect("chat table present");
    assert_eq!(chat.server.as_deref(), Some("https://chat.internal"));
    assert!(chat.notify, "notify defaults to true");
    assert_eq!(chat.name.as_deref(), Some("demo-seat"));
}

#[test]
fn chat_table_is_optional_and_notify_can_be_disabled() {
    let config: MailspaceConfig = toml::from_str("name = \"demo\"").expect("parse minimal");
    assert!(config.chat.is_none());
    let raw = "name = \"demo\"\n[chat]\nnotify = false\n";
    let config: MailspaceConfig = toml::from_str(raw).expect("parse notify false");
    assert!(!config.chat.expect("chat").notify);
}

#[test]
fn ensure_chat_dir_writes_self_gitignore() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("chat");
    ensure_chat_dir(&dir).expect("ensure chat dir");
    let body = fs::read_to_string(dir.join(".gitignore")).expect("gitignore readable");
    assert_eq!(body, "*\n");
}
