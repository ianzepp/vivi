use std::collections::BTreeMap;
use std::io;

use super::*;

use crate::letter::work::TaskFront;
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

/// A pool with the full four choices for `P1`, weighted as named; an empty
/// weight string omits the `weight` key, meaning 1.
fn quartet(weights: [&str; 4]) -> PoolConfig {
    let mut text = String::from("provider = \"local\"\nslots = 4\n");
    for (n, weight) in weights.iter().enumerate() {
        text.push_str(&format!(
            "[[bands.P1]]\nmodel = \"m{n}\"\nreasoning = \"low\"\n"
        ));
        if !weight.is_empty() {
            text.push_str(&format!("weight = {weight}\n"));
        }
    }
    text.push_str("[local]\nroot = \"~/lanes\"\n");
    toml::from_str(&text).unwrap()
}

/// A draw source with canned values, served in order and reduced into
/// `0..below`; exhausted, it errors. Repeatable: no real entropy.
struct Canned(Vec<u64>);

impl Draw for Canned {
    fn select(&mut self, below: u64) -> Result<u64, VivariumError> {
        let value = self
            .0
            .first()
            .copied()
            .ok_or_else(|| VivariumError::Message("the draw source is exhausted".into()))?;
        self.0.remove(0);
        Ok(value % below)
    }
}

/// A draw source that always fails, as OS entropy would when the system
/// refuses to hand over a word.
struct Failing;

impl Draw for Failing {
    fn select(&mut self, _below: u64) -> Result<u64, VivariumError> {
        Err(VivariumError::Io(io::Error::other("no entropy")))
    }
}

/// A draw source cycling through `0..below`, exactly uniform over any run
/// whose length is a multiple of the range.
struct Cycle(u64);

impl Draw for Cycle {
    fn select(&mut self, below: u64) -> Result<u64, VivariumError> {
        self.0 = (self.0 + 1) % below;
        Ok(self.0)
    }
}

/// `draws` selections over `config`'s band, counted per model.
fn tallies(
    config: &PoolConfig,
    band: u8,
    draw: &mut dyn Draw,
    draws: usize,
) -> BTreeMap<String, usize> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for _ in 0..draws {
        let choice = choose_with(config, "bridge", band, draw).unwrap();
        *counts.entry(choice.model).or_default() += 1;
    }
    counts
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
fn all_four_choices_participate() {
    let config = quartet(["", "", "", ""]);
    let mut draws = Canned(vec![0, 1, 2, 3]);
    let mut served: Vec<String> = (0..4)
        .map(|_| choose_with(&config, "bridge", 1, &mut draws).unwrap().model)
        .collect();
    served.sort();
    assert_eq!(served, ["m0", "m1", "m2", "m3"]);
}

#[test]
fn equal_weights_are_uniform() {
    // Two unweighted entries over 400 fixed draws: each takes its half of
    // the work, inside a wide tolerance so the check states a share, not a
    // streak.
    let counts = tallies(&bridge(), 1, &mut Cycle(0), 400);
    for model in ["zai/glm-5.3-flash", "other/model"] {
        let count = counts[model];
        assert!(count.abs_diff(200) <= 20, "{model} drew {count} of 400");
    }
}

#[test]
fn weighted_choices_follow_their_shares() {
    // Weights 1, 3, 1, 1: the total is 6 and 600 is a whole number of
    // cycles of it, so every share lands exactly; the tolerance still
    // states the share honestly rather than demanding a perfect run.
    let counts = tallies(&quartet(["1", "3", "", ""]), 1, &mut Cycle(0), 600);
    let wants = [("m0", 100), ("m1", 300), ("m2", 100), ("m3", 100)];
    for (model, want) in wants {
        let count = counts[model];
        assert!(
            count.abs_diff(want) <= 30,
            "{model} drew {count}, want {want}"
        );
    }
}

#[test]
fn single_choice_preserves_model_and_reasoning() {
    // The sole entry is returned without drawing randomness: even a draw
    // source that always fails cannot disturb it.
    let mut draws = Failing;
    let choice = choose_with(&bridge(), "bridge", 2, &mut draws).unwrap();
    assert_eq!(choice.band, 2);
    assert_eq!(choice.model, "zai/glm-5.3-flash");
    assert_eq!(choice.reasoning, "low");
    let choice = choose(&bridge(), "bridge", 2).unwrap();
    assert_eq!(choice.model, "zai/glm-5.3-flash");
    assert_eq!(choice.reasoning, "low");
}

#[test]
fn bounded_reduction_rejects_words_at_or_above_the_range() {
    // Masking covers the range with a power of two; the excess is redrawn,
    // so a kept value is always inside `0..below`.
    assert_eq!(bounded(2, 3), Some(2));
    assert_eq!(bounded(0, 3), Some(0));
    assert_eq!(bounded(3, 3), None, "3 is the first value past 0..3");
    assert_eq!(bounded(u64::MAX, 3), None);
    assert_eq!(bounded(u64::MAX, 5), None, "15 masks to 15, past 0..5");
    assert_eq!(bounded(4, 5), Some(4));
    assert_eq!(bounded(u64::MAX, 1), Some(0), "a one-valued range keeps 0");
}

