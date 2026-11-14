use super::*;

/// A config with secrets planted in every place a field could leak from.
pub(crate) const CONFIG: &str = r#"name = "demo"

[[identities]]
name = "mind"
kind = "mind"
pool = "main"
model = "model-a"
models = ["model-a", "model-b"]
cadence = "5m"
command = "run --api-key sekret-command"

[[identities]]
name = "hand"
kind = "hand"
status = "parked"

[judgment]
provider = "judge"
key_cmd = "pass show sekret-judgment"

[pump]
tick = 30

[pump.dispatch]
default_kind = "build"

[pump.dispatch.kinds.build]
pools = ["main"]
max = 3

[[pump.dispatch.lanes]]
model = "model-a"
cap = 2

[pools.main]
provider = "ssh"
slots = 4
fallback = "spare"
api_key = "sekret-unknown-field"
auth_token = "sekret-token"

[pools.main.ssh]
host = "studio-01"

[pools.main.caps]
model-a = 2
secret_model = 1

[pools.main.harness]
name = "claude"
run = "claude --token sekret-run"
models = ["model-a"]
env = ["SEKRET_ENV_NAME"]

[pools.spare]
provider = "local"
slots = 1

[[slot]]
name = "s1"
repository = "owner/sekret-repo"
host = "studio-01"
dir = "/x/s1"
pools = ["main"]
"#;

fn project(config: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".vivi")).unwrap();
    std::fs::write(dir.path().join(".vivi/mailspace.toml"), config).unwrap();
    dir
}

#[test]
fn roles_show_kind_pool_models_and_cadence() {
    let dir = project(CONFIG);
    let data = load(dir.path()).unwrap();
    let names: Vec<&str> = data.roles.iter().map(|role| role.name.as_str()).collect();
    assert_eq!(names, ["hand", "mind"]);
    let mind = &data.roles[1];
    assert_eq!(mind.kind.as_deref(), Some("mind"));
    assert_eq!(mind.pool.as_deref(), Some("main"));
    assert_eq!(mind.models, ["model-a", "model-b"], "no repeats");
    assert_eq!(mind.cadence.as_deref(), Some("5m"));
    assert_eq!(data.roles[0].status, "parked");
}

#[test]
fn pools_show_slots_caps_harness_routed_slots_and_roles() {
    let dir = project(CONFIG);
    let data = load(dir.path()).unwrap();
    let main = data.pools.iter().find(|pool| pool.name == "main").unwrap();
    // One declared slot routes to the pool, which sets the pool's size.
    assert_eq!((main.provider.as_str(), main.slots), ("ssh", 1));
    assert_eq!(main.fallback.as_deref(), Some("spare"));
    assert_eq!(main.caps[0], ("model-a".to_string(), 2));
    assert_eq!(
        main.harness,
        Some(("claude".to_string(), vec!["model-a".to_string()]))
    );
    assert_eq!(
        main.declared_slots,
        [DeclaredSlot {
            name: "s1".into(),
            host: "studio-01".into()
        }]
    );
    assert_eq!(main.roles, ["mind"]);
}

#[test]
fn the_pump_dispatch_kinds_and_lanes_are_shown() {
    let dir = project(CONFIG);
    let dispatch = load(dir.path()).unwrap().dispatch.unwrap();
    assert_eq!(dispatch.tick, 30);
    assert_eq!(dispatch.lanes, [("model-a".to_string(), 2)]);
    assert_eq!(dispatch.kinds[0].name, "build");
    assert_eq!(dispatch.kinds[0].max, Some(3));
}

#[test]
fn nothing_secret_survives_into_the_view() {
    let dir = project(CONFIG);
    let data = load(dir.path()).unwrap();
    let dump = format!("{data:?}");
    for planted in [
        "sekret",
        "SEKRET",
        "key_cmd",
        "api_key",
        "auth_token",
        "--token",
        "--api-key",
    ] {
        assert!(!dump.contains(planted), "{planted} leaked: {dump}");
    }
}

#[test]
fn names_that_look_secret_are_hidden() {
    for name in [
        "api_key",
        "key_cmd",
        "AUTH_TOKEN",
        "client_secret",
        "Password",
        "db_passwd",
    ] {
        assert!(is_secret_name(name), "{name}");
        assert_eq!(shown(name), HIDDEN);
    }
    for name in ["model-a", "provider", "slots", "cadence"] {
        assert!(!is_secret_name(name), "{name}");
        assert_eq!(shown(name), name);
    }
}

#[test]
fn a_secret_looking_cap_name_is_hidden_in_the_view() {
    let dir = project(CONFIG);
    let data = load(dir.path()).unwrap();
    let main = data.pools.iter().find(|pool| pool.name == "main").unwrap();
    let names: Vec<&str> = main.caps.iter().map(|(name, _)| name.as_str()).collect();
    assert!(names.contains(&HIDDEN), "{names:?}");
    assert!(!names.contains(&"secret_model"));
}

#[test]
fn a_project_without_pools_or_a_pump_has_empty_lists() {
    let dir = project("name = \"demo\"\n");
    let data = load(dir.path()).unwrap();
    assert!(data.roles.is_empty() && data.pools.is_empty());
    assert!(data.dispatch.is_none());
}
