//! Purge timing cases (`tests/host_archive.rs` child module): purge is
//! only allowed once the archive has aged past its 24-hour delay, judged
//! by the fixture clock (`Fx::clock`, `Arc<AtomicI64>`) — moving the
//! atomic forward simulates elapsed time, and no archive timestamp in
//! metadata is ever altered by hand. The ruling is universal: one second
//! before the deadline no principal — host operator token or minted
//! `host: true` server key — can purge, and the refusal names the instant
//! the delay allows. A project that is not archived, and a confirm that
//! does not repeat the name, are refused before anything moves.

use std::path::PathBuf;
use std::sync::atomic::Ordering;

use serde_json::{Value, json};

/// The purge delay under test, in seconds: one day.
const DAY_SECONDS: i64 = 24 * 60 * 60;

/// The project tree triple a refused purge must leave untouched: the
/// host projects root (the mailspace), the lanes root, and the server's
/// own `DATA_DIR/projects` (the archive record lives there). The audit
/// log beside `projects/` is excluded: a refusal itself writes one line.
fn trees(fx: &super::Fx) -> (Vec<(String, u64)>, Vec<(String, u64)>, Vec<(String, u64)>) {
    (
        super::snapshot(&fx.projects),
        super::snapshot(&fx.lanes),
        super::snapshot(&fx.data.join("projects")),
    )
}

/// Grants and usable keys across the whole store, as comparable JSON
/// (digests only; `revoked_at` included, so a revocation shows).
fn keys(fx: &super::Fx) -> Value {
    serde_json::to_value(fx.state.server_keys.list()).unwrap()
}

type Before = (
    Value,
    (Vec<(String, u64)>, Vec<(String, u64)>, Vec<(String, u64)>),
    Value,
);

/// Assert the three trees, the project metadata and the key store are
/// exactly as the captured `before` state left them.
fn assert_untouched(fx: &super::Fx, id: &str, before: Before) {
    let (meta_before, trees_before, keys_before) = before;
    assert_eq!(fx.project_json(id), meta_before, "project metadata");
    assert_eq!(trees(fx), trees_before, "the three trees");
    assert_eq!(keys(fx), keys_before, "grants and usable keys");
}

/// The `project_list` entry for `id`, with its `mode`.
async fn listed_entry(fx: &super::Fx, id: &str) -> Value {
    let (_, listed) = fx.call_tool("project_list", json!({})).await;
    listed
        .as_array()
        .expect("project_list answers an array")
        .iter()
        .find(|item| item["project_id"] == json!(id))
        .cloned()
        .unwrap_or_else(|| panic!("project {id} is listed: {listed:?}"))
}

/// Archive a created project through the archive tool at the fixture
/// clock, returning the created project's id and the archive answer.
async fn create_and_archive(fx: &super::Fx, name: &str) -> (String, Value) {
    let (errored, made) = fx
        .create(json!({ "name": name, "initial_commit": true }))
        .await;
    assert!(!errored, "{made}");
    let id = made["project_id"].as_str().unwrap().to_owned();

    let (errored, archived) = fx
        .call_tool("archive_project", json!({ "name": name, "confirm": name }))
        .await;
    assert!(!errored, "{archived}");
    assert_eq!(
        archived["archived"]["at"],
        json!(super::rfc3339(super::BASE_SECONDS)),
        "{archived}"
    );
    (id, archived)
}

/// One second before the 24-hour deadline the purge is refused through
/// the MCP tool, the refusal names the instant the delay allows, and
/// nothing moved: metadata, all three trees, grants and keys stay, and
/// the host's `project_list` still shows the project archived.
#[tokio::test]
async fn purge_is_refused_before_the_delay_and_names_when_it_is_allowed() {
    let fx = super::fixture("purge-early");
    let name = "purge-timing-early";
    let (id, _archived) = create_and_archive(&fx, name).await;

    // The archive stamp stands at the fixture clock T; move only the
    // clock, to one second before the delay allows.
    let deadline = super::BASE_SECONDS + DAY_SECONDS;
    fx.clock.store(deadline - 1, Ordering::SeqCst);

    let before: Before = (fx.project_json(&id), trees(&fx), keys(&fx));
    let (errored, answer) = fx
        .call_tool("purge_project", json!({ "name": name, "confirm": name }))
        .await;
    assert!(errored, "{answer}");
    let error = answer["error"].as_str().unwrap();
    assert!(error.contains("purge is allowed from"), "{error}");
    assert!(
        error.contains(&super::rfc3339(deadline)),
        "the refusal names the computed allowed instant: {error}"
    );

    assert_untouched(&fx, &id, before);
    let entry = listed_entry(&fx, &id).await;
    assert_eq!(entry["mode"], "archived", "{entry}");
}

