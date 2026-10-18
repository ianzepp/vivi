use super::*;

use crate::pool::slot::PoolId;

/// A pool that serves `P1` with the flash model at `xhigh` and `P2` with the
/// other model at `low`, and offers both models to its harness.
fn pool() -> PoolConfig {
    toml::from_str(
        r#"
provider = "local"
slots = 4
[harness]
name = "pi"
run = "pi --model {model}"
models = ["zai/glm-5.3-flash", "other/model"]
[[bands.P1]]
model = "zai/glm-5.3-flash"
reasoning = "xhigh"
[[bands.P2]]
model = "other/model"
reasoning = "low"
[local]
root = "~/lanes"
"#,
    )
    .unwrap()
}

fn pool_config(toml_text: &str) -> PoolConfig {
    toml::from_str(toml_text).unwrap()
}

fn role(name: &str, kind: Option<&str>, band: Option<&str>) -> RoleFacts {
    RoleFacts {
        name: name.to_string(),
        kind: kind.map(str::to_string),
        band: band.map(str::to_string),
        charter: String::new(),
        read_only: None,
    }
}

fn input<'a>(
    role: Option<&'a RoleFacts>,
    body: &'a str,
    model: Option<&'a str>,
    read_only: bool,
) -> Input<'a> {
    Input {
        model_flag: model,
        read_only_flag: read_only,
        role,
        body,
    }
}

fn plain(body: &str) -> Input<'_> {
    input(None, body, None, false)
}

#[test]
fn an_auditor_role_is_read_only_and_takes_its_band_model_and_reasoning() {
    let auditor = role("reviewer", Some("auditor"), Some("P1-S0"));
    let resolved = resolve(
        &pool(),
        "bridge",
        &input(Some(&auditor), "review it", None, false),
    )
    .unwrap();
    assert_eq!(
        resolved,
        Resolved {
            model: "zai/glm-5.3-flash".into(),
            reasoning: Some("xhigh".into()),
            band: Some(1),
            read_only: true,
            role: Some("reviewer".into()),
        }
    );
}

#[test]
fn triage_and_cadence_kinds_are_read_only_too() {
    for kind in [
        "triage", "cadence", "AUDITOR", "Triage", "steward", "head", "gate",
    ] {
        let worker = role("gatekeeper", Some(kind), Some("P1"));
        let resolved = resolve(
            &pool(),
            "bridge",
            &input(Some(&worker), "sort it", None, false),
        )
        .unwrap();
        assert!(resolved.read_only, "kind {kind} must be read-only");
    }
}

#[test]
fn an_explicit_read_only_flag_on_a_hand_role_wins() {
    let mut hand = role("lane-3", Some("hand"), Some("P1"));
    hand.read_only = Some(true);
    let resolved = resolve(
        &pool(),
        "bridge",
        &input(Some(&hand), "build it", None, false),
    )
    .unwrap();
    assert!(resolved.read_only);
}

#[test]
fn a_hand_role_and_a_merge_named_hand_write() {
    let hand = role("lane-3", Some("hand"), Some("P1"));
    let resolved = resolve(
        &pool(),
        "bridge",
        &input(Some(&hand), "build it", None, false),
    )
    .unwrap();
    assert!(!resolved.read_only);
    let merge = role("merge", Some("hand"), Some("P1"));
    let resolved = resolve(
        &pool(),
        "bridge",
        &input(Some(&merge), "merge it", None, false),
    )
    .unwrap();
    assert!(!resolved.read_only);
    let capitalised = role("lane-4", Some("Hand"), Some("P1"));
    let resolved = resolve(
        &pool(),
        "bridge",
        &input(Some(&capitalised), "build it", None, false),
    )
    .unwrap();
    assert!(!resolved.read_only);
    let lint = role("lint", Some("lint"), Some("P1"));
    let resolved = resolve(
        &pool(),
        "bridge",
        &input(Some(&lint), "lint it", None, false),
    )
    .unwrap();
    assert!(!resolved.read_only);
}

