//! Tests for `vivi mcp` resolution: flags, environment, and the mailspace
//! fallback, all through lookups, so no test touches the disk.

use std::cell::Cell;

use super::*;

/// An environment lookup answering nothing.
fn no_env() -> impl Fn(&str) -> Option<String> {
    |_| None
}

/// An environment lookup backed by a fixed table.
fn env_of<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
    let table: Vec<(String, String)> = vars
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect();
    move |name| {
        table
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
            .filter(|value| !value.is_empty())
    }
}

/// A mailspace source with the given `[sync]` values.
fn space(server: Option<&str>, project: Option<&str>) -> MailspaceSource {
    MailspaceSource {
        server: server.map(str::to_string),
        project: project.map(str::to_string),
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

fn flags_with(server: Option<&str>, project_id: Option<&str>) -> McpFlags {
    McpFlags {
        server: server.map(str::to_string),
        project_id: project_id.map(str::to_string),
        project: None,
    }
}

/// An environment holding only the API key.
fn key_env() -> impl Fn(&str) -> Option<String> {
    env_of(&[(ENV_API_KEY, "vivi_secret")])
}

#[test]
fn a_flag_beats_a_variable_and_a_variable_beats_the_sync_table() {
    let flags = flags_with(Some("http://flag"), Some("flagproj"));
    let env = env_of(&[
        (ENV_SERVER, "http://env"),
        (ENV_PROJECT_ID, "envproj"),
        (ENV_API_KEY, "vivi_secret"),
    ]);

    let resolved = resolve(
        &flags,
        env,
        finding(space(Some("http://sync"), Some("syncproj"))),
    )
    .expect("resolves");

    assert_eq!(resolved.server, "http://flag");
    assert_eq!(resolved.project.as_deref(), Some("flagproj"));
    assert_eq!(resolved.api_key, "vivi_secret");
}

#[test]
fn a_variable_beats_the_sync_table_and_the_table_fills_the_rest() {
    let env = env_of(&[(ENV_SERVER, "http://env"), (ENV_API_KEY, "vivi_secret")]);

    let resolved = resolve(
        &McpFlags::default(),
        env,
        finding(space(Some("http://sync/"), Some("syncproj"))),
    )
    .expect("resolves");

    assert_eq!(resolved.server, "http://env", "variable beats [sync]");
    assert_eq!(
        resolved.project.as_deref(),
        Some("syncproj"),
        "[sync] fills the project"
    );
}

#[test]
fn without_a_variable_or_flag_the_sync_table_is_the_source() {
    let resolved = resolve(
        &McpFlags::default(),
        key_env(),
        finding(space(Some("http://sync"), Some("syncproj"))),
    )
    .expect("resolves");

    assert_eq!(resolved.server, "http://sync");
    assert_eq!(resolved.project.as_deref(), Some("syncproj"));
}

#[test]
fn no_project_anywhere_resolves_with_none() {
    let flags = flags_with(Some("http://server"), None);

    let resolved = resolve(&flags, key_env(), nothing()).expect("resolves");

    assert_eq!(resolved.project, None);
    assert_eq!(resolved.server, "http://server");
}

#[test]
fn a_missing_api_key_is_refused_naming_the_variable() {
    let flags = flags_with(Some("http://server"), Some("proj"));

    let error = resolve(&flags, no_env(), nothing()).expect_err("refused");

    let message = error.to_string();
    assert!(message.contains("VIVI_MCP_API_KEY"), "{message}");
    assert!(message.contains("vivi sync api-key mint"), "{message}");
    assert!(message.contains("API keys only"), "{message}");
}

#[test]
fn nothing_resolvable_lists_the_server_and_the_key() {
    let error = resolve(&McpFlags::default(), no_env(), nothing()).expect_err("refused");

    let message = error.to_string();
    assert!(message.contains("VIVI_MCP_SERVER"), "{message}");
    assert!(message.contains("VIVI_MCP_API_KEY"), "{message}");
}

#[test]
fn flags_and_variables_alone_never_consult_the_mailspace() {
    let flags = flags_with(Some("http://server"), Some("proj"));
    let (lookup, calls) = counted(Some(space(Some("http://sync"), None)));

    resolve(&flags, key_env(), lookup).expect("resolves");
    assert_eq!(calls.get(), 0, "the mailspace must not be looked up");
}

#[test]
fn environment_alone_never_consults_the_mailspace() {
    let env = env_of(&[
        (ENV_SERVER, "http://server"),
        (ENV_PROJECT_ID, "proj"),
        (ENV_API_KEY, "vivi_secret"),
    ]);
    let (lookup, calls) = counted(Some(space(Some("http://sync"), None)));

    resolve(&McpFlags::default(), env, lookup).expect("resolves");
    assert_eq!(calls.get(), 0);
}

#[test]
fn a_trailing_slash_on_the_server_is_trimmed() {
    for server in ["http://flag-server/", "http://flag-server"] {
        let flags = flags_with(Some(server), Some("proj"));

        let resolved = resolve(&flags, key_env(), nothing()).expect("resolves");

        assert_eq!(resolved.server, "http://flag-server", "{server}");
    }
}

#[test]
fn an_invalid_project_id_is_refused() {
    let flags = flags_with(Some("http://server"), Some("bad id!"));

    let error = resolve(&flags, key_env(), nothing()).expect_err("refused");

    assert!(error.to_string().contains("not a project id"), "{error}");
}

#[test]
fn a_mailspace_lookup_error_is_passed_through() {
    let lookup = || -> Result<Option<MailspaceSource>, VivariumError> {
        Err(VivariumError::Message("unreadable mailspace.toml".into()))
    };

    let error =
        resolve(&McpFlags::default(), no_env(), lookup).expect_err("the lookup error surfaces");

    assert!(
        error.to_string().contains("unreadable mailspace.toml"),
        "{error}"
    );
}

#[test]
fn an_empty_environment_value_counts_as_unset() {
    let env = env_of(&[(ENV_SERVER, ""), (ENV_PROJECT_ID, ""), (ENV_API_KEY, "")]);

    let error = resolve(&McpFlags::default(), env, nothing()).expect_err("empty means unset");

    assert!(error.to_string().contains("VIVI_MCP_SERVER"), "{error}");
}

#[test]
fn resolve_itself_treats_an_empty_value_as_unset() {
    // No filtering in the lookup at all: every variable is Some("").
    let flags = flags_with(Some("http://server"), Some("proj"));

    let error = resolve(&flags, |_| Some(String::new()), nothing()).expect_err("no key");

    assert!(error.to_string().contains("VIVI_MCP_API_KEY"), "{error}");
}
