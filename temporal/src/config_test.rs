use super::*;

const SANDBOX: &str = include_str!("../deploy/sandbox.toml");

#[test]
fn sandbox_config_parses() {
    let c = Config::parse(SANDBOX).unwrap();
    assert_eq!(c.server.namespace, "default");
    assert_eq!(
        c.queues_for("studio").unwrap(),
        vec![
            ("agent-studio".to_string(), 2),
            ("check-studio".to_string(), 2),
            ("gate-studio".to_string(), 1),
        ]
    );
    assert_eq!(
        c.queues_for("pepper").unwrap(),
        vec![("control".to_string(), 50), ("lane".to_string(), 2)]
    );
    let t = c.target("pump-sandbox").unwrap();
    assert_eq!(t.main, "temporal/main");
    assert_eq!(t.branch_prefix, "temporal/");
    assert_eq!(c.record.kind, RecordKind::Log);
}

#[test]
fn routing_and_lander_input_convert_seconds() {
    let c = Config::parse(SANDBOX).unwrap();
    let r = c.routing();
    assert_eq!(r.affinity_wait_ms, 60_000);
    assert_eq!(r.agent, "agent-{host}");
    let l = c.lander_input("pump-sandbox").unwrap();
    assert_eq!(l.admission.max_backlog_ms, 3_600_000);
    assert_eq!(l.admission.land_ms_per_unit, 180_000);
    assert_eq!(l.gate_cap_ms, 1_200_000);
    assert!(c.lander_input("radix").is_err());
    assert_eq!(lander_id("pump-sandbox"), "lander-pump-sandbox");
}

fn with(replace: &str, by: &str) -> String {
    assert!(SANDBOX.contains(replace), "{replace}");
    SANDBOX.replace(replace, by)
}

#[test]
fn routing_host_without_hosts_entry_is_refused() {
    let text = with("gate_hosts = [\"studio\"]", "gate_hosts = [\"studio-02\"]");
    let err = Config::parse(&text).unwrap_err().to_string();
    assert!(err.contains("studio-02"), "{err}");
}

#[test]
fn zero_slots_and_missing_control_are_refused() {
    let text = with("gate-studio = 1", "gate-studio = 0");
    assert!(
        Config::parse(&text)
            .unwrap_err()
            .to_string()
            .contains("0 slots")
    );
    let text = with(
        "queues = { control = 50, lane = 2 }",
        "queues = { lane = 2 }",
    );
    assert!(
        Config::parse(&text)
            .unwrap_err()
            .to_string()
            .contains("control")
    );
}

#[test]
fn unknown_keys_are_refused() {
    let text = with(
        "[record]\nkind = \"log\"",
        "[record]\nkind = \"log\"\nbogus = 1",
    );
    assert!(Config::parse(&text).is_err());
}

#[test]
fn mcp_record_needs_server_and_project() {
    let text = with("kind = \"log\"", "kind = \"mcp\"");
    assert!(Config::parse(&text).is_err());
    let text = with(
        "kind = \"log\"",
        "kind = \"mcp\"\nserver = \"http://hub.local:18190\"\nproject = \"faberlang\"",
    );
    assert_eq!(Config::parse(&text).unwrap().record.kind, RecordKind::Mcp);
}

#[test]
fn tilde_expands_to_home() {
    let home = std::env::var("HOME").unwrap();
    assert_eq!(expand("~/x/y"), PathBuf::from(home).join("x/y"));
    assert_eq!(expand("/abs"), PathBuf::from("/abs"));
}
