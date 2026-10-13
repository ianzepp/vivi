//! Tests for `vivi mcp` resolution: flags, environment, and the mailspace
//! fallback, all through lookups, so no test touches the disk.

use std::cell::Cell;
use std::path::PathBuf;

use super::*;

/// An environment lookup answering nothing.
fn no_env() -> impl Fn(&str) -> Option<String> {
    |_| None
}

/// An environment lookup backed by a fixed table.
fn env_of(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + '_ {
    let table: Vec<(String, String)> = vars
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect();
    move |name| {
        table
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    }
}

/// A mailspace source with `.vivi` at `/mailspace/.vivi`.
fn space(server: Option<&str>, project: Option<&str>, name: Option<&str>) -> MailspaceSource {
    MailspaceSource {
        server: server.map(str::to_string),
        project: project.map(str::to_string),
        name: name.map(str::to_string),
        vivi_dir: PathBuf::from("/mailspace/.vivi"),
    }
}

/// A mailspace lookup always finding `found`.
fn finding(found: MailspaceSource) -> impl Fn() -> Result<Option<MailspaceSource>, VivariumError> {
    move || Ok(Some(found.clone()))
}

/// A mailspace lookup finding nothing.
fn nothing() -> impl Fn() -> Result<Option<MailspaceSource>, VivariumError> {
    || Ok(None)
}

/// A mailspace lookup that counts calls, wrapped so the test can assert it
/// was never consulted.
fn counted(
    found: Option<MailspaceSource>,
) -> (
    impl Fn() -> Result<Option<MailspaceSource>, VivariumError>,
    Cell<usize>,
) {
    let calls = Cell::new(0);
    let counter = Cell::clone(&calls);
    let lookup = move || {
        counter.set(counter.get() + 1);
        Ok(found.clone())
    };
    (lookup, calls)
}

fn flags_with(
    server: Option<&str>,
    project_id: Option<&str>,
    name: Option<&str>,
    key_file: Option<&str>,
) -> McpFlags {
    McpFlags {
        server: server.map(str::to_string),
        project_id: project_id.map(str::to_string),
        name: name.map(str::to_string),
        key_file: key_file.map(PathBuf::from),
        project: None,
    }
}

#[test]
fn a_flag_beats_a_variable_and_a_variable_beats_the_sync_table() {
    let flags = flags_with(
        Some("http://flag"),
        Some("flagproj"),
        Some("flagname"),
        Some("/keys/alice.key"),
    );
    let env = env_of(&[
        (ENV_SERVER, "http://env"),
        (ENV_PROJECT_ID, "envproj"),
        (ENV_NAME, "envname"),
    ]);

    let resolved = resolve(
        &flags,
        env,
        finding(space(
            Some("http://sync"),
            Some("syncproj"),
            Some("syncname"),
        )),
    )
    .expect("resolves");

    assert_eq!(resolved.settings.server, "http://flag");
    assert_eq!(resolved.settings.project, "flagproj");
    assert_eq!(resolved.settings.name, "flagname");
    assert!(matches!(
        resolved.credential,
        PendingCredential::KeyFile(ref path) if path == &PathBuf::from("/keys/alice.key")
    ));
}

#[test]
fn a_variable_beats_the_sync_table_and_the_table_fills_the_rest() {
    let flags = flags_with(None, None, None, Some("/keys/alice.key"));
    let env = env_of(&[(ENV_SERVER, "http://env"), (ENV_PROJECT_ID, "envproj")]);

    let resolved = resolve(
        &flags,
        env,
        finding(space(
            Some("http://sync/"),
            Some("syncproj"),
            Some("syncname"),
        )),
    )
    .expect("resolves");

    assert_eq!(
        resolved.settings.server, "http://env",
        "variable beats [sync]"
    );
    assert_eq!(resolved.settings.project, "envproj");
    assert_eq!(resolved.settings.name, "syncname", "[sync] fills the name");
}

#[test]
fn without_a_variable_or_flag_the_sync_table_is_the_source() {
    let flags = flags_with(None, None, None, Some("/keys/alice.key"));

    let resolved = resolve(
        &flags,
        no_env(),
        finding(space(
            Some("http://sync"),
            Some("syncproj"),
            Some("syncname"),
        )),
    )
    .expect("resolves");

    assert_eq!(resolved.settings.server, "http://sync");
    assert_eq!(resolved.settings.project, "syncproj");
    assert_eq!(resolved.settings.name, "syncname");
}

#[test]
fn an_api_key_alone_needs_no_name_and_no_mailspace() {
    let flags = flags_with(Some("http://server"), Some("proj"), None, None);
    let env = env_of(&[(ENV_API_KEY, "vivi_secret")]);
    let (lookup, calls) = counted(None);

    let resolved = resolve(&flags, env, lookup).expect("resolves");

    assert_eq!(
        calls.get(),
        0,
        "nothing was missing, so no mailspace lookup"
    );
    assert_eq!(resolved.settings.name, "", "an API key needs no name");
    assert!(matches!(
        resolved.credential,
        PendingCredential::ApiKey(ref key) if key == "vivi_secret"
    ));
}

