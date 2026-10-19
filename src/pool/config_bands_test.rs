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