#[test]
fn an_invalid_programmatic_weight_fails_even_for_a_single_entry() {
    for weight in ["0", "-1", "4294967296"] {
        let config: PoolConfig = toml::from_str(&format!(
            "provider = \"local\"\nslots = 4\n[[bands.P1]]\n\
             model = \"m0\"\nreasoning = \"low\"\nweight = {weight}\n\
             [local]\nroot = \"~/lanes\"\n"
        ))
        .unwrap();
        let err = choose(&config, "solo", 1).unwrap_err().to_string();
        assert!(
            err.contains(&format!("gives weight {weight}")),
            "weight {weight}: got: {err}"
        );
        assert!(err.contains("[pools.solo]"), "weight {weight}: got: {err}");
    }
}

#[test]
fn a_failing_draw_propagates_without_a_fallback() {
    let err = choose_with(&bridge(), "bridge", 1, &mut Failing)
        .unwrap_err()
        .to_string();
    assert!(err.contains("no entropy"), "got: {err}");
    let mut draws = Canned(vec![]);
    let err = choose_with(&bridge(), "bridge", 1, &mut draws)
        .unwrap_err()
        .to_string();
    assert!(err.contains("exhausted"), "got: {err}");
}

#[test]
fn large_weights_sum_without_overflow() {
    // Two entries at the top of the weight range: the total is summed in
    // u64, and the draw falling past the first entry's cumulative weight
    // names the second entry, not the first.
    let config = quartet(["4294967295", "4294967295", "", ""]);
    let mut draws = Canned(vec![u64::from(u32::MAX) - 1, u64::from(u32::MAX)]);
    assert_eq!(
        choose_with(&config, "bridge", 1, &mut draws).unwrap().model,
        "m0"
    );
    assert_eq!(
        choose_with(&config, "bridge", 1, &mut draws).unwrap().model,
        "m1"
    );
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
fn an_empty_bands_list_serves_nothing() {
    let config: PoolConfig = toml::from_str(
        "provider = \"local\"\nslots = 4\nbands.P1 = []\n[local]\nroot = \"~/lanes\"\n",
    )
    .unwrap();
    let err = choose(&config, "hollow", 1).unwrap_err().to_string();
    assert!(
        err.contains("pool hollow has no model for band P1"),
        "got: {err}"
    );
}

fn band_body(band: Option<&str>) -> String {
    let front = TaskFront {
        band: band.map(str::to_string),
        ..TaskFront::default()
    };
    crate::letter::fixture::task_body_with(&front, "Do it.")
}

#[test]
fn the_band_key_of_the_front_matter_is_read() {
    assert_eq!(body_band(&band_body(Some("P1"))).unwrap(), Some(1));
    assert_eq!(body_band(&band_body(Some("p2"))).unwrap(), Some(2));
    assert_eq!(body_band(&band_body(Some("P3-S1"))).unwrap(), Some(3));
}

#[test]
fn a_body_without_a_band_key_has_no_band() {
    let front = TaskFront {
        from: Some("main".into()),
        ..TaskFront::default()
    };
    let body = crate::letter::fixture::task_body_with(&front, "Do it.");
    assert_eq!(body_band(&body).unwrap(), None);
    assert_eq!(body_band(&band_body(None)).unwrap(), None);
    assert_eq!(body_band("").unwrap(), None);
}

#[test]
fn a_band_key_that_does_not_parse_is_an_error_naming_the_value() {
    let err = body_band(&band_body(Some("P9"))).unwrap_err().to_string();
    assert!(err.contains("band: P9"), "got: {err}");
    let err = body_band(&band_body(Some("quick")))
        .unwrap_err()
        .to_string();
    assert!(err.contains("band: quick"), "got: {err}");
}

#[test]
fn a_pool_config_roundtrips_through_its_bands_table() {
    // Selection is a draw, so the round trip is judged on what was
    // configured: the entries and their effective weights, not on two
    // independent draws agreeing.
    let config = quartet(["2", "3", "", ""]);
    let text = toml::to_string(&config).unwrap();
    let again: PoolConfig = toml::from_str(&text).unwrap();
    again.validate(&PoolId::new("space", "", "main")).unwrap();
    for (key, entries) in &config.bands.entries {
        let mirrored = again
            .bands
            .entries
            .get(key)
            .unwrap_or_else(|| panic!("{key} survives the round trip"));
        assert_eq!(entries.len(), mirrored.len(), "{key} keeps its entry count");
        for (entry, twin) in entries.iter().zip(mirrored) {
            assert_eq!(
                (entry.model.as_str(), entry.reasoning.as_str()),
                (twin.model.as_str(), twin.reasoning.as_str()),
                "{key} keeps its model-and-reasoning pairs"
            );
            assert_eq!(
                entry.effective_weight("bridge", key).unwrap(),
                twin.effective_weight("bridge", key).unwrap(),
                "{key} keeps its effective weights"
            );
        }
    }
}