#[test]
fn a_read_only_flag_makes_a_hand_role_read_only() {
    let hand = role("lane-3", Some("hand"), Some("P1"));
    let resolved = resolve(
        &pool(),
        "bridge",
        &input(Some(&hand), "build it", None, true),
    )
    .unwrap();
    assert!(resolved.read_only);
}

#[test]
fn a_body_band_line_beats_the_role_band() {
    let worker = role("lane-3", Some("hand"), Some("P2-S0"));
    let resolved = resolve(
        &pool(),
        "bridge",
        &input(Some(&worker), "build it\nband: P1", None, false),
    )
    .unwrap();
    assert_eq!(resolved.band, Some(1));
    assert_eq!(resolved.model, "zai/glm-5.3-flash");
    assert_eq!(resolved.reasoning.as_deref(), Some("xhigh"));
}

#[test]
fn a_role_band_alone_resolves() {
    let worker = role("lane-3", Some("hand"), Some("P2-S0"));
    let resolved = resolve(
        &pool(),
        "bridge",
        &input(Some(&worker), "build it", None, false),
    )
    .unwrap();
    assert_eq!(resolved.band, Some(2));
    assert_eq!(resolved.model, "other/model");
    assert_eq!(resolved.reasoning.as_deref(), Some("low"));
}

#[test]
fn an_unparseable_role_band_is_an_error_naming_the_role_and_the_value() {
    let worker = role("scribe", Some("hand"), Some("S1"));
    let err = resolve(
        &pool(),
        "bridge",
        &input(Some(&worker), "write it", None, false),
    )
    .unwrap_err();
    let message = err.to_string();
    assert!(message.contains("role scribe"), "got: {message}");
    assert!(message.contains("S1"), "got: {message}");
}

#[test]
fn no_band_and_no_model_is_an_error_naming_the_role() {
    let worker = role("scribe", Some("hand"), None);
    let err = resolve(
        &pool(),
        "bridge",
        &input(Some(&worker), "write it", None, false),
    )
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "configuration error: no model: pass --model, give the task a `band:` line, \
         or give role scribe a band (`vivi role set scribe --model P2`)"
    );
}

#[test]
fn no_band_and_no_model_without_a_role_blames_the_role_slot() {
    let err = resolve(&pool(), "bridge", &plain("write it")).unwrap_err();
    assert_eq!(
        err.to_string(),
        "configuration error: no model: pass --model, give the task a `band:` line, \
         or give the role a band (`vivi role set <name> --model P2`)"
    );
}

#[test]
fn a_band_the_pool_does_not_serve_is_refused_naming_the_band_and_pool() {
    let err = resolve(&pool(), "bridge", &plain("build it\nband: P3")).unwrap_err();
    let message = err.to_string();
    assert!(message.contains("band P3"), "got: {message}");
    assert!(message.contains("bridge"), "got: {message}");
}

#[test]
fn a_model_override_keeps_the_role_read_only_and_takes_reasoning_only_for_a_match() {
    let auditor = role("reviewer", Some("auditor"), Some("P1-S0"));
    let same = resolve(
        &pool(),
        "bridge",
        &input(
            Some(&auditor),
            "review it",
            Some("zai/glm-5.3-flash"),
            false,
        ),
    )
    .unwrap();
    assert!(same.read_only);
    assert_eq!(same.model, "zai/glm-5.3-flash");
    assert_eq!(same.reasoning.as_deref(), Some("xhigh"));

    let other = resolve(
        &pool(),
        "bridge",
        &input(Some(&auditor), "review it", Some("other/model"), false),
    )
    .unwrap();
    assert!(other.read_only);
    assert_eq!(other.model, "other/model");
    assert_eq!(other.reasoning, None);
}

