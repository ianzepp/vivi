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

/// A pool whose `P1` band lists one entry per `(model, reasoning, weight)`
/// triple; `None` stores no weight.
fn pool_with_entries(entries: &[(&str, &str, Option<f64>)]) -> PoolConfig {
    let mut bands = String::new();
    for (model, reasoning, weight) in entries {
        bands.push_str(&format!(
            "[[bands.P1]]\nmodel = \"{model}\"\nreasoning = \"{reasoning}\"\n"
        ));
        if let Some(weight) = weight {
            bands.push_str(&format!("weight = {weight}\n"));
        }
    }
    toml::from_str(&format!(
        "provider = \"local\"\nslots = 4\n{bands}[local]\nroot = \"~/lanes\"\n"
    ))
    .unwrap()
}

/// A draw source replaying canned draws in call order.
struct Canned(Vec<f64>);

impl Draw for Canned {
    fn draw(&mut self) -> Result<f64, VivariumError> {
        Ok(self.0.remove(0))
    }
}

/// A draw source whose entropy is always exhausted.
struct Dry;

impl Draw for Dry {
    fn draw(&mut self) -> Result<f64, VivariumError> {
        Err(VivariumError::Other("entropy unavailable".into()))
    }
}

/// Run the selection core for `P1` with `draws` replayed in order.
fn drawn(config: &PoolConfig, availability: &Availability, draws: &[f64]) -> Option<Choice> {
    select(config, "w", 1, availability, &mut Canned(draws.to_vec())).unwrap()
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
fn choose_draws_one_entry_of_the_bands_list() {
    let choice = choose(&bridge(), "bridge", 1).unwrap();
    assert!(
        (choice.model == "zai/glm-5.3-flash" && choice.reasoning == "xhigh")
            || (choice.model == "other/model" && choice.reasoning == "low"),
        "got: {choice:?}"
    );
    let choice = choose(&bridge(), "bridge", 2).unwrap();
    assert_eq!(choice.model, "zai/glm-5.3-flash");
    assert_eq!(choice.reasoning, "low");
}

#[test]
fn deterministic_equal_weight_draws_cover_all_four_entries() {
    let config = pool_with_entries(&[
        ("a/one", "low", None),
        ("b/two", "mid", None),
        ("c/three", "high", None),
        ("d/four", "low", None),
    ]);
    let nothing_blocked = Availability::default();
    let models: Vec<String> = [0.0, 0.3, 0.6, 0.9]
        .iter()
        .map(|draw| drawn(&config, &nothing_blocked, &[*draw]).unwrap().model)
        .collect();
    assert_eq!(models, ["a/one", "b/two", "c/three", "d/four"]);
}

#[test]
fn weighted_draws_follow_relative_weights() {
    let config = pool_with_entries(&[("a/one", "low", None), ("b/two", "high", Some(3.0))]);
    let nothing_blocked = Availability::default();
    // The weight-one entry owns a quarter of the range; the weight-three
    // entry owns the remaining three quarters.
    let choice = drawn(&config, &nothing_blocked, &[0.24]).unwrap();
    assert_eq!(choice.model, "a/one");
    let choice = drawn(&config, &nothing_blocked, &[0.26]).unwrap();
    assert_eq!(choice.model, "b/two");
    assert_eq!(choice.reasoning, "high");
}

#[test]
fn unavailable_entries_are_removed_before_weighting() {
    let config = pool_with_entries(&[
        ("a/one", "low", None),
        ("b/two", "mid", None),
        ("c/three", "high", None),
    ]);
    let mut capped = Availability::default();
    capped.capped_models.insert("b/two".into());
    // With three equal shares a draw of 0.4 lands on the capped entry; with
    // the cap removing that share before normalization it falls to `a/one`.
    assert_eq!(drawn(&config, &capped, &[0.4]).unwrap().model, "a/one");
    assert_eq!(drawn(&config, &capped, &[0.6]).unwrap().model, "c/three");

    // A provider cooldown still ahead of the view's instant blocks the same
    // way; a deadline at or before the instant does not block at all.
    let now = chrono::Utc::now();
    let mut cooling = Availability::at(now);
    cooling.provider_deadlines.insert(
        health::provider_key("c/three").to_string(),
        now + chrono::Duration::hours(1),
    );
    assert_eq!(drawn(&config, &cooling, &[0.6]).unwrap().model, "b/two");
    let expired = Availability {
        now: now + chrono::Duration::hours(2),
        ..cooling
    };
    assert_eq!(drawn(&config, &expired, &[0.7]).unwrap().model, "c/three");
}

#[test]
fn same_model_choices_keep_their_own_reasoning() {
    let config = pool_with_entries(&[("m/one", "low", None), ("m/one", "high", Some(2.0))]);
    let nothing_blocked = Availability::default();
    // Two entries of one model: each keeps its own reasoning and its own
    // share of the draw.
    assert_eq!(
        drawn(&config, &nothing_blocked, &[0.0]).unwrap().reasoning,
        "low"
    );
    assert_eq!(
        drawn(&config, &nothing_blocked, &[0.9]).unwrap().reasoning,
        "high"
    );
    // Capping the model blocks both entries at once, so the band has no
    // eligible choice left.
    let mut capped = Availability::default();
    capped.capped_models.insert("m/one".into());
    assert_eq!(choose_eligible(&config, "w", 1, &capped).unwrap(), None);
}

#[test]
fn single_entry_preserves_choice_without_entropy() {
    let config = pool_with_entries(&[("a/one", "high", Some(7.0))]);
    // An empty canister: the choice answers without a single draw.
    let choice = drawn(&config, &Availability::default(), &[]).unwrap();
    assert_eq!(
        choice,
        Choice {
            band: 1,
            model: "a/one".into(),
            reasoning: "high".into(),
        }
    );
}

#[test]
fn an_entropy_failure_is_an_error_not_a_fallback() {
    let config = pool_with_entries(&[("a/one", "low", None), ("b/two", "high", None)]);
    let err = select(&config, "w", 1, &Availability::default(), &mut Dry)
        .unwrap_err()
        .to_string();
    assert!(err.contains("entropy unavailable"), "got: {err}");
}

#[test]
fn a_missing_or_empty_band_is_a_configuration_error_for_eligible_too() {
    let err = choose_eligible(&bridge(), "bridge", 0, &Availability::default())
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("pool bridge has no model for band P0"),
        "got: {err}"
    );
    let bare: PoolConfig = toml::from_str(
        "provider = \"local\"\nslots = 4\nbands = { P3 = [] }\n[local]\nroot = \"~/lanes\"\n",
    )
    .unwrap();
    let err = choose_eligible(&bare, "bare", 3, &Availability::default())
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("pool bare has no model for band P3"),
        "got: {err}"
    );
}

