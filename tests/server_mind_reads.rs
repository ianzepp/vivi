//! The enumerated operational reads (`pump_log`, `flow_show`, `config_show`,
//! `config_validate`, `pool_list`, `pool_show`, `role_list`, `role_show`):
//! the host's own member's Mind-kind seats may take them, foreign members
//! and other kinds are refused, the operator keeps access, and a read files
//! no action mail and changes no configuration.

use std::collections::BTreeSet;
use std::path::PathBuf;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::util::ServiceExt;
use vivi::mailspace::{Mailspace, RoleUpdate};
use vivi::server::grants::{GrantLevel, GrantRecord};
use vivi::server::{AppState, app_state, hosted, router};

/// The eight reads the operational rule now covers, with minimal arguments.
fn reads() -> [(&'static str, Value); 8] {
    [
        ("pump_log", json!({"file": "log"})),
        ("flow_show", json!({"kind": "build"})),
        ("config_show", json!({})),
        ("config_validate", json!({})),
        ("pool_list", json!({})),
        ("pool_show", json!({"name": "main"})),
        ("role_list", json!({})),
        ("role_show", json!({"role": "hand"})),
    ]
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "vivi-server-mind-reads-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct Fx {
    app: Router,
    state: AppState,
    root: PathBuf,
    project: String,
    /// The host's own member, which the operator's key authenticates as.
    member: String,
}

/// A served host root with one pool, a `[pump]` table (so the pump home and
/// its log exist), a seeded pump log, and `operator` and `hand` roles.
fn fixture(name: &str) -> Fx {
    let base = scratch(name);
    let root = base.join("hosts").join("app");
    let mut ms = Mailspace::init(Some(&root)).unwrap();
    ms.ensure_operator_role().unwrap();
    ms.add_role(
        "hand",
        RoleUpdate {
            kind: Some(Some("hand".into())),
            ..RoleUpdate::default()
        },
    )
    .unwrap();
    let toml = root.join(".vivi").join("mailspace.toml");
    let mut text = std::fs::read_to_string(&toml).unwrap();
    text.push_str(
        "\n[pools.main]\nprovider = \"local\"\nslots = 2\n\
         [pools.main.harness]\nname = \"pi\"\nrun = \"pi --model {model}\"\n\
         models = [\"zai/glm-5.3-flash\"]\n\
         \n[pump]\ntick = 20\n",
    );
    std::fs::write(&toml, text).unwrap();
    let pump_home = root.join(".vivi").join("pump");
    std::fs::create_dir_all(&pump_home).unwrap();
    std::fs::write(pump_home.join("log.txt"), "tick 1 ok\ntick 2 dispatched\n").unwrap();
    let state = app_state(base.join("data"), None, None).expect("app state");
    let report = hosted::reconcile(
        &state.projects,
        &state.identities,
        &BTreeSet::from([root.clone()]),
    );
    assert!(report.refused.is_empty(), "{report:?}");
    Fx {
        app: router(state.clone()),
        root,
        project: report.served[0].project_id.clone(),
        member: report.served[0].member.clone(),
        state,
    }
}

impl Fx {
    /// Add one role to the hosted mailspace with kind `kind`.
    fn add_role(&self, name: &str, kind: Option<&str>) {
        Mailspace::open_root(self.root.clone())
            .unwrap()
            .add_role(
                name,
                RoleUpdate {
                    kind: kind.map(str::to_owned).map(Some),
                    ..RoleUpdate::default()
                },
            )
            .unwrap();
    }

    /// The canonical name of the one role of kind `kind` (case-insensitive):
    /// the seat a Mind test acts as, found by kind and never by name.
    fn seat_of_kind(&self, kind: &str) -> String {
        let seats: Vec<String> = Mailspace::open_root(self.root.clone())
            .unwrap()
            .list_role_views()
            .unwrap()
            .into_iter()
            .filter(|role| {
                role.kind
                    .as_deref()
                    .is_some_and(|held| held.eq_ignore_ascii_case(kind))
            })
            .map(|role| role.name)
            .collect();
        assert_eq!(seats.len(), 1, "expected one role of kind {kind}");
        seats[0].clone()
    }

    /// Every action letter the mailspace holds (role `inbox`, kind `mail`).
    fn action_mail(&self) -> Vec<String> {
        Mailspace::open_root(self.root.clone())
            .unwrap()
            .list_kind_all(None, "inbox", "mail")
            .unwrap()
            .into_iter()
            .map(|message| message.subject)
            .collect()
    }

    /// The mailspace config's bytes, the only file a config change touches.
    fn config_bytes(&self) -> Vec<u8> {
        std::fs::read(self.root.join(".vivi").join("mailspace.toml")).unwrap()
    }

    fn key_by(&self, member: &str, identity: Option<&str>) -> String {
        server_key(&self.state, &self.project, member, identity)
    }

    async fn post(&self, token: &str, method: &str, mut params: Value) -> (StatusCode, Value) {
        if method == "tools/call" {
            let args = params
                .as_object_mut()
                .unwrap()
                .entry("arguments")
                .or_insert_with(|| json!({}));
            if let Some(args) = args.as_object_mut() {
                args.entry("project")
                    .or_insert_with(|| json!(self.project.clone()));
            }
        }
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        let response = self
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/mcp")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn rpc(&self, token: &str, method: &str, params: Value) -> Value {
        let (status, value) = self.post(token, method, params).await;
        assert_eq!(status, StatusCode::OK, "{value}");
        value
    }

    async fn call(&self, token: &str, tool: &str, arguments: Value) -> Value {
        let value = self
            .rpc(
                token,
                "tools/call",
                json!({"name": tool, "arguments": arguments}),
            )
            .await;
        let text = value["result"]["content"][0]["text"].as_str().unwrap();
        assert_ne!(value["result"]["isError"], true, "{tool}: {text}");
        serde_json::from_str(text).unwrap()
    }

    async fn refused(&self, token: &str, tool: &str, arguments: Value) -> String {
        let value = self
            .rpc(
                token,
                "tools/call",
                json!({"name": tool, "arguments": arguments}),
            )
            .await;
        assert_eq!(value["result"]["isError"], true, "{tool}: {value}");
        let text = value["result"]["content"][0]["text"].as_str().unwrap();
        serde_json::from_str::<Value>(text).unwrap()["error"]
            .as_str()
            .unwrap()
            .to_owned()
    }
}

fn server_key(state: &AppState, project: &str, member: &str, identity: Option<&str>) -> String {
    let level = match identity {
        None | Some("operator") => GrantLevel::Operator,
        Some(_) => GrantLevel::Member,
    };
    // Grants live on the principal, and the first grant for a project wins.
    // A separate principal per acting identity keeps each key's grant alone.
    let tag = identity.unwrap_or("default");
    state
        .server_keys
        .mint(
            &format!("{member}.{tag}@{project}"),
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

fn spki_pem(seed: u8) -> String {
    use base64::Engine;
    const PREFIX: &[u8] = &[
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
    ];
    let mut der = PREFIX.to_vec();
    der.extend_from_slice(
        ed25519_dalek::SigningKey::from_bytes(&[seed; 32])
            .verifying_key()
            .as_bytes(),
    );
    format!(
        "-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----",
        base64::engine::general_purpose::STANDARD.encode(der)
    )
}

#[tokio::test]
async fn a_mind_seat_may_take_every_operational_read() {
    let fx = fixture("mind-reads");
    fx.add_role("steward", Some("mind"));
    // The seat is found by kind, in either case, and its name is never
    // relied upon: the rule must read the role's kind, not its name.
    let seat = fx.seat_of_kind("MiNd");
    let key = fx.key_by(&fx.member, Some(&seat));

    for (tool, arguments) in reads() {
        fx.call(&key, tool, arguments.clone()).await;
    }
    // Each read answers its CLI `--json` body, not an error shell.
    let log = fx
        .call(&key, "pump_log", json!({"file": "log", "filter": "tick 2"}))
        .await;
    assert_eq!(log["file"], "log");
    assert_eq!(log["lines"].as_array().unwrap().len(), 1, "{log}");
    let roles = fx.call(&key, "role_list", json!({})).await;
    assert!(roles.as_array().unwrap().len() >= 2, "{roles}");
    let role = fx.call(&key, "role_show", json!({"role": "hand"})).await;
    assert_eq!(role["name"], "hand", "{role}");
    let pools = fx.call(&key, "pool_list", json!({})).await;
    assert_eq!(pools[0]["name"], "main", "{pools}");
    let pool = fx.call(&key, "pool_show", json!({"name": "main"})).await;
    assert_eq!(pool["name"], "main", "{pool}");
    let config = fx.call(&key, "config_show", json!({})).await;
    assert!(config.get("roles").is_some(), "{config}");
    let issues = fx.call(&key, "config_validate", json!({})).await;
    assert!(issues.is_array(), "{issues}");
    let flow = fx.call(&key, "flow_show", json!({"kind": "build"})).await;
    assert_eq!(flow["kind"], "build", "{flow}");
}

#[tokio::test]
async fn the_reads_file_no_mail_and_change_no_config() {
    let fx = fixture("no-writes");
    fx.add_role("steward", Some("mind"));
    let key = fx.key_by(&fx.member, Some("steward"));
    let before_config = fx.config_bytes();
    assert!(
        fx.action_mail().is_empty(),
        "a fresh mailspace holds no mail"
    );

    for (tool, arguments) in reads() {
        fx.call(&key, tool, arguments.clone()).await;
    }
    // Not one read filed an action letter or touched the mailspace config.
    assert!(fx.action_mail().is_empty(), "{:?}", fx.action_mail());
    assert_eq!(fx.config_bytes(), before_config, "the config changed");
}

#[tokio::test]
async fn foreign_member_and_hand_keys_are_refused_and_operator_keeps_access() {
    let fx = fixture("refusals");
    fx.add_role("steward", Some("mind"));
    fx.state
        .identities
        .register(
            &fx.project,
            "bob",
            &spki_pem(11),
            "0c0c0c0c-1111-4222-8333-444444444444",
        )
        .unwrap();
    let bob_mind = fx.key_by("bob", Some("steward"));
    let hand = fx.key_by(&fx.member, Some("hand"));
    let operator = fx.key_by(&fx.member, None);

    for (tool, arguments) in reads() {
        // A mind-kind seat only helps when the key's minting member is the
        // host's own; a key bob minted for itself stays shut out.
        let message = fx.refused(&bob_mind, tool, arguments.clone()).await;
        assert!(message.contains("host's own member"), "{tool}: {message}");
        // The host's own hand seat is the wrong kind for the reads.
        let message = fx.refused(&hand, tool, arguments.clone()).await;
        assert!(
            message.contains("kind mind or operator"),
            "{tool}: {message}"
        );
        assert!(message.contains("of kind hand"), "{tool}: {message}");
        // The operator's key keeps every read it had.
        fx.call(&operator, tool, arguments.clone()).await;
    }
}

#[tokio::test]
async fn the_operator_only_reads_stay_closed_and_resolved_stays_refused() {
    let fx = fixture("operator-only");
    fx.add_role("steward", Some("mind"));
    let seat = fx.seat_of_kind("mind");
    let key = fx.key_by(&fx.member, Some(&seat));

    // charter_show and target_list are not among the enumerated reads and
    // keep the operator-only rule.
    for (tool, arguments) in [
        ("charter_show", json!({"role": "hand"})),
        ("target_list", json!({})),
    ] {
        let message = fx.refused(&key, tool, arguments).await;
        assert!(message.contains("kind operator"), "{tool}: {message}");
    }
    // The no-live-free-slot restriction is preserved on the reads: the probe
    // runs the vivi binary's `pool status`, which the server is not.
    for (tool, arguments) in [
        ("config_show", json!({"resolved": true})),
        ("pool_show", json!({"name": "main", "resolved": true})),
    ] {
        let message = fx.refused(&key, tool, arguments).await;
        assert!(
            message.contains("cannot resolve live free slots"),
            "{tool}: {message}"
        );
    }
    // A bad `resolved` shape is refused before any authorization answers.
    let message = fx
        .refused(&key, "config_show", json!({"resolved": "yes"}))
        .await;
    assert!(
        message.contains("resolved must be true or false"),
        "{message}"
    );
}