#[test]
fn a_model_override_needs_no_band_and_ignores_a_band_the_pool_does_not_serve() {
    let hand = role("lane-3", Some("hand"), None);
    let resolved = resolve(
        &pool(),
        "bridge",
        &input(Some(&hand), "build it", Some("other/model"), false),
    )
    .unwrap();
    assert_eq!(resolved.band, None);
    assert_eq!(resolved.reasoning, None);
    let unserved = role("lane-3", Some("hand"), Some("P4"));
    let resolved = resolve(
        &pool(),
        "bridge",
        &input(Some(&unserved), "build it", Some("other/model"), false),
    )
    .unwrap();
    assert_eq!(resolved.band, Some(4));
    assert_eq!(resolved.reasoning, None);
}

#[test]
fn a_model_the_harness_does_not_offer_is_refused_naming_the_model() {
    let hand = role("lane-3", Some("hand"), Some("P1"));
    let err = resolve(
        &pool(),
        "bridge",
        &input(Some(&hand), "build it", Some("mystery/model"), false),
    )
    .unwrap_err();
    let message = err.to_string();
    assert!(message.contains("mystery/model"), "got: {message}");
    assert!(message.contains("harness.models"), "got: {message}");
}

#[test]
fn a_band_choice_outside_the_harness_models_is_refused_too() {
    let config = pool_config(
        r#"
provider = "local"
slots = 4
[harness]
name = "pi"
run = "pi --model {model}"
models = ["zai/glm-5.3-flash"]
[[bands.P1]]
model = "mystery/model"
reasoning = "low"
[local]
root = "~/lanes"
"#,
    );
    let hand = role("lane-3", Some("hand"), Some("P1"));
    let err = resolve(
        &config,
        "bridge",
        &input(Some(&hand), "build it", None, false),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("mystery/model"), "got: {err}");
}

const RULES: &str = "Work the brief. Never weaken a test.\n";

#[test]
fn a_blank_charter_leaves_the_pool_rules_byte_for_byte() {
    for charter in ["", "  ", "\n  \n"] {
        assert_eq!(standing_prompt("auditor", charter, RULES, true), RULES);
        assert_eq!(standing_prompt("auditor", charter, RULES, false), RULES);
    }
}

#[test]
fn a_charter_gives_the_headings_in_order_over_the_pool_rules() {
    let prompt = standing_prompt("scribe", "Write the letters.", RULES, false);
    assert_eq!(
        prompt,
        "# Standing charter (role scribe)\n\nWrite the letters.\n\n---\n\n\
         You run in a pool slot with no `vivi` command and no mailspace. Where the charter tells you to read or settle your handle with `vivi`, read your brief instead and end with your receipt on standard output; the runner records your result and the Mind settles the handle.\n\n\
         # Pool rules (these bound the charter)\n\nWork the brief. Never weaken a test.\n"
    );
    let charter = prompt.find("# Standing charter (role scribe)").unwrap();
    let rule = prompt
        .find("# Pool rules (these bound the charter)")
        .unwrap();
    let separator = prompt.find("---").unwrap();
    assert!(charter < separator && separator < rule);
}

#[test]
fn the_two_pool_notes_differ() {
    let bridged = standing_prompt("scribe", "Write the letters.", RULES, true);
    let unbridged = standing_prompt("scribe", "Write the letters.", RULES, false);
    assert_ne!(bridged, unbridged);
    assert!(bridged.contains("MCP server `vivi`"), "got: {bridged}");
    assert!(
        unbridged.contains("no `vivi` command and no mailspace"),
        "got: {unbridged}"
    );
}

#[test]
fn resolve_leaves_the_pool_config_alone() {
    let config = pool();
    let worker = role("lane-3", Some("hand"), Some("P1"));
    resolve(
        &config,
        "bridge",
        &input(Some(&worker), "build it", None, false),
    )
    .unwrap();
    config.validate(&PoolId::new("space", "bridge")).unwrap();
}
