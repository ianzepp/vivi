use super::*;

#[test]
fn the_label_names_the_pool_and_the_job() {
    assert_eq!(label_for("wt", "abc123456789"), "pool-wt-abc123456789");
}

#[test]
fn an_over_long_pool_is_truncated_but_the_job_id_survives() {
    let label = label_for(&"p".repeat(200), "abc123456789");
    assert_eq!(label.chars().count(), 64);
    assert!(label.starts_with("pool-pppp"), "{label}");
    assert!(label.ends_with("-abc123456789"), "{label}");
}

#[test]
fn a_long_job_id_still_fits_the_cap() {
    let job = "f".repeat(40);
    let label = label_for("poolname", &job);
    assert!(label.chars().count() <= 64, "{label}");
    assert!(label.ends_with(&job), "{label}");
}

#[test]
fn the_mcp_config_parses_and_names_the_key_only_by_reference() {
    let key = format!("vivi_bridge_{}{}", "S", "k".repeat(40));
    let json = mcp_config_json("http://127.0.0.1:8080", "abcde-fghjk");
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    let server = &parsed["mcpServers"]["vivi"];
    assert_eq!(server["command"], "vivi");
    assert_eq!(server["args"], serde_json::json!(["mcp"]));
    assert_eq!(server["env"]["VIVI_MCP_SERVER"], "http://127.0.0.1:8080");
    assert_eq!(server["env"]["VIVI_MCP_PROJECT_ID"], "abcde-fghjk");
    assert_eq!(server["env"]["VIVI_MCP_API_KEY"], "${VIVI_MCP_API_KEY}");
    assert!(!json.contains(&key), "no 40-plus character key: {json}");
}

#[test]
fn the_bridge_brief_names_the_handle_and_the_four_tools() {
    let brief = bridge_brief("abcd1234efgh", "Fix the flux capacitor");
    assert!(brief.contains("abcd1234efgh"), "{brief}");
    assert!(brief.contains("Fix the flux capacitor"), "{brief}");
    for tool in ["`show`", "`thread`", "`reply`", "`done`"] {
        assert!(brief.contains(tool), "{tool} missing: {brief}");
    }
    assert!(brief.contains("on the server"), "{brief}");
}
