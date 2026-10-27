use super::*;

use crate::pool::slot::PoolId;

fn pool(toml_text: &str) -> PoolConfig {
    toml::from_str(toml_text).unwrap()
}

fn id() -> PoolId {
    PoolId::new("space", "", "main")
}

const HARNESS: &str = "\
[harness]
name = \"pi\"
run = \"pi --model {model}\"
models = [\"zai/glm-5.3-flash\", \"other/model\"]
";

const TAIL: &str = "\
[local]
root = \"~/lanes\"
";

#[test]
fn a_valid_bands_table_validates() {
    let config = pool(&format!(
        "provider = \"local\"\nslots = 4\n{HARNESS}\
         [[bands.P1]]\nmodel = \"zai/glm-5.3-flash\"\nreasoning = \"xhigh\"\n\
         [[bands.P2]]\nmodel = \"other/model\"\nreasoning = \"low\"\n{TAIL}"
    ));
    assert_eq!(config.bands.entries.len(), 2);
    assert_eq!(config.bands.entries["P1"].len(), 1);
    assert_eq!(config.bands.entries["P1"][0].model, "zai/glm-5.3-flash");
    config.validate(&id()).unwrap();
}

#[test]
fn a_pool_with_no_bands_table_validates_unchanged() {
    let config = pool(&format!("provider = \"local\"\nslots = 4\n{TAIL}"));
    assert!(config.bands.is_empty());
    config.validate(&id()).unwrap();
}

#[test]
fn an_unknown_band_key_is_refused_naming_the_allowed_set() {
    let config = pool(&format!(
        "provider = \"local\"\nslots = 4\n{HARNESS}\
         [[bands.P9]]\nmodel = \"zai/glm-5.3-flash\"\nreasoning = \"low\"\n{TAIL}"
    ));
    let err = config.validate(&id()).unwrap_err().to_string();
    assert!(err.contains("[pools.main]"), "got: {err}");
    assert!(err.contains("bands holds the key \"P9\""), "got: {err}");
    assert!(err.contains("P0, P1, P2, P3, P4, P5"), "got: {err}");
}

#[test]
fn more_than_four_choices_for_a_band_are_refused() {
    let choices = "[[bands.P1]]\nmodel = \"zai/glm-5.3-flash\"\nreasoning = \"low\"\n".repeat(5);
    let config = pool(&format!(
        "provider = \"local\"\nslots = 4\n{HARNESS}{choices}{TAIL}"
    ));
    let err = config.validate(&id()).unwrap_err().to_string();
    assert!(err.contains("bands.P1 holds 5 choices"), "got: {err}");
    assert!(err.contains("at most 4"), "got: {err}");
}

#[test]
fn four_choices_for_a_band_are_still_allowed() {
    let choices = "[[bands.P1]]\nmodel = \"zai/glm-5.3-flash\"\nreasoning = \"low\"\n".repeat(4);
    let config = pool(&format!(
        "provider = \"local\"\nslots = 4\n{HARNESS}{choices}{TAIL}"
    ));
    config.validate(&id()).unwrap();
}

#[test]
fn a_reasoning_outside_the_allowed_characters_is_refused() {
    let config = pool(&format!(
        "provider = \"local\"\nslots = 4\n{HARNESS}\
         [[bands.P1]]\nmodel = \"zai/glm-5.3-flash\"\nreasoning = \"x high\"\n{TAIL}"
    ));
    let err = config.validate(&id()).unwrap_err().to_string();
    assert!(
        err.contains("bands.P1 has reasoning \"x high\""),
        "got: {err}"
    );
    assert!(err.contains("1 to 32 characters"), "got: {err}");
}

#[test]
fn an_empty_reasoning_is_refused_too() {
    let config = pool(&format!(
        "provider = \"local\"\nslots = 4\n{HARNESS}\
         [[bands.P1]]\nmodel = \"zai/glm-5.3-flash\"\nreasoning = \"\"\n{TAIL}"
    ));
    let err = config.validate(&id()).unwrap_err().to_string();
    assert!(err.contains("bands.P1 has reasoning \"\""), "got: {err}");
}

#[test]
fn a_band_model_the_harness_does_not_offer_is_refused() {
    let config = pool(&format!(
        "provider = \"local\"\nslots = 4\n{HARNESS}\
         [[bands.P0]]\nmodel = \"mystery/model\"\nreasoning = \"low\"\n{TAIL}"
    ));
    let err = config.validate(&id()).unwrap_err().to_string();
    assert!(
        err.contains("bands.P0 gives model \"mystery/model\""),
        "got: {err}"
    );
    assert!(
        err.contains("harness.models does not offer (zai/glm-5.3-flash, other/model)"),
        "got: {err}"
    );
}

#[test]
fn omitted_band_weight_is_one() {
    let config = pool(&format!(
        "provider = \"local\"\nslots = 4\n{HARNESS}\
         [[bands.P1]]\nmodel = \"zai/glm-5.3-flash\"\nreasoning = \"low\"\n{TAIL}"
    ));
    let choice = &config.bands.entries["P1"][0];
    assert!(choice.unknown.get("weight").is_none());
    assert_eq!(choice.weight(), 1);
    config.validate(&id()).unwrap();
}

#[test]
fn explicit_band_weight_roundtrips() {
    let config = pool(&format!(
        "provider = \"local\"\nslots = 4\n{HARNESS}\
         [[bands.P1]]\nmodel = \"zai/glm-5.3-flash\"\nreasoning = \"low\"\nweight = 3\n\
         [[bands.P2]]\nmodel = \"other/model\"\nreasoning = \"low\"\nweight = 4294967295\n{TAIL}"
    ));
    assert_eq!(config.bands.entries["P1"][0].weight(), 3);
    assert_eq!(config.bands.entries["P2"][0].weight(), u32::MAX);
    config.validate(&id()).unwrap();
    let text = toml::to_string(&config).unwrap();
    assert!(text.contains("weight = 3"), "{text}");
    assert!(text.contains("weight = 4294967295"), "{text}");
    let again: PoolConfig = toml::from_str(&text).unwrap();
    assert_eq!(again.bands.entries["P1"][0].weight(), 3);
    assert_eq!(again.bands.entries["P2"][0].weight(), u32::MAX);
}

#[test]
fn an_absent_weight_stays_absent_through_a_roundtrip() {
    let config = pool(&format!(
        "provider = \"local\"\nslots = 4\n{HARNESS}\
         [[bands.P1]]\nmodel = \"zai/glm-5.3-flash\"\nreasoning = \"low\"\n{TAIL}"
    ));
    let text = toml::to_string(&config).unwrap();
    assert!(!text.contains("weight"), "{text}");
    let again: PoolConfig = toml::from_str(&text).unwrap();
    assert!(again.bands.entries["P1"][0].unknown.get("weight").is_none());
    assert_eq!(again.bands.entries["P1"][0].weight(), 1);
}

#[test]
fn invalid_band_weights_are_config_errors() {
    for weight in [
        "0",
        "-1",
        "1.5",
        "\"3\"",
        "true",
        "4294967296",
        "9223372036854775807",
    ] {
        let config = pool(&format!(
            "provider = \"local\"\nslots = 4\n{HARNESS}\
             [[bands.P1]]\nmodel = \"zai/glm-5.3-flash\"\nreasoning = \"low\"\
             \nweight = {weight}\n{TAIL}"
        ));
        let err = config.validate(&id()).unwrap_err().to_string();
        assert!(err.contains("invalid-band-weight"), "{weight}: {err}");
        assert!(err.contains("bands.P1"), "{weight}: {err}");
    }
}

#[test]
fn unknown_band_choice_fields_still_fail() {
    let config = pool(&format!(
        "provider = \"local\"\nslots = 4\n{HARNESS}\
         [[bands.P1]]\nmodel = \"zai/glm-5.3-flash\"\nreasoning = \"low\"\neffort = 3\n{TAIL}"
    ));
    let err = config.validate(&id()).unwrap_err().to_string();
    assert!(
        err.contains("unknown field \"effort\" in [pools.main.bands.P1]"),
        "got: {err}"
    );
}

#[test]
fn an_unknown_field_besides_a_weight_is_still_refused() {
    let config = pool(&format!(
        "provider = \"local\"\nslots = 4\n{HARNESS}\
         [[bands.P1]]\nmodel = \"zai/glm-5.3-flash\"\nreasoning = \"low\"\
         \nweight = 2\neffort = 3\n{TAIL}"
    ));
    let err = config.validate(&id()).unwrap_err().to_string();
    assert!(
        err.contains("unknown field \"effort\" in [pools.main.bands.P1]"),
        "got: {err}"
    );
}

#[test]
fn an_unknown_field_in_a_band_choice_is_refused() {
    let config = pool(&format!(
        "provider = \"local\"\nslots = 4\n{HARNESS}\
         [[bands.P1]]\nmodel = \"zai/glm-5.3-flash\"\nreasoning = \"low\"\neffort = 3\n{TAIL}"
    ));
    let err = config.validate(&id()).unwrap_err().to_string();
    assert!(
        err.contains("unknown field \"effort\" in [pools.main.bands.P1]"),
        "got: {err}"
    );
}
