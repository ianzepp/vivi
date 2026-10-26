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
fn band_weight_defaults_to_one() {
    let config = pool(&format!(
        "provider = \"local\"\nslots = 4\n{HARNESS}\
         [[bands.P1]]\nmodel = \"zai/glm-5.3-flash\"\nreasoning = \"low\"\n{TAIL}"
    ));
    config.validate(&id()).unwrap();
    let choice = &config.bands.entries["P1"][0];
    assert_eq!(choice.weight().unwrap(), 1.0);
}

#[test]
fn band_weights_accept_positive_integer_and_fraction() {
    let config = pool(&format!(
        "provider = \"local\"\nslots = 4\n{HARNESS}\
         [[bands.P1]]\nmodel = \"zai/glm-5.3-flash\"\nreasoning = \"low\"\nweight = 3\n\
         [[bands.P1]]\nmodel = \"other/model\"\nreasoning = \"high\"\nweight = 0.25\n{TAIL}"
    ));
    config.validate(&id()).unwrap();
    let choices = &config.bands.entries["P1"];
    assert_eq!(choices[0].weight().unwrap(), 3.0);
    assert_eq!(choices[1].weight().unwrap(), 0.25);
}

#[test]
fn band_weight_rejects_invalid_values() {
    for bad in [
        "0",
        "0.0",
        "-2",
        "-0.5",
        "nan",
        "inf",
        "\"heavy\"",
        "true",
        "[1]",
    ] {
        let config = pool(&format!(
            "provider = \"local\"\nslots = 4\n{HARNESS}\
             [[bands.P1]]\nmodel = \"zai/glm-5.3-flash\"\nreasoning = \"low\"\
             \nweight = {bad}\n{TAIL}"
        ));
        let err = config.validate(&id()).unwrap_err();
        assert!(matches!(err, VivariumError::Config(_)), "for {bad}: {err}");
        let text = err.to_string();
        assert!(text.contains("pools.main"), "for {bad}: {text}");
        assert!(text.contains("bands.P1"), "for {bad}: {text}");
        assert!(text.contains("\"weight\""), "for {bad}: {text}");
        let direct = config.bands.entries["P1"][0]
            .weight()
            .expect_err("an invalid stored weight must fail the accessor");
        assert!(matches!(direct, VivariumError::Config(_)), "for {bad}");
    }
}

#[test]
fn band_weight_roundtrips_without_losing_other_entries() {
    let config = pool(&format!(
        "provider = \"local\"\nslots = 4\n{HARNESS}\
         [[bands.P1]]\nmodel = \"zai/glm-5.3-flash\"\nreasoning = \"low\"\
         \nweight = 2.5\n\
         [[bands.P2]]\nmodel = \"other/model\"\nreasoning = \"high\"\n{TAIL}"
    ));
    config.validate(&id()).unwrap();
    let text = toml::to_string(&config.bands).unwrap();
    assert!(text.contains("weight = 2.5"), "got: {text}");
    let round: PoolBands = toml::from_str(&text).unwrap();
    assert_eq!(round.entries["P1"][0].weight().unwrap(), 2.5);
    assert_eq!(round.entries["P1"][0].model, "zai/glm-5.3-flash");
    assert_eq!(round.entries["P1"][0].reasoning, "low");
    assert_eq!(round.entries["P2"][0].weight().unwrap(), 1.0);
    assert!(!round.entries["P2"][0].unknown.contains_key("weight"));
    let plain: PoolBands = toml::from_str("[[P1]]\nmodel = \"m\"\nreasoning = \"low\"\n").unwrap();
    assert!(!toml::to_string(&plain).unwrap().contains("weight"));
}

#[test]
fn band_weight_does_not_allow_unrelated_unknown_keys() {
    let config = pool(&format!(
        "provider = \"local\"\nslots = 4\n{HARNESS}\
         [[bands.P1]]\nmodel = \"zai/glm-5.3-flash\"\nreasoning = \"low\"\
         \nweight = 2\neffort = 3\n{TAIL}"
    ));
    let err = config.validate(&id()).unwrap_err();
    assert!(matches!(err, VivariumError::Config(_)), "got: {err}");
    let text = err.to_string();
    assert!(text.contains("unknown field \"effort\""), "got: {text}");
    assert!(text.contains("pools.main.bands.P1"), "got: {text}");
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