#[test]
fn an_api_key_with_a_name_or_key_file_is_refused() {
    let env = env_of(&[(ENV_API_KEY, "vivi_secret")]);
    for flags in [
        flags_with(Some("http://s"), Some("p"), Some("alice"), None),
        flags_with(Some("http://s"), Some("p"), None, Some("/keys/alice.key")),
    ] {
        let error = resolve(&flags, env, nothing()).expect_err("refused");
        assert!(
            error.to_string().contains("not both"),
            "unexpected refusal: {error}"
        );
    }
}

#[test]
fn a_key_file_without_a_name_anywhere_is_refused() {
    let flags = flags_with(
        Some("http://server"),
        Some("proj"),
        None,
        Some("/keys/alice.key"),
    );

    let error = resolve(&flags, no_env(), nothing()).expect_err("refused");

    let message = error.to_string();
    assert!(message.contains("--name"), "{message}");
    assert!(message.contains("VIVI_MCP_NAME"), "{message}");
    assert!(message.contains("[sync] name"), "{message}");
}

#[test]
fn nothing_resolvable_lists_exactly_what_to_provide() {
    let flags = McpFlags::default();

    let error = resolve(&flags, no_env(), nothing()).expect_err("refused");

    let message = error.to_string();
    assert!(message.contains("VIVI_MCP_API_KEY"), "{message}");
    assert!(message.contains("--name"), "{message}");
    assert!(message.contains("--key-file"), "{message}");
}

#[test]
fn flags_and_variables_alone_never_consult_the_mailspace() {
    let flags = flags_with(
        Some("http://server"),
        Some("proj"),
        Some("alice"),
        Some("/keys/alice.key"),
    );
    let (lookup, calls) = counted(Some(space(Some("http://sync"), None, None)));

    resolve(&flags, no_env(), lookup).expect("resolves");
    assert_eq!(calls.get(), 0, "the mailspace must not be looked up");
}

#[test]
fn environment_alone_never_consults_the_mailspace() {
    let env = env_of(&[
        (ENV_SERVER, "http://server"),
        (ENV_PROJECT_ID, "proj"),
        (ENV_API_KEY, "vivi_secret"),
    ]);
    let (lookup, calls) = counted(Some(space(Some("http://sync"), None, None)));

    resolve(&McpFlags::default(), env, lookup).expect("resolves");
    assert_eq!(calls.get(), 0);
}

#[test]
fn a_trailing_slash_on_the_server_is_trimmed() {
    for server in ["http://flag-server/", "http://flag-server"] {
        let flags = flags_with(Some(server), Some("proj"), Some("alice"), Some("/keys/k"));

        let resolved = resolve(&flags, no_env(), nothing()).expect("resolves");

        assert_eq!(resolved.settings.server, "http://flag-server", "{server}");
    }
}

#[test]
fn an_invalid_project_id_is_refused() {
    let flags = flags_with(
        Some("http://server"),
        Some("bad id!"),
        Some("alice"),
        Some("/keys/k"),
    );

    let error = resolve(&flags, no_env(), nothing()).expect_err("refused");

    assert!(error.to_string().contains("not a project id"), "{error}");
}

#[test]
fn the_mailspace_key_is_chat_identity_under_the_vivi_dir() {
    let flags = flags_with(Some("http://server"), Some("proj"), None, None);

    let resolved =
        resolve(&flags, no_env(), finding(space(None, None, Some("alice")))).expect("resolves");

    assert_eq!(resolved.settings.name, "alice");
    assert_eq!(
        resolved.credential,
        PendingCredential::MailspaceKey(PathBuf::from("/mailspace/.vivi/chat/identity.ed25519"))
    );
}

#[test]
fn a_mailspace_lookup_error_is_passed_through() {
    let flags = flags_with(None, None, None, None);
    let broken = |_name: &str| None;
    let lookup = || -> Result<Option<MailspaceSource>, VivariumError> {
        Err(VivariumError::Message("unreadable mailspace.toml".into()))
    };

    let error = resolve(&flags, broken, lookup).expect_err("the lookup error surfaces");

    assert!(
        error.to_string().contains("unreadable mailspace.toml"),
        "{error}"
    );
}

#[test]
fn an_empty_environment_value_counts_as_unset() {
    let flags = flags_with(None, None, None, None);
    let env = env_of(&[(ENV_SERVER, ""), (ENV_PROJECT_ID, ""), (ENV_NAME, "")]);

    let error = resolve(&flags, env, nothing()).expect_err("empty means unset");

    assert!(error.to_string().contains("VIVI_MCP_SERVER"), "{error}");
}
