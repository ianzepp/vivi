use super::*;

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
    "\n[sync]\nserver = \"https://sync.example/\"\nproject = \"abcde-fghjk\"\nname = \"alice\"\n"
}

#[test]
fn a_complete_table_resolves_and_trims_the_server_url() {
    let (_tmp, mailspace) = bare_mailspace(complete());
    let settings = SyncSettings::resolve(&mailspace).unwrap();

    assert_eq!(settings.server, "https://sync.example");
    assert_eq!(settings.project, "abcde-fghjk");
    assert_eq!(settings.name, "alice");
    assert_eq!(settings.mailspace_id, mailspace.mailspace_id());
    assert_eq!(settings.mailspace_id.len(), 36);
}

#[test]
fn a_missing_sync_table_says_what_to_add() {
    let (_tmp, mailspace) = bare_mailspace("");
    let error = SyncSettings::resolve(&mailspace).unwrap_err().to_string();

    assert!(error.contains("[sync]"), "{error}");
    assert!(error.contains("server"), "{error}");
}

#[test]
fn each_missing_or_blank_key_is_named() {
    let keys = ["server", "project", "name"];
    for key in keys {
        let lines: Vec<String> = keys
            .iter()
            .filter(|other| **other != key)
            .map(|other| format!("{other} = \"x\""))
            .collect();
        let missing = format!("\n[sync]\n{}\n", lines.join("\n"));
        let (_tmp, mailspace) = bare_mailspace(&missing);
        let error = SyncSettings::resolve(&mailspace).unwrap_err().to_string();
        assert!(
            error.contains(&format!("[sync] {key} is not set")),
            "{key}: {error}"
        );

        let blank = format!("{missing}{key} = \"  \"\n");
        let (_tmp, mailspace) = bare_mailspace(&blank);
        let error = SyncSettings::resolve(&mailspace).unwrap_err().to_string();
        assert!(
            error.contains(&format!("[sync] {key} is not set")),
            "blank {key}: {error}"
        );
    }
}

#[test]
fn keys_older_versions_wrote_are_ignored_and_dropped_on_rewrite() {
    let table = format!(
        "{}recipient = \"mind\"\n\n[sync.names]\nhand = \"bob\"\n",
        complete()
    );
    let (tmp, mut mailspace) = bare_mailspace(&table);

    let settings = SyncSettings::resolve(&mailspace).unwrap();
    assert_eq!(settings.name, "alice");
    mailspace.set_description(Some("rewritten".into())).unwrap();

    let raw = std::fs::read_to_string(tmp.path().join(".vivi/mailspace.toml")).unwrap();
    assert!(raw.contains("description = \"rewritten\""), "{raw}");
    // Targeted toml_edit writes keep other keys, including retired ones.
    // Resolve still ignores them.
    assert!(raw.contains("recipient"), "{raw}");
}

#[test]
fn a_project_id_must_be_safe_in_a_url_path() {
    let (_tmp, mailspace) = bare_mailspace(&complete().replace("abcde-fghjk", "a/b?c"));
    let error = SyncSettings::resolve(&mailspace).unwrap_err().to_string();

    assert!(error.contains("[sync] project"), "{error}");
}

#[test]
fn the_sync_table_survives_a_config_rewrite() {
    let (tmp, mut mailspace) = bare_mailspace(complete());
    mailspace.set_description(Some("rewritten".into())).unwrap();

    let reopened = Mailspace::open_root(tmp.path().to_path_buf()).unwrap();
    let sync = reopened.config.sync.expect("[sync] kept");
    assert_eq!(sync.server.as_deref(), Some("https://sync.example/"));
    assert_eq!(sync.project.as_deref(), Some("abcde-fghjk"));
    assert_eq!(sync.name.as_deref(), Some("alice"));
    assert_eq!(reopened.config.id, mailspace.config.id, "the id survives");
}
