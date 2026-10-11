use super::*;
use crate::sync::fake_server::checkout;

fn bare_mailspace(sync: &str) -> (tempfile::TempDir, Mailspace) {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("mind").unwrap();
    mailspace.add_identity("hand").unwrap();
    let path = mailspace.dir.join("mailspace.toml");
    let mut raw = std::fs::read_to_string(&path).unwrap();
    raw.push_str(sync);
    std::fs::write(&path, raw).unwrap();
    let mailspace = Mailspace::open_root(tmp.path().to_path_buf()).unwrap();
    (tmp, mailspace)
}

fn complete() -> &'static str {
    "\n[sync]\nserver = \"https://sync.example/\"\nproject = \"abcde-fghjk\"\nname = \"alice\"\n\
     recipient = \"mind\"\n"
}

#[test]
fn a_complete_table_resolves_and_trims_the_server_url() {
    let (_tmp, mailspace) = bare_mailspace(complete());
    let settings = SyncSettings::resolve(&mailspace, true).unwrap();

    assert_eq!(settings.server, "https://sync.example");
    assert_eq!(settings.project, "abcde-fghjk");
    assert_eq!(settings.name, "alice");
    assert_eq!(settings.recipient.as_deref(), Some("mind"));
}

#[test]
fn a_missing_sync_table_says_what_to_add() {
    let (_tmp, mailspace) = bare_mailspace("");
    let error = SyncSettings::resolve(&mailspace, false)
        .unwrap_err()
        .to_string();

    assert!(error.contains("[sync]"), "{error}");
    assert!(error.contains("server"), "{error}");
}

#[test]
fn each_missing_or_blank_key_is_named() {
    let keys = ["server", "project", "name", "recipient"];
    for key in keys {
        let lines: Vec<String> = keys
            .iter()
            .filter(|other| **other != key)
            .map(|other| {
                format!(
                    "{other} = \"{}\"",
                    if *other == "recipient" { "mind" } else { "x" }
                )
            })
            .collect();
        let missing = format!("\n[sync]\n{}\n", lines.join("\n"));
        let (_tmp, mailspace) = bare_mailspace(&missing);
        let error = SyncSettings::resolve(&mailspace, true)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(&format!("[sync] {key} is not set")),
            "{key}: {error}"
        );

        let blank = format!("{missing}{key} = \"  \"\n");
        let (_tmp, mailspace) = bare_mailspace(&blank);
        let error = SyncSettings::resolve(&mailspace, true)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(&format!("[sync] {key} is not set")),
            "blank {key}: {error}"
        );
    }
}

#[test]
fn recipient_must_resolve_to_a_local_role_but_only_pulls_need_it() {
    let unknown = complete().replace("\"mind\"", "\"nobody\"");
    let (_tmp, mailspace) = bare_mailspace(&unknown);

    let error = SyncSettings::resolve(&mailspace, true)
        .unwrap_err()
        .to_string();
    assert!(error.contains("recipient"), "{error}");
    assert!(error.contains("nobody"), "{error}");

    let settings = SyncSettings::resolve(&mailspace, false).unwrap();
    assert_eq!(settings.recipient, None, "push does not resolve it");
}

#[test]
fn a_project_id_must_be_safe_in_a_url_path() {
    let (_tmp, mailspace) = bare_mailspace(&complete().replace("abcde-fghjk", "a/b?c"));
    let error = SyncSettings::resolve(&mailspace, false)
        .unwrap_err()
        .to_string();

    assert!(error.contains("[sync] project"), "{error}");
}

#[test]
fn a_blank_wire_name_is_refused() {
    let table = format!("{}\n[sync.names]\nhand = \" \"\n", complete());
    let (_tmp, mailspace) = bare_mailspace(&table);
    let error = SyncSettings::resolve(&mailspace, false)
        .unwrap_err()
        .to_string();

    assert!(error.contains("[sync.names] hand"), "{error}");
}

#[test]
fn sync_names_map_local_roles_to_wire_names_else_the_checkout_name() {
    let fx = checkout(
        "alice",
        "mind",
        &["mind", "hand", "peer"],
        &[("peer", "bob")],
    );

    assert_eq!(fx.settings.wire_name(&fx.mailspace, "peer"), "bob");
    let address = fx.mailspace.address_for("peer");
    assert_eq!(fx.settings.wire_name(&fx.mailspace, &address), "bob");
    assert_eq!(fx.settings.wire_name(&fx.mailspace, "hand"), "alice");
    assert_eq!(
        fx.settings.wire_name(&fx.mailspace, "unknown-role"),
        "alice"
    );
}

#[test]
fn the_sync_table_survives_a_config_rewrite() {
    let table = format!("{}\n[sync.names]\nhand = \"bob\"\n", complete());
    let (tmp, mut mailspace) = bare_mailspace(&table);
    mailspace.set_description(Some("rewritten".into())).unwrap();

    let reopened = Mailspace::open_root(tmp.path().to_path_buf()).unwrap();
    let sync = reopened.config.sync.expect("[sync] kept");
    assert_eq!(sync.server.as_deref(), Some("https://sync.example/"));
    assert_eq!(sync.project.as_deref(), Some("abcde-fghjk"));
    assert_eq!(sync.name.as_deref(), Some("alice"));
    assert_eq!(sync.recipient.as_deref(), Some("mind"));
    assert_eq!(sync.names.get("hand").map(String::as_str), Some("bob"));
}