/// The delay binds every host principal: one hour after the archive, the
/// host operator token and a minted `host: true` server key — two
/// authentication paths onto the same universal rule — are both refused
/// with the same allowed instant, and nothing is destroyed.
#[tokio::test]
async fn the_host_principal_cannot_purge_early() {
    let fx = super::fixture("purge-host");
    let name = "purge-timing-host";
    let (id, _archived) = create_and_archive(&fx, name).await;

    // One hour in: well inside the delay.
    fx.clock.store(super::BASE_SECONDS + 3600, Ordering::SeqCst);

    // A second host principal on a different authentication path: a
    // minted host server key, no project grants at all.
    let (errored, minted) = fx
        .call_tool("key_mint", json!({ "host": true, "label": "purge probe" }))
        .await;
    assert!(!errored, "{minted}");
    assert_eq!(minted["host"], true, "{minted}");
    let host_key = minted["key"].as_str().unwrap().to_owned();

    let deadline = super::BASE_SECONDS + DAY_SECONDS;
    let before: Before = (fx.project_json(&id), trees(&fx), keys(&fx));
    for (who, bearer) in [
        ("host operator token", super::TOKEN),
        ("minted host key", host_key.as_str()),
    ] {
        let (errored, answer) = fx
            .call_as(
                bearer,
                "purge_project",
                json!({ "name": name, "confirm": name }),
            )
            .await;
        assert!(errored, "{who}: {answer}");
        let error = answer["error"].as_str().unwrap();
        assert!(
            error.contains(&super::rfc3339(deadline)),
            "{who} is refused with the allowed instant: {error}"
        );
    }

    assert_untouched(&fx, &id, before);
    let entry = listed_entry(&fx, &id).await;
    assert_eq!(entry["mode"], "archived", "{entry}");
}

/// A project that is not archived refuses the purge before anything
/// moves: the issue says not-archived, and the metadata, the three
/// trees, the grants and keys, and the registry membership of the
/// project's own root are exactly as they were.
#[tokio::test]
async fn purge_refuses_an_unarchived_project() {
    let fx = super::fixture("purge-live");
    let name = "purge-timing-live";
    let (errored, made) = fx
        .create(json!({ "name": name, "initial_commit": true }))
        .await;
    assert!(!errored, "{made}");
    let id = made["project_id"].as_str().unwrap().to_owned();
    let root = PathBuf::from(made["project_root"].as_str().unwrap());
    let registered = vivi::registry::load().unwrap().get(&root);
    assert!(registered.is_some(), "a served creation registers its root");

    let before: Before = (fx.project_json(&id), trees(&fx), keys(&fx));
    let (errored, answer) = fx
        .call_tool("purge_project", json!({ "name": name, "confirm": name }))
        .await;
    assert!(errored, "{answer}");
    assert!(
        answer["error"].as_str().unwrap().contains("not archived"),
        "{answer}"
    );

    assert_untouched(&fx, &id, before);
    assert_eq!(
        vivi::registry::load().unwrap().get(&root),
        registered,
        "the project stays registered"
    );
}

/// A confirm that does not repeat the name refuses the purge before any
/// metadata is read — even with the clock already past the deadline, so
/// only the confirmation stands between the call and the destruction.
/// Nothing moves, and the archived project stays out of the registry.
#[tokio::test]
async fn purge_refuses_a_bad_confirm() {
    let fx = super::fixture("purge-confirm");
    let name = "purge-timing-confirm";
    let (id, archived) = create_and_archive(&fx, name).await;
    let root = PathBuf::from(archived["root"].as_str().unwrap());
    let registered = vivi::registry::load().unwrap().get(&root);
    assert!(registered.is_none(), "the archive unregistered the root");

    // Past the deadline: the refusal must come from the confirm alone.
    fx.clock
        .store(super::BASE_SECONDS + DAY_SECONDS, Ordering::SeqCst);

    let before: Before = (fx.project_json(&id), trees(&fx), keys(&fx));
    let (errored, answer) = fx
        .call_tool(
            "purge_project",
            json!({ "name": name, "confirm": "not-the-name" }),
        )
        .await;
    assert!(errored, "{answer}");
    assert!(
        answer["error"].as_str().unwrap().contains("confirmation"),
        "{answer}"
    );

    assert_untouched(&fx, &id, before);
    assert_eq!(
        vivi::registry::load().unwrap().get(&root),
        registered,
        "the archived root stays unregistered"
    );
    let entry = listed_entry(&fx, &id).await;
    assert_eq!(entry["mode"], "archived", "{entry}");
}
