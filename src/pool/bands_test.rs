use super::*;

use crate::letter::work::TaskFront;
use crate::pool::config_bands::BandChoice;
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
fn choose_returns_a_complete_pair_from_the_band_it_serves() {
    let config = bridge();
    let choice = choose(&config, "bridge", 2).unwrap();
    assert_eq!(choice.band, 2);
    assert_eq!(choice.model, "zai/glm-5.3-flash");
    assert_eq!(choice.reasoning, "low");

    // Both entries of a multi-entry band take work: the sampled pair is
    // always one the pool listed, never a reserved spare.
    let listed = [("zai/glm-5.3-flash", "xhigh"), ("other/model", "low")];
    for _ in 0..16 {
        let choice = choose(&config, "bridge", 1).unwrap();
        let pair = (choice.model.as_str(), choice.reasoning.as_str());
        assert!(listed.contains(&pair), "got: {pair:?}");
    }
}

#[test]
fn single_entry_preserves_pair_without_randomness() {
    let list = vec![choice("zai/glm-5.3-flash", "xhigh", None)];
    // `Dry` fails on every draw, so an answer proves none was consulted.
    let picked = select(&list, &mut Dry).unwrap();
    assert_eq!(picked.model, "zai/glm-5.3-flash");
    assert_eq!(picked.reasoning, "xhigh");
}

#[test]
fn equal_weights_distribute_across_all_four_entries() {
    let list: Vec<BandChoice> = (0..4)
        .map(|n| choice(&format!("m/{n}"), "low", None))
        .collect();
    // Residues 0 to 3 of a weight total of 4: each entry serves exactly one.
    let mut draw = Canned(vec![0, 1, 2, 3]);
    let mut served = [0usize; 4];
    for _ in 0..4 {
        let picked = select(&list, &mut draw).unwrap();
        let index = list.iter().position(|c| c.model == picked.model).unwrap();
        served[index] += 1;
    }
    assert_eq!(served, [1, 1, 1, 1]);
}

#[test]
fn weighted_distribution_matches_relative_weights() {
    // A 1:3 split over a weight total of 4: residue 0 lands on the light
    // entry, residues 1 to 3 on the heavy one. The stream is deterministic,
    // so the exact 10:30 counts fail any uniform or first-entry sampler.
    let list = vec![
        choice("m/light", "low", Some(1)),
        choice("m/heavy", "low", Some(3)),
    ];
    let mut draw = Canned((0..40).map(|n| (n % 4) as u64).collect());
    let mut light = 0;
    let mut heavy = 0;
    for _ in 0..40 {
        match select(&list, &mut draw).unwrap().model.as_str() {
            "m/light" => light += 1,
            _ => heavy += 1,
        }
    }
    assert_eq!((light, heavy), (10, 30));
}

#[test]
fn unserved_band_remains_a_configuration_error() {
    let err = choose(&bridge(), "bridge", 0).unwrap_err();
    assert!(matches!(err, VivariumError::Config(_)), "got: {err}");

    let empty: PoolConfig = toml::from_str(
        "provider = \"local\"\nslots = 4\nbands.P1 = []\n[local]\nroot = \"~/lanes\"\n",
    )
    .unwrap();
    let err = choose(&empty, "empty", 1).unwrap_err();
    assert!(matches!(err, VivariumError::Config(_)), "got: {err}");
}

#[test]
fn an_oversized_band_list_is_refused_not_truncated() {
    let list: Vec<BandChoice> = (0..5)
        .map(|n| choice(&format!("m/{n}"), "low", None))
        .collect();
    let mut draw = Canned(vec![0]);
    let err = select(&list, &mut draw).unwrap_err().to_string();
    assert!(err.contains("holds 5 choices"), "got: {err}");
    assert!(err.contains("at most 4"), "got: {err}");
}

#[test]
fn an_entropy_failure_is_returned_not_panicked() {
    let list = vec![choice("m/a", "low", None), choice("m/b", "low", None)];
    let err = select(&list, &mut Dry).unwrap_err().to_string();
    assert!(err.contains("entropy"), "got: {err}");
}

#[test]
fn draws_above_the_unbiased_zone_are_rejected_and_redrawn() {
    let list = vec![choice("m/a", "low", None), choice("m/b", "low", None)];
    // Total 2 puts the unbiased zone one below u64::MAX: the top value is
    // rejected and the redraw lands on the first entry.
    let mut draw = Canned(vec![u64::MAX, 0]);
    assert_eq!(select(&list, &mut draw).unwrap().model, "m/a");
    // The top in-zone value still selects, without a redraw.
    let mut draw = Canned(vec![u64::MAX - 2]);
    assert_eq!(select(&list, &mut draw).unwrap().model, "m/b");
}

/// One band choice built from its TOML shape, weight folded into the
/// extension map exactly as configuration parses it.
fn choice(model: &str, reasoning: &str, weight: Option<u32>) -> BandChoice {
    let mut text = format!("model = \"{model}\"\nreasoning = \"{reasoning}\"");
    if let Some(weight) = weight {
        text.push_str(&format!("\nweight = {weight}"));
    }
    toml::from_str(&text).unwrap()
}

/// A draw source replaying canned u64 values in order, then repeating `0`.
struct Canned(Vec<u64>);

impl Draw for Canned {
    fn fill(&mut self, dest: &mut [u8]) -> Result<(), VivariumError> {
        let value = if self.0.is_empty() {
            0
        } else {
            self.0.remove(0)
        };
        dest.copy_from_slice(&value.to_be_bytes());
        Ok(())
    }
}

/// A draw source with no entropy at all: any consult fails.
struct Dry;

impl Draw for Dry {
    fn fill(&mut self, _: &mut [u8]) -> Result<(), VivariumError> {
        Err(VivariumError::Message(
            "the entropy source failed: dry".into(),
        ))
    }
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
    // Entries, not independent draws, are what a round trip must preserve.
    let pairs = |list: &[BandChoice]| {
        list.iter()
            .map(|c| (c.model.clone(), c.reasoning.clone(), c.weight()))
            .collect::<Vec<_>>()
    };
    for (key, choices) in &config.bands.entries {
        assert_eq!(
            pairs(choices),
            pairs(&again.bands.entries[key]),
            "band {key}"
        );
    }
}