#[test]
fn a_band_with_every_entry_blocked_has_no_eligible_choice() {
    let config = pool_with_entries(&[("a/one", "low", None), ("b/two", "high", None)]);
    let mut blocked = Availability::default();
    blocked.capped_models.insert("a/one".into());
    blocked.capped_models.insert("b/two".into());
    assert_eq!(choose_eligible(&config, "w", 1, &blocked).unwrap(), None);
    // `choose` answers anyway: its view has nothing blocked.
    assert!(choose(&config, "w", 1).is_ok());
}

#[test]
fn choose_eligible_answers_when_an_entry_survives_the_view() {
    let config = pool_with_entries(&[("a/one", "low", None), ("b/two", "high", None)]);
    let mut capped = Availability::default();
    capped.capped_models.insert("a/one".into());
    assert_eq!(
        choose_eligible(&config, "w", 1, &capped).unwrap(),
        Some(Choice {
            band: 1,
            model: "b/two".into(),
            reasoning: "high".into(),
        })
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
    let config = bridge();
    let text = toml::to_string(&config).unwrap();
    let again: PoolConfig = toml::from_str(&text).unwrap();
    again.validate(&PoolId::new("space", "", "main")).unwrap();
    for (key, entries) in &config.bands.entries {
        let reread = again.bands.entries.get(key).unwrap();
        assert_eq!(entries.len(), reread.len(), "band {key}");
        for (entry, again_entry) in entries.iter().zip(reread) {
            assert_eq!(entry.model, again_entry.model, "band {key}");
            assert_eq!(entry.reasoning, again_entry.reasoning, "band {key}");
            assert_eq!(
                entry.weight().unwrap(),
                again_entry.weight().unwrap(),
                "band {key}"
            );
        }
    }
}
