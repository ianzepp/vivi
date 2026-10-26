//! Unit tests for [`super::is_write`]: every classification row the server
//! endpoint relies on for write auditing and rate limiting.

use serde_json::{Value, json};

use super::is_write;

/// `(tool, arguments, writes)` rows: unconditional writes, unconditional
/// reads, action-gated tools, `config_migrate`'s `write` argument, and
/// `pump_priority`.
fn cases() -> Vec<(&'static str, Value, bool)> {
    vec![
        // Unconditional writes, whatever the arguments carry.
        ("send", json!({}), true),
        ("reply", json!({}), true),
        ("done", json!({}), true),
        ("reopen", json!({}), true),
        ("mark", json!({}), true),
        ("unmark", json!({}), true),
        ("depend", json!({}), true),
        ("pump_control", json!({}), true),
        ("pump_send", json!({}), true),
        ("pump_action", json!({}), true),
        ("pump_reset", json!({}), true),
        ("config_set", json!({}), true),
        ("config_unset", json!({}), true),
        ("pool_allow_set", json!({}), true),
        ("target_add", json!({}), true),
        ("target_remove", json!({}), true),
        ("role_add", json!({}), true),
        ("role_set", json!({}), true),
        ("charter_set", json!({}), true),
        // Unconditional reads, including unknown tool names.
        ("list", json!({}), false),
        ("show", json!({}), false),
        ("projects", json!({}), false),
        ("pump_health", json!({}), false),
        ("not_a_tool", json!({}), false),
        // Action-gated tools write only on their write actions.
        ("memo", json!({"action": "save"}), true),
        ("memo", json!({"action": "list"}), false),
        ("memo", json!({}), false),
        ("pump_rungs", json!({"action": "set"}), true),
        ("pump_rungs", json!({"action": "show"}), false),
        ("pump_rungs", json!({}), false),
        ("pool_bands", json!({"action": "set"}), true),
        ("pool_bands", json!({"action": "unset"}), true),
        ("pool_bands", json!({"action": "show"}), false),
        ("pool_bands", json!({}), false),
        // `config_migrate` writes only when its `write` argument says so;
        // absent and null mean no, malformed input is treated as a write.
        ("config_migrate", json!({"write": false}), false),
        ("config_migrate", json!({"write": null}), false),
        ("config_migrate", json!({}), false),
        ("config_migrate", json!({"write": true}), true),
        ("config_migrate", json!({"write": "yes"}), true),
        ("config_migrate", json!({"write": 1}), true),
        // `pump_priority` writes on set and clear; list (and anything else) reads.
        ("pump_priority", json!({"action": "set"}), true),
        ("pump_priority", json!({"action": "clear"}), true),
        ("pump_priority", json!({"action": "list"}), false),
        ("pump_priority", json!({}), false),
        ("pump_priority", json!({"action": "wipe"}), false),
    ]
}

#[test]
fn classification_table_matches_the_wire_contract() {
    for (name, arguments, writes) in cases() {
        assert_eq!(is_write(name, &arguments), writes, "{name} {arguments}");
    }
}
