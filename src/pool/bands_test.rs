use super::*;

use crate::pool::slot::PoolId;

/// A pool with two choices for `P1` and one for `P2`.
fn bridge() -> PoolConfig {
    toml::from_str(
        r#"
provider = "local"
slots = 4
[[bands.P1]]
model = "zai/glm-5.3-flash"
reasoning = "xhigh"
[[bands.P1]]
model = "other/model"
reasoning = "low"
[[bands.P2]]
model = "zai/glm-5.3-flash"
reasoning = "low"
[local]
root = "~/lanes"
"#,
    )
    .unwrap()
}

#[test]
fn a_band_string_parses_to_its_band_number() {
    assert_eq!(parse_band("P1"), Some(1));
    assert_eq!(parse_band("p2"), Some(2));
    assert_eq!(parse_band("P2-S0"), Some(2));
    assert_eq!(parse_band("P5-S3"), Some(5));
}

#[test]
fn anything_but_a_band_string_is_no_band() {
    assert_eq!(parse_band("P6"), None);
    assert_eq!(parse_band("S1"), None);
    assert_eq!(parse_band(""), None);
    assert_eq!(parse_band("P1-S9"), None);
    assert_eq!(parse_band("P1-S4"), None);
    assert_eq!(parse_band("P1x"), None);
    assert_eq!(parse_band("P1-S0-x"), None);
}

#[test]
fn the_speed_axis_parses_case_insensitively_and_is_ignored() {
    assert_eq!(parse_band("p3-s2"), Some(3));
    assert_eq!(parse_band("P0-S3"), Some(0));
}

#[test]
fn choose_returns_the_first_entry_of_the_bands_list() {
    let choice = choose(&bridge(), "bridge", 1).unwrap();
    assert_eq!(
        choice,
        Choice {
            band: 1,
            model: "zai/glm-5.3-flash".into(),
            reasoning: "xhigh".into(),
        }
    );
    let choice = choose(&bridge(), "bridge", 2).unwrap();
    assert_eq!(choice.model, "zai/glm-5.3-flash");
    assert_eq!(choice.reasoning, "low");
}

#[test]
fn a_band_the_pool_does_not_serve_names_the_band_pool_and_served_bands() {
    let err = choose(&bridge(), "bridge", 0).unwrap_err().to_string();
    assert!(
        err.contains("pool bridge has no model for band P0"),
        "got: {err}"
    );
    assert!(err.contains("it serves P1, P2"), "got: {err}");
    assert!(
        err.contains("send this job to a pool that serves P0"),
        "got: {err}"
    );
    assert!(err.contains("or pass --model"), "got: {err}");
}

#[test]
fn a_pool_with_no_bands_table_serves_no_bands() {
    let bare: PoolConfig =
        toml::from_str("provider = \"local\"\nslots = 4\n[local]\nroot = \"~/lanes\"\n").unwrap();
    assert!(bare.bands.is_empty());
    let err = choose(&bare, "bare", 3).unwrap_err().to_string();
    assert!(
        err.contains("pool bare has no model for band P3"),
        "got: {err}"
    );
    assert!(err.contains("it serves no bands"), "got: {err}");
}

#[test]
fn the_first_band_line_of_a_body_wins() {
    assert_eq!(
        body_band("Do it.\n\nband: P1\nband: P2\n").unwrap(),
        Some(1)
    );
    assert_eq!(body_band("- band: p2\n").unwrap(), Some(2));
    assert_eq!(body_band("* band: P3-S1\n").unwrap(), Some(3));
}

#[test]
fn a_body_without_a_band_line_has_no_band() {
    assert_eq!(body_band("Do it.\n\nfrom: main\n").unwrap(), None);
    assert_eq!(body_band("").unwrap(), None);
    assert_eq!(body_band("bandwith: P1").unwrap(), None);
}

#[test]
fn a_band_value_with_spaces_is_skipped_not_an_error() {
    assert_eq!(body_band("band: P1 now\nband: P2\n").unwrap(), Some(2));
}

#[test]
fn a_band_line_that_does_not_parse_is_an_error_naming_the_value() {
    let err = body_band("band: P9\n").unwrap_err().to_string();
    assert!(err.contains("band: P9"), "got: {err}");
    let err = body_band("band: quick\n").unwrap_err().to_string();
    assert!(err.contains("band: quick"), "got: {err}");
}

#[test]
fn a_pool_config_roundtrips_through_its_bands_table() {
    let config = bridge();
    let text = toml::to_string(&config).unwrap();
    let again: PoolConfig = toml::from_str(&text).unwrap();
    again.validate(&PoolId::new("space", "main")).unwrap();
    assert_eq!(
        choose(&again, "bridge", 1).unwrap(),
        choose(&config, "bridge", 1).unwrap()
    );
}
