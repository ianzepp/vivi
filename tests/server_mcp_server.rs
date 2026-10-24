//! The server-level MCP endpoint `/v1/mcp` (wave 3 D3): server keys and
//! grants, the pinned project refusals, name resolution, audit, the host
//! tools, the boot retirement of the old per-project key files, and the
//! self-service mint.

use std::collections::BTreeSet;
use std::path::PathBuf;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tower::util::ServiceExt;
use vivi::mailspace::{Mailspace, RoleUpdate};
use vivi_server::grants::{GrantLevel, GrantRecord};
use vivi_server::{AppState, app_state, hosted, router};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "vivi-server-mcp-server-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn spki_pem(signing: &SigningKey) -> String {
    const PREFIX: &[u8] = &[
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
    ];
    let mut der = PREFIX.to_vec();
    der.extend(signing.verifying_key().as_bytes());
    format!(
        "-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----",
        STANDARD.encode(der)
    )
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn mailspace_for(name: &str) -> String {
    let hex = sha256_hex(name.as_bytes());
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

fn prefix_for(name: &str) -> String {
    mailspace_for(name)[..8].to_owned()
}

fn address(name: &str) -> String {
    format!("{name}@{}.local", prefix_for(name))
}

/// One server key minted on `state`, granted as asked.
fn server_key(
    state: &AppState,
    project: &str,
    member: &str,
    identity: Option<&str>,
    level: GrantLevel,
) -> String {
    state
        .server_keys
        .mint(
            &format!("{member}@{project}"),
            false,
            vec![GrantRecord {
                project_id: project.to_owned(),
                member: member.to_owned(),
                identity: identity.map(str::to_owned),
                level,
            }],
            "test",
            member,
        )
        .unwrap()
        .key
}

/// A durable project with a registered `creator` and its identity token.
struct Durable {
    app: Router,
    state: AppState,
    dir: PathBuf,
    project: String,
    token: String,
}

fn durable_with(name: &str, tune: impl FnOnce(&mut AppState)) -> Durable {
    let base = scratch(name);
    let mut state = app_state(base.join("data"), None, None);
    tune(&mut state);
    let signing = key(1);
    let meta = state
        .projects
        .create_project_with_creator(&spki_pem(&signing), "creator")
        .unwrap();
    let outcome = state
        .identities
        .register(
            &meta.project_id,
            "creator",
            &spki_pem(&signing),
            &mailspace_for("creator"),
        )
        .unwrap();
    Durable {
        app: router(state.clone()),
        state,
        dir: base.join("data"),
        project: meta.project_id,
        token: outcome.token,
    }
}

fn durable(name: &str) -> Durable {
    durable_with(name, |_| {})
}

impl Durable {
    /// One more durable project, so cross-tenant refusals name a real
    /// tenant.
    fn second_project(&self) -> String {
        self.state
            .projects
            .create_project_with_creator(&spki_pem(&key(2)), "bob")
            .unwrap()
            .project_id
    }
}

/// A hosted fixture: one served root per name under `hosts/<i>/<name>`,
/// each a mailspace with operator, mind and hand roles. Roots that share a
/// name live under different parents, so two projects can carry one name.
struct Hosted {
    app: Router,
    state: AppState,
    dir: PathBuf,
    /// (project_id, member) per served root, in the order roots were given.
    served: Vec<(String, String)>,
}

fn hosted_with(name: &str, tune: impl FnOnce(&mut AppState), root_names: &[&str]) -> Hosted {
    let base = scratch(name);
    let mut roots = BTreeSet::new();
    for (index, root_name) in root_names.iter().enumerate() {
        let root = base.join("hosts").join(index.to_string()).join(root_name);
        let mut ms = Mailspace::init(Some(&root)).unwrap();
        ms.ensure_operator_role().unwrap();
        for (role, kind) in [("mind", "mind"), ("hand", "hand")] {
            ms.add_role(
                role,
                RoleUpdate {
                    kind: Some(Some(kind.into())),
                    ..RoleUpdate::default()
                },
            )
            .unwrap();
        }
        roots.insert(root);
    }
    let mut state = app_state(base.join("data"), None, None);
    tune(&mut state);
    let report = hosted::reconcile(&state.projects, &state.identities, &roots);
    assert!(report.refused.is_empty(), "{report:?}");
    let served = report
        .served
        .iter()
        .map(|s| (s.project_id.clone(), s.member.clone()))
        .collect();
    Hosted {
        app: router(state.clone()),
        state,
        dir: base.join("data"),
        served,
    }
}

/// One durable project and one hosted project ("app") in ONE server, so a
/// single key can be granted both and the `projects` tool sees both.
fn mixed(name: &str) -> (Router, AppState, PathBuf, String, (String, String)) {
    let base = scratch(name);
    let root = base.join("hosts").join("app");
    let mut ms = Mailspace::init(Some(&root)).unwrap();
    ms.ensure_operator_role().unwrap();
    for (role, kind) in [("mind", "mind"), ("hand", "hand")] {
        ms.add_role(
            role,
            RoleUpdate {
                kind: Some(Some(kind.into())),
                ..RoleUpdate::default()
            },
        )
        .unwrap();
    }
    let state = app_state(base.join("data"), None, None);
    let report = hosted::reconcile(&state.projects, &state.identities, &BTreeSet::from([root]));
    assert!(report.refused.is_empty(), "{report:?}");
    let hosted = (
        report.served[0].project_id.clone(),
        report.served[0].member.clone(),
    );
    let signing = key(1);
    let meta = state
        .projects
        .create_project_with_creator(&spki_pem(&signing), "creator")
        .unwrap();
    let _token = state
        .identities
        .register(
            &meta.project_id,
            "creator",
            &spki_pem(&signing),
            &mailspace_for("creator"),
        )
        .unwrap()
        .token;
    let app = router(state.clone());
    (app, state, base.join("data"), meta.project_id, hosted)
}

/// POST /v1/mcp as `bearer`.
async fn rpc(app: &Router, bearer: &str, method: &str, params: Value) -> (StatusCode, Value) {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/mcp")
                .header("authorization", format!("Bearer {bearer}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

/// A successful tool call, parsed from its text answer.
async fn call_ok(app: &Router, bearer: &str, name: &str, arguments: Value) -> Value {
    let (status, value) = rpc(
        app,
        bearer,
        "tools/call",
        json!({"name": name, "arguments": arguments}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value["error"], Value::Null, "{value}");
    assert_ne!(value["result"]["isError"], true, "{value}");
    let text = value["result"]["content"][0]["text"].as_str().unwrap();
    serde_json::from_str(text).unwrap_or_else(|_| json!({ "raw": text }))
}

/// A refused tool call: its exact error text.
async fn call_err(app: &Router, bearer: &str, name: &str, arguments: Value) -> String {
    let (status, value) = rpc(
        app,
        bearer,
        "tools/call",
        json!({"name": name, "arguments": arguments}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value["result"]["isError"], true, "{value}");
    let text = value["result"]["content"][0]["text"].as_str().unwrap();
    serde_json::from_str::<Value>(text).unwrap()["error"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// The audit lines written under `dir`.
fn audit_lines(dir: &PathBuf) -> Vec<Value> {
    std::fs::read_to_string(dir.join("audit.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn legacy_keys_file(count: usize) -> String {
    let keys: Vec<String> = (0..count)
        .map(|i| {
            format!(
                r#"{{"key_id":"key_{i}","label":"k{i}","digest":"{i:064}","created_by":"operator","created_at":"2026-01-01T00:00:00Z"}}"#
            )
        })
        .collect();
    format!("{{\"keys\":[{}]}}", keys.join(","))
}

// -------------------------------------------------------- pinned refusals

#[tokio::test]
async fn a_missing_project_is_the_pinned_tool_error() {
    let fx = durable("missing-project");
    let key = server_key(&fx.state, &fx.project, "creator", None, GrantLevel::Member);
    let err = call_err(&fx.app, &key, "overview", json!({})).await;
    assert_eq!(err, "project is required");
    // A non-string project counts as missing, as the wire contract pins.
    let err = call_err(&fx.app, &key, "overview", json!({"project": 7})).await;
    assert_eq!(err, "project is required");
    // A read refusal is not audited.
    assert!(audit_lines(&fx.dir).is_empty());
}

#[tokio::test]
async fn a_key_granted_on_a_cannot_reach_b_by_read_or_write() {
    let fx = durable("cross-tenant");
    let other = fx.second_project();
    let key = server_key(&fx.state, &fx.project, "creator", None, GrantLevel::Member);

    // A read tool naming B.
    let err = call_err(&fx.app, &key, "overview", json!({"project": other})).await;
    assert_eq!(err, format!("this key has no grant on {other}"));

    // A write tool naming B: same text, and the refusal is audited.
    let err = call_err(
        &fx.app,
        &key,
        "send",
        json!({
            "project": other,
            "kind": "need",
            "to": address("creator"),
            "subject": "s",
            "body": "b",
            "done_when": "y",
        }),
    )
    .await;
    assert_eq!(err, format!("this key has no grant on {other}"));
    let lines = audit_lines(&fx.dir);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["tool"], "send");
    assert_eq!(lines[0]["outcome"], "refused");
    assert_eq!(lines[0]["principal"], format!("creator@{}", fx.project));
    assert!(lines[0]["key_id"].as_str().unwrap().starts_with("key_"));
    assert_eq!(lines[0]["project_id"], Value::Null, "{lines:?}");

    // An unknown project shares the text, so a key cannot probe.
    let err = call_err(&fx.app, &key, "overview", json!({"project": "nope"})).await;
    assert_eq!(err, "this key has no grant on nope");
    // A grant whose member is no longer registered is refused too.
    let ghost = server_key(&fx.state, &fx.project, "ghost", None, GrantLevel::Member);
    let err = call_err(&fx.app, &ghost, "overview", json!({"project": fx.project})).await;
    assert!(
        err.contains("'ghost' is no longer a registered identity"),
        "{err}"
    );
}

#[tokio::test]
async fn an_operator_tool_refuses_a_member_acting_grant_per_call() {
    let fx = hosted_with("operator-rule", |_| {}, &["app"]);
    let (project, member) = fx.served[0].clone();
    // The grant's stored level is member and the acting role (mind) is not
    // operator kind: today's rule is re-derived per call and refuses.
    let key = server_key(
        &fx.state,
        &project,
        &member,
        Some("mind"),
        GrantLevel::Member,
    );
    let err = call_err(
        &fx.app,
        &key,
        "pump_control",
        json!({"project": project, "action": "pause"}),
    )
    .await;
    assert!(
        err.contains("needs a key acting as a role of kind operator"),
        "{err}"
    );
    let lines = audit_lines(&fx.dir);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["tool"], "pump_control");
    assert_eq!(lines[0]["outcome"], "refused");
    assert_eq!(lines[0]["project_id"], project);
}

// -------------------------------------------------------------- the list

#[tokio::test]
async fn tools_list_requires_project_and_names_the_projects_tool() {
    let fx = durable("tools-list");
    let key = server_key(&fx.state, &fx.project, "creator", None, GrantLevel::Member);
    let (_, listed) = rpc(&fx.app, &key, "tools/list", json!({})).await;
    let tools = listed["result"]["tools"].as_array().unwrap().clone();
    assert!(tools.len() > 10, "{tools:?}");
    let bare = [
        "projects",
        "create_project",
        "lane_create",
        "key_mint",
        "key_list",
        "key_revoke",
        "grant_add",
        "grant_remove",
    ];
    for tool in &tools {
        let name = tool["name"].as_str().unwrap();
        let schema = &tool["inputSchema"];
        assert_eq!(schema["additionalProperties"], false, "{name}");
        if bare.contains(&name) {
            assert!(
                schema["properties"].get("project").is_none(),
                "{name} takes no project"
            );
            continue;
        }
        assert!(
            schema["properties"]["project"].is_object(),
            "{name} declares project"
        );
        let required = schema["required"].as_array().unwrap();
        assert!(
            required.contains(&json!("project")),
            "{name} requires project"
        );
    }
    for name in bare {
        assert!(tools.iter().any(|t| t["name"] == name), "missing {name}");
    }
    for name in ["pump_action", "pump_reset"] {
        assert!(tools.iter().any(|t| t["name"] == name), "missing {name}");
    }
    let (_, init) = rpc(
        &fx.app,
        &key,
        "initialize",
        json!({"protocolVersion": "2025-03-26"}),
    )
    .await;
    let instructions = init["result"]["instructions"].as_str().unwrap();
    assert!(instructions.contains("`projects`"), "{instructions}");
}

// ------------------------------------------------------------ resolution

#[tokio::test]
async fn projects_lists_live_grants_with_names_and_the_host_answer() {
    let (app, state, dir, durable_project, (hosted_project, hosted_member)) =
        mixed("projects-tool");
    let key = server_key(
        &state,
        &durable_project,
        "creator",
        None,
        GrantLevel::Member,
    );
    // Grant the same principal the hosted project too.
    state
        .server_keys
        .mint(
            &format!("creator@{durable_project}"),
            false,
            vec![GrantRecord {
                project_id: hosted_project.clone(),
                member: hosted_member.clone(),
                identity: Some("operator".to_owned()),
                level: GrantLevel::Operator,
            }],
            "test",
            "creator",
        )
        .unwrap();

    let answer = call_ok(&app, &key, "projects", json!({})).await;
    let items = answer.as_array().unwrap();
    assert_eq!(items.len(), 2, "{items:?}");
    let durable_entry = items
        .iter()
        .find(|item| item["project_id"] == durable_project.as_str())
        .unwrap();
    assert_eq!(durable_entry["mode"], "durable");
    assert_eq!(durable_entry["member"], "creator");
    assert_eq!(durable_entry["identity"], Value::Null);
    assert_eq!(durable_entry["level"], "member");
    assert!(durable_entry.get("name").is_none(), "{durable_entry:?}");
    let hosted_entry = items
        .iter()
        .find(|item| item["project_id"] == hosted_project.as_str())
        .unwrap();
    assert_eq!(hosted_entry["mode"], "hosted");
    assert_eq!(hosted_entry["name"], "app");
    assert_eq!(hosted_entry["member"], hosted_member);
    assert_eq!(hosted_entry["identity"], "operator");
    assert_eq!(hosted_entry["level"], "operator");

    // A grant whose project no longer exists is skipped.
    state
        .server_keys
        .mint(
            "creator@deleted",
            false,
            vec![GrantRecord {
                project_id: "zzzzzzzzzzzzzzzzzzzzzzzzz".to_owned(),
                member: "creator".to_owned(),
                identity: None,
                level: GrantLevel::Member,
            }],
            "test",
            "creator",
        )
        .unwrap();
    let answer = call_ok(&app, &key, "projects", json!({})).await;
    assert_eq!(answer.as_array().unwrap().len(), 2);

    // The built-in host principal answers {host: true}.
    let host = hosted_with(
        "projects-host",
        |state: &mut AppState| {
            state.host_operator_token = Some("host-tok".into());
        },
        &["app"],
    );
    let answer = call_ok(&host.app, "host-tok", "projects", json!({})).await;
    assert_eq!(answer, json!({ "host": true }));
    let _ = dir;
}

#[tokio::test]
async fn a_name_resolves_among_grants_only_and_two_names_error() {
    let fx = hosted_with("two-names", |_| {}, &["app", "app"]);
    let (one, member) = fx.served[0].clone();
    let (two, member_two) = fx.served[1].clone();
    assert_ne!(one, two);
    let key = server_key(
        &fx.state,
        &one,
        &member,
        Some("operator"),
        GrantLevel::Operator,
    );
    // Grant the same principal the second project: one name, two grants.
    fx.state
        .server_keys
        .mint(
            &format!("{member}@{one}"),
            false,
            vec![GrantRecord {
                project_id: two.clone(),
                member: member_two.clone(),
                identity: Some("operator".to_owned()),
                level: GrantLevel::Operator,
            }],
            "test",
            &member,
        )
        .unwrap();

    // One name, two grants: the error names both project ids.
    let err = call_err(&fx.app, &key, "overview", json!({"project": "app"})).await;
    assert!(err.contains("two grants"), "{err}");
    assert!(err.contains(&one), "{err}");
    assert!(err.contains(&two), "{err}");

    // The exact id still reaches its own project.
    let overview = call_ok(&fx.app, &key, "overview", json!({"project": one})).await;
    assert_eq!(overview["project_id"], one);

    // With one grant, the name resolves.
    let single = server_key(
        &fx.state,
        &two,
        &member_two,
        Some("operator"),
        GrantLevel::Operator,
    );
    let overview = call_ok(&fx.app, &single, "overview", json!({"project": "app"})).await;
    assert_eq!(overview["project_id"], two);
}

// ---------------------------------------------------------------- audit

#[tokio::test]
async fn one_audit_line_per_write_call_and_none_for_reads() {
    let fx = durable("audit");
    let key = server_key(&fx.state, &fx.project, "creator", None, GrantLevel::Member);
    let send = |project: Option<String>| {
        let mut args = json!({
            "kind": "need",
            "to": address("creator"),
            "subject": "s",
            "body": "b",
            "done_when": "y",
        });
        if let Some(project) = project {
            args["project"] = json!(project);
        }
        args
    };

    let sent = call_ok(&fx.app, &key, "send", send(Some(fx.project.clone()))).await;
    assert!(sent["handle"].is_string(), "{sent}");
    let lines = audit_lines(&fx.dir);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["tool"], "send");
    assert_eq!(lines[0]["outcome"], "ok");
    assert_eq!(lines[0]["project_id"], fx.project);
    assert_eq!(lines[0]["principal"], format!("creator@{}", fx.project));
    assert!(
        lines[0]["summary"].as_str().unwrap().contains("send"),
        "{lines:?}"
    );

    // Reads add nothing: memo list and show are reads too.
    call_ok(&fx.app, &key, "overview", json!({"project": fx.project})).await;
    call_ok(&fx.app, &key, "memo", json!({"project": fx.project})).await;
    assert_eq!(audit_lines(&fx.dir).len(), 1);

    // A refused write is still audited.
    call_err(&fx.app, &key, "send", send(None)).await;
    let lines = audit_lines(&fx.dir);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(lines[1]["outcome"], "refused");
    assert_eq!(lines[1]["tool"], "send");

    call_ok(
        &fx.app,
        &key,
        "memo",
        json!({"project": fx.project, "action": "save", "subject": "s", "body": "b"}),
    )
    .await;
    let lines = audit_lines(&fx.dir);
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert_eq!(lines[2]["tool"], "memo");
    assert_eq!(lines[2]["outcome"], "ok");
}

#[tokio::test]
async fn a_hosted_write_is_audited_with_its_project() {
    let fx = hosted_with("audit-hosted", |_| {}, &["app"]);
    let (project, member) = fx.served[0].clone();
    let key = server_key(
        &fx.state,
        &project,
        &member,
        Some("operator"),
        GrantLevel::Operator,
    );
    let overview = call_ok(&fx.app, &key, "overview", json!({"project": project})).await;
    assert_eq!(overview["project_id"], project);
    assert!(audit_lines(&fx.dir).is_empty(), "a read is not audited");
    call_ok(
        &fx.app,
        &key,
        "memo",
        json!({"project": project, "action": "save", "subject": "s", "body": "b"}),
    )
    .await;
    let lines = audit_lines(&fx.dir);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["project_id"], project);
}

// ----------------------------------------------------------- host tools

#[tokio::test]
async fn host_tools_refuse_member_keys_and_work_for_the_host_token() {
    let fx = durable_with("host-tools", |state| {
        state.host_operator_token = Some("host-tok".into());
    });
    let member_key = server_key(&fx.state, &fx.project, "creator", None, GrantLevel::Member);

    // A member key is not a host principal.
    for tool in [
        "key_mint",
        "key_list",
        "key_revoke",
        "grant_add",
        "grant_remove",
    ] {
        let err = call_err(&fx.app, &member_key, tool, json!({})).await;
        assert!(
            err.contains("is a host tool; this key is not a host principal"),
            "{tool}: {err}"
        );
    }
    let err = call_err(&fx.app, &member_key, "create_project", json!({"name": "x"})).await;
    assert!(err.contains("is a host tool"), "{err}");

    // The refusal of a write host tool is audited; key_list (a read) is not.
    let lines = audit_lines(&fx.dir);
    assert_eq!(lines.len(), 5, "{lines:?}");
    assert!(
        lines.iter().all(|line| line["outcome"] == "refused"),
        "{lines:?}"
    );

    // Without the token configured, no bearer becomes the host: the
    // endpoint still authenticates keys, and host tools stay refused.
    let bare = durable_with("host-tools-no-token", |_| {});
    let other = server_key(
        &bare.state,
        &bare.project,
        "creator",
        None,
        GrantLevel::Member,
    );
    let err = call_err(&bare.app, &other, "key_list", json!({})).await;
    assert!(err.contains("host tool"), "{err}");
    let (status, value) = rpc(&bare.app, "host-tok", "ping", json!({})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{value}");

    // The host operator token reaches the host tools.
    let answer = call_ok(&fx.app, "host-tok", "key_list", json!({})).await;
    assert!(answer["principals"].is_array(), "{answer}");
}

#[tokio::test]
async fn key_admin_round_trip_mints_lists_revokes_and_audits() {
    let fx = durable_with("key-admin", |state| {
        state.host_operator_token = Some("host-tok".into());
    });
    let other = fx.second_project();
    let minted = call_ok(
        &fx.app,
        "host-tok",
        "key_mint",
        json!({
            "principal": format!("hand@{}", fx.project),
            "grants": [{
                "project_id": fx.project,
                "member": "creator",
                "level": "member",
            }],
            "label": "cli",
        }),
    )
    .await;
    let key = minted["key"].as_str().unwrap().to_owned();
    let key_id = minted["key_id"].as_str().unwrap().to_owned();
    assert!(key.starts_with("vivi_"), "{key}");
    assert_eq!(minted["grants"][0]["project_id"], fx.project, "{minted}");

    // The minted key reaches its granted project on this endpoint.
    let overview = call_ok(&fx.app, &key, "overview", json!({"project": fx.project})).await;
    assert_eq!(overview["you"]["name"], "creator");
    let mine = call_ok(&fx.app, &key, "projects", json!({})).await;
    assert_eq!(mine.as_array().unwrap().len(), 1);

    // List shows the principal and its key, digest only.
    let listed = call_ok(&fx.app, "host-tok", "key_list", json!({})).await;
    let principal = listed["principals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["principal"] == format!("hand@{}", fx.project))
        .unwrap_or_else(|| panic!("{listed}"));
    assert_eq!(principal["keys"][0]["key_id"], key_id);
    assert!(principal["keys"][0].get("key").is_none(), "{listed}");

    // Grants move through the admin tools; a host principal accepts none.
    call_ok(
        &fx.app,
        "host-tok",
        "grant_add",
        json!({
            "principal": format!("hand@{}", fx.project),
            "grant": {
                "project_id": other,
                "member": "creator",
                "identity": "operator",
                "level": "operator",
            },
        }),
    )
    .await;
    let err = call_err(
        &fx.app,
        "host-tok",
        "grant_remove",
        json!({"principal": "nobody@nowhere", "project_id": fx.project}),
    )
    .await;
    assert!(err.contains("holds no grant on"), "{err}");
    call_ok(
        &fx.app,
        "host-tok",
        "grant_remove",
        json!({"principal": format!("hand@{}", fx.project), "project_id": other}),
    )
    .await;
    let err = call_err(
        &fx.app,
        "host-tok",
        "key_mint",
        json!({"principal": "host", "host": true, "grants": [{"project_id": "p", "member": "m"}]}),
    )
    .await;
    assert!(
        err.contains("host principal carries no project grants"),
        "{err}"
    );

    // Revocation kills the key on this endpoint.
    call_ok(&fx.app, "host-tok", "key_revoke", json!({"key_id": key_id})).await;
    let (status, value) = rpc(&fx.app, &key, "ping", json!({})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{value}");

    // Audit: one line per admin write (ok and refused), none for the list.
    let lines = audit_lines(&fx.dir);
    let recorded: Vec<(String, String)> = lines
        .iter()
        .map(|line| {
            (
                line["tool"].as_str().unwrap().to_owned(),
                line["outcome"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        recorded,
        vec![
            ("key_mint".to_owned(), "ok".to_owned()),
            ("grant_add".to_owned(), "ok".to_owned()),
            ("grant_remove".to_owned(), "refused".to_owned()),
            ("grant_remove".to_owned(), "ok".to_owned()),
            ("key_mint".to_owned(), "refused".to_owned()),
            ("key_revoke".to_owned(), "ok".to_owned()),
        ],
        "{lines:?}"
    );
    assert!(
        lines.iter().all(|line| line["principal"] == "host"),
        "{lines:?}"
    );
}

// ----------------------------------------------------------------- boot

#[test]
fn boot_retires_legacy_project_key_files_once() {
    let dir = scratch("boot-retire");
    for (project, count) in [("p1", 2), ("p2", 1)] {
        let project = dir.join("projects").join(project);
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("api_keys.json"), legacy_keys_file(count)).unwrap();
    }
    let state = app_state(dir.clone(), None, None);
    for project in ["p1", "p2"] {
        let project_dir = dir.join("projects").join(project);
        assert!(!project_dir.join("api_keys.json").exists(), "{project}");
        assert!(
            project_dir.join("api_keys.json.retired").exists(),
            "{project}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(dir.join("projects/p1/api_keys.json.retired")).unwrap(),
        legacy_keys_file(2),
        "the rename moves the exact bytes"
    );
    // The store is usable after the retirement.
    state
        .server_keys
        .mint("creator@p1", false, vec![], "ops", "creator")
        .unwrap();

    // A second boot changes nothing (the .retired name wins).
    let _again = app_state(dir.clone(), None, None);
    assert!(dir.join("projects/p1/api_keys.json.retired").exists());
    assert!(
        !dir.join("projects/p1/api_keys.json").exists(),
        "no live file reappeared"
    );

    // A later mint re-creates the live file. Rooms auth and the letter wire
    // read it; project MCP does not.
    state
        .keys
        .mint("p1", "creator", "after retirement", None)
        .unwrap();
    assert!(dir.join("projects/p1/api_keys.json").exists());
    assert!(dir.join("projects/p1/api_keys.json.retired").exists());

    // A project that appears after the sweep keeps its mirror across a boot.
    // Rooms auth and the letter wire read that file.
    std::fs::create_dir_all(dir.join("projects/p3")).unwrap();
    state
        .keys
        .mint("p3", "creator", "after the sweep", None)
        .unwrap();
    let _third = app_state(dir.clone(), None, None);
    assert!(
        dir.join("projects/p3/api_keys.json").is_file(),
        "a mirror written after the sweep survives the next boot"
    );
    assert!(!dir.join("projects/p3/api_keys.json.retired").exists());
}

// ---------------------------------------------------------- self-service

#[tokio::test]
async fn the_self_service_mint_yields_a_working_server_key() {
    let fx = durable("self-service");
    let (status, minted) = post_json(
        &fx.app,
        &format!("/v1/projects/{}/keys", fx.project),
        Some(json!({"label": "bot"})),
        Some(&fx.token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{minted}");
    let key = minted["key"].as_str().unwrap().to_owned();
    let key_id = minted["key_id"].as_str().unwrap().to_owned();
    assert!(key.starts_with("vivi_"), "{key}");
    assert!(minted.get("identity").is_none(), "{minted}");
    assert_eq!(
        minted["grants"],
        json!([{
            "project_id": fx.project,
            "member": "creator",
            "level": "member",
        }]),
        "{minted}"
    );

    // The key works on /v1/mcp with its grant.
    let overview = call_ok(&fx.app, &key, "overview", json!({"project": fx.project})).await;
    assert_eq!(overview["you"]["name"], "creator");
    // The per-project route is gone. The mirror file is still written:
    // rooms auth and the letter wire read it.
    let (status, old) = old_route_call(&fx.app, &key, "overview", &fx.project).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{old}");
    assert!(
        fx.dir
            .join("projects")
            .join(&fx.project)
            .join("api_keys.json")
            .is_file(),
        "self-service mint still mirrors the secret"
    );

    // Revoking through the per-project route kills the server twin too.
    let (status, _) = delete(
        &fx.app,
        &format!("/v1/projects/{}/keys/{key_id}", fx.project),
        &fx.token,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, value) = rpc(&fx.app, &key, "ping", json!({})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{value}");
}

#[tokio::test]
async fn a_self_service_mint_never_shares_a_derived_principal() {
    // Order one: the plain derivation holds the principal first.
    let fx = durable("collision-plain-first");
    let ops_token = fx
        .state
        .identities
        .register(
            &fx.project,
            "creator.ops",
            &spki_pem(&key(3)),
            &mailspace_for("creator.ops"),
        )
        .unwrap()
        .token;
    let (status, plain) = post_json(
        &fx.app,
        &format!("/v1/projects/{}/keys", fx.project),
        Some(json!({"label": "plain"})),
        Some(&ops_token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{plain}");
    // `creator` minting with identity `ops` derives the same principal
    // name as the member literally called `creator.ops`: refused, with
    // both derivations named.
    let (status, err) = post_json(
        &fx.app,
        &format!("/v1/projects/{}/keys", fx.project),
        Some(json!({"label": "collide", "identity": "ops"})),
        Some(&fx.token),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{err}");
    let message = err["error"]["message"].as_str().unwrap();
    assert!(
        message.contains(&format!("creator.ops@{}", fx.project)),
        "{message}"
    );
    assert!(
        message.contains("member 'creator.ops' with no acting identity"),
        "{message}"
    );
    assert!(
        message.contains("member 'creator' acting as 'ops'"),
        "{message}"
    );

    // Order two: the identity derivation holds it first.
    let fx = durable("collision-identity-first");
    let (status, named) = post_json(
        &fx.app,
        &format!("/v1/projects/{}/keys", fx.project),
        Some(json!({"label": "named", "identity": "ops"})),
        Some(&fx.token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{named}");
    let ops_token = fx
        .state
        .identities
        .register(
            &fx.project,
            "creator.ops",
            &spki_pem(&key(3)),
            &mailspace_for("creator.ops"),
        )
        .unwrap()
        .token;
    let (status, err) = post_json(
        &fx.app,
        &format!("/v1/projects/{}/keys", fx.project),
        Some(json!({"label": "plain"})),
        Some(&ops_token),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{err}");
    let message = err["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("member 'creator' acting as 'ops'"),
        "{message}"
    );
    assert!(
        message.contains("member 'creator.ops' with no acting identity"),
        "{message}"
    );

    // The same derivation may re-mint (the existing re-mint semantics).
    let (status, again) = post_json(
        &fx.app,
        &format!("/v1/projects/{}/keys", fx.project),
        Some(json!({"label": "again", "identity": "ops"})),
        Some(&fx.token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{again}");
}

#[tokio::test]
async fn a_host_key_revoke_kills_the_per_project_mirror() {
    let fx = durable_with("mirror-revoke", |state| {
        state.host_operator_token = Some("host-tok".into());
    });
    let (status, minted) = post_json(
        &fx.app,
        &format!("/v1/projects/{}/keys", fx.project),
        Some(json!({"label": "bot"})),
        Some(&fx.token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{minted}");
    let key = minted["key"].as_str().unwrap().to_owned();
    let key_id = minted["key_id"].as_str().unwrap().to_owned();

    // The mirror is live: rooms auth accepts the same secret.
    let (status, value) = rooms_call(&fx.app, &key, "overview", &fx.project).await;
    assert_eq!(status, StatusCode::OK, "{value}");

    // Host revoke on /v1/mcp.
    let answer = call_ok(&fx.app, "host-tok", "key_revoke", json!({"key_id": key_id})).await;
    assert_eq!(answer["revoked"], key_id.as_str());

    // Dead on /v1/mcp AND on the rooms route: the mirror entry is revoked.
    let (status, value) = rpc(&fx.app, &key, "ping", json!({})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{value}");
    let (status, value) = rooms_call(&fx.app, &key, "overview", &fx.project).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{value}");
    let mirror: Value = serde_json::from_str(
        &std::fs::read_to_string(
            fx.dir
                .join("projects")
                .join(&fx.project)
                .join("api_keys.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let entry = mirror["keys"]
        .as_array()
        .unwrap()
        .iter()
        .find(|key| key["key_id"] == key_id.as_str())
        .unwrap_or_else(|| panic!("{mirror}"));
    assert!(entry["revoked_at"].as_str().is_some(), "{mirror}");
}

#[tokio::test]
async fn a_host_grant_remove_revokes_that_projects_mirror_only() {
    let fx = durable_with("mirror-grant-remove", |state| {
        state.host_operator_token = Some("host-tok".into());
    });
    let other = fx.second_project();
    // `bob` is a registered member of the second project.
    let bob_token = fx
        .state
        .identities
        .register(&other, "bob", &spki_pem(&key(2)), &mailspace_for("bob"))
        .unwrap()
        .token;
    let (status, mine) = post_json(
        &fx.app,
        &format!("/v1/projects/{}/keys", fx.project),
        Some(json!({"label": "mine"})),
        Some(&fx.token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{mine}");
    let key_a = mine["key"].as_str().unwrap().to_owned();
    let (status, bobs) = post_json(
        &fx.app,
        &format!("/v1/projects/{other}/keys"),
        Some(json!({"label": "bobs"})),
        Some(&bob_token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{bobs}");
    let key_b = bobs["key"].as_str().unwrap().to_owned();

    // The host drops creator's grant on the first project.
    call_ok(
        &fx.app,
        "host-tok",
        "grant_remove",
        json!({"principal": format!("creator@{}", fx.project), "project_id": fx.project}),
    )
    .await;

    // The key is ungranted on /v1/mcp ...
    let err = call_err(&fx.app, &key_a, "overview", json!({"project": fx.project})).await;
    assert_eq!(err, format!("this key has no grant on {}", fx.project));
    // ... and its mirror in that project died with the grant.
    let (status, value) = rooms_call(&fx.app, &key_a, "overview", &fx.project).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{value}");
    // The other project's mirror is untouched.
    let (status, value) = rooms_call(&fx.app, &key_b, "overview", &other).await;
    assert_eq!(status, StatusCode::OK, "{value}");
}

#[tokio::test]
async fn self_service_mint_and_revoke_each_write_one_audit_line() {
    let fx = durable("self-service-audit");
    let (status, minted) = post_json(
        &fx.app,
        &format!("/v1/projects/{}/keys", fx.project),
        Some(json!({"label": "bot"})),
        Some(&fx.token),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{minted}");
    let key_id = minted["key_id"].as_str().unwrap().to_owned();

    let lines = audit_lines(&fx.dir);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["tool"], "self_service_mint");
    assert_eq!(lines[0]["outcome"], "ok");
    assert_eq!(lines[0]["key_id"], key_id.as_str());
    assert_eq!(lines[0]["principal"], format!("creator@{}", fx.project));
    assert_eq!(lines[0]["project_id"], fx.project);
    assert!(
        lines[0]["summary"].as_str().unwrap().contains(&key_id),
        "{lines:?}"
    );

    let (status, _) = delete(
        &fx.app,
        &format!("/v1/projects/{}/keys/{key_id}", fx.project),
        &fx.token,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let lines = audit_lines(&fx.dir);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(lines[1]["tool"], "self_service_revoke");
    assert_eq!(lines[1]["outcome"], "ok");
    assert_eq!(lines[1]["key_id"], key_id.as_str());
    assert_eq!(lines[1]["principal"], format!("creator@{}", fx.project));
    assert_eq!(lines[1]["project_id"], fx.project);
    assert!(
        lines[1]["summary"].as_str().unwrap().contains(&key_id),
        "{lines:?}"
    );
}

#[tokio::test]
async fn an_identity_token_is_refused_with_a_server_key_hint() {
    let fx = durable("identity-token");
    let (status, value) = rpc(&fx.app, &fx.token, "ping", json!({})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{value}");
    let message = value["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("identity tokens are not accepted on /v1/mcp"),
        "{message}"
    );
    assert!(message.contains("server key"), "{message}");
    // An unknown vivi_-shaped secret is the generic invalid bearer.
    let (status, value) = rpc(&fx.app, "vivi_nope", "ping", json!({})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{value}");
    assert_eq!(value["error"]["message"], "invalid bearer token");
}

// --------------------------------------------------------------- helpers

/// POST JSON to a v1 HTTP route (the self-service key route).
async fn post_json(
    app: &Router,
    uri: &str,
    body: Option<Value>,
    token: Option<&str>,
) -> (StatusCode, Value) {
    let bytes = body
        .as_ref()
        .map(|v| v.to_string().into_bytes())
        .unwrap_or_default();
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(bytes)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

async fn delete(app: &Router, uri: &str, token: &str) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(uri)
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

/// One tool call through the removed per-project MCP route.
async fn old_route_call(app: &Router, key: &str, name: &str, project: &str) -> (StatusCode, Value) {
    let body = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": name, "arguments": {}}
    });
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/v1/projects/{project}/mcp"))
                .header("authorization", format!("Bearer {key}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

/// One tool call through the rooms MCP route, which authenticates the
/// per-project mirror (`api_keys.json`), not the server store.
async fn rooms_call(app: &Router, key: &str, name: &str, project: &str) -> (StatusCode, Value) {
    let body = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": name, "arguments": {}}
    });
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/v1/rooms/{project}/mcp"))
                .header("authorization", format!("Bearer {key}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}
