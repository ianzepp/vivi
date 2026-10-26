use super::*;

use crate::letter::work::TaskFront;
use crate::pool::slot::PoolId;
use rand_core::RngCore;

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

/// A pool whose P1 lists the given `(model, reasoning, weight)` entries.
fn banded(entries: &[(&str, &str, u32)]) -> PoolConfig {
    let mut text = String::from("provider = \"local\"\nslots = 4\n");
    for (model, reasoning, weight) in entries {
        text.push_str(&format!(
            "[[bands.P1]]\nmodel = \"{model}\"\nreasoning = \"{reasoning}\"\nweight = {weight}\n"
        ));
    }
    text.push_str("[local]\nroot = \"~/lanes\"\n");
    toml::from_str(&text).unwrap()
}

/// The whole model-and-reasoning pairs the pool lists for P1.
fn p1_entries(config: &PoolConfig) -> Vec<Choice> {
    config.bands.entries["P1"]
        .iter()
        .map(|entry| Choice {
            band: 1,
            model: entry.model.clone(),
            reasoning: entry.reasoning.clone(),
        })
        .collect()
}

/// A deterministic `rand_core` source: answers the given 64-bit words in
/// order, then cycles.
struct Fixed {
    words: Vec<u64>,
    at: usize,
}

impl Fixed {
    fn new(words: &[u64]) -> Self {
        Self {
            words: words.to_vec(),
            at: 0,
        }
    }

    fn word(&mut self) -> u64 {
        let word = self.words[self.at % self.words.len()];
        self.at += 1;
        word
    }
}

impl RngCore for Fixed {
    fn next_u32(&mut self) -> u32 {
        self.word() as u32
    }

    fn next_u64(&mut self) -> u64 {
        self.word()
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        for chunk in dest.chunks_mut(8) {
            let bytes = self.word().to_le_bytes();
            chunk.copy_from_slice(&bytes[..chunk.len()]);
        }
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        self.fill_bytes(dest);
        Ok(())
    }
}

/// The selection as a plain choice, failing the test on unavailability.
fn selected(config: &PoolConfig, words: &mut Injected<Fixed>) -> Choice {
    match choose_eligible(config, "bridge", 1, words, |_| None).unwrap() {
        Selection::Selected(choice) => choice,
        Selection::Unavailable(unavailable) => panic!("the band was unavailable: {unavailable:?}"),
    }
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
fn choose_draws_one_whole_entry_of_the_bands_list() {
    // Two equal-weight entries: the draw lands on one whole configured pair,
    // never a mix, and the single-entry band P2 keeps its existing choice.
    let config = bridge();
    let choice = choose(&config, "bridge", 1).unwrap();
    assert!(p1_entries(&config).contains(&choice), "got: {choice:?}");
    let choice = choose(&config, "bridge", 2).unwrap();
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
fn an_empty_band_list_still_names_the_configuration_error() {
    let config: PoolConfig = toml::from_str(
        "provider = \"local\"\nslots = 4\nbands.P3 = []\n[local]\nroot = \"~/lanes\"\n",
    )
    .unwrap();
    let err = choose(&config, "bridge", 3).unwrap_err().to_string();
    assert!(
        err.contains("pool bridge has no model for band P3"),
        "got: {err}"
    );
}

#[test]
fn equal_weights_distribute_across_all_four_entries() {
    let config = banded(&[
        ("one/model", "low", 1),
        ("two/model", "low", 1),
        ("three/model", "low", 1),
        ("four/model", "low", 1),
    ]);
    let mut words = Injected(Fixed::new(&[0, 1, 2, 3]));
    let picked: Vec<Choice> = (0..4).map(|_| selected(&config, &mut words)).collect();
    assert_eq!(picked, p1_entries(&config));
}

#[test]
fn weights_control_relative_distribution() {
    // 3:1: of the four tickets, three land on the weightier entry and one
    // on the lighter one.
    let config = banded(&[("heavy/model", "low", 3), ("light/model", "low", 1)]);
    let mut words = Injected(Fixed::new(&[0, 1, 2, 3]));
    for expected in ["heavy/model", "heavy/model", "heavy/model", "light/model"] {
        assert_eq!(selected(&config, &mut words).model, expected);
    }
}

#[test]
fn selection_preserves_model_reasoning_pairs() {
    // The same model listed at two efforts stays two choices: the middle
    // ticket carries the xhigh pair, not the low pair it sits next to.
    let config = banded(&[
        ("twin/model", "low", 1),
        ("twin/model", "xhigh", 1),
        ("other/model", "low", 1),
    ]);
    let mut words = Injected(Fixed::new(&[0, 1, 2]));
    assert_eq!(
        selected(&config, &mut words),
        Choice {
            band: 1,
            model: "twin/model".into(),
            reasoning: "low".into()
        }
    );
    assert_eq!(
        selected(&config, &mut words),
        Choice {
            band: 1,
            model: "twin/model".into(),
            reasoning: "xhigh".into()
        }
    );
    assert_eq!(
        selected(&config, &mut words),
        Choice {
            band: 1,
            model: "other/model".into(),
            reasoning: "low".into()
        }
    );
}

#[test]
fn single_entry_preserves_existing_choice() {
    let config = banded(&[("only/model", "medium", 1)]);
    for draw in [0u64, 7, 12345] {
        let mut words = Injected(Fixed::new(&[draw]));
        assert_eq!(
            selected(&config, &mut words),
            Choice {
                band: 1,
                model: "only/model".into(),
                reasoning: "medium".into()
            }
        );
    }
    let choice = choose(&config, "bridge", 1).unwrap();
    assert_eq!(choice.model, "only/model");
    assert_eq!(choice.reasoning, "medium");
}

#[test]
fn maximum_weight_total_does_not_overflow() {
    // Four choices at u32::MAX: summed in u64 the total cannot overflow,
    // and tickets at either end of the range still pick whole entries — a
    // wrapped u32 total would send the last ticket elsewhere.
    let max = u32::MAX;
    let config = banded(&[
        ("first/model", "low", max),
        ("second/model", "low", max),
        ("third/model", "low", max),
        ("fourth/model", "low", max),
    ]);
    let total = u64::from(max) * 4;
    for (draw, expected) in [
        (total - 1, "fourth/model"),
        (0, "first/model"),
        (total / 2, "third/model"),
    ] {
        let mut words = Injected(Fixed::new(&[draw]));
        assert_eq!(selected(&config, &mut words).model, expected);
    }
}

/// A word source whose entropy acquisition fails, as the operating system's
/// might.
struct NoEntropy;

impl RandomWords for NoEntropy {
    fn word(&mut self) -> Result<u64, VivariumError> {
        Err(VivariumError::Other(
            "the operating system served no entropy".into(),
        ))
    }
}

#[test]
fn a_failed_entropy_acquisition_is_an_error_not_a_panic() {
    let err = choose_eligible(&bridge(), "bridge", 1, &mut NoEntropy, |_| None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("entropy"), "got: {err}");
}

#[test]
fn an_excluded_model_leaves_the_rest_of_the_band() {
    let config = banded(&[("out/model", "low", 3), ("in/model", "high", 1)]);
    let mut words = Injected(Fixed::new(&[0, 9, 77, 1]));
    for _ in 0..4 {
        let choice = choose_eligible(&config, "bridge", 1, &mut words, |model| {
            (model == "out/model").then(|| Unavailable {
                reason: Reason::Capacity,
                retry_at: Some(10),
            })
        })
        .unwrap();
        assert_eq!(
            choice,
            Selection::Selected(Choice {
                band: 1,
                model: "in/model".into(),
                reasoning: "high".into(),
            })
        );
    }
}

#[test]
fn the_soonest_exclusion_carries_the_reason_and_deadline() {
    let mut words = Injected(Fixed::new(&[0]));
    let cooldowns = [
        ("zai/glm-5.3-flash", Some(100u64)),
        ("other/model", Some(50)),
    ];
    let answer = choose_eligible(&bridge(), "bridge", 1, &mut words, |model| {
        cooldowns
            .iter()
            .find(|(name, _)| *name == model)
            .and_then(|(_, lifts)| {
                lifts.map(|at| Unavailable {
                    reason: Reason::ProviderCooldown,
                    retry_at: Some(at),
                })
            })
    })
    .unwrap();
    assert_eq!(
        answer,
        Selection::Unavailable(Unavailable {
            reason: Reason::ProviderCooldown,
            retry_at: Some(50),
        })
    );
}

#[test]
fn a_known_deadline_beats_an_unknown_one() {
    let mut words = Injected(Fixed::new(&[0]));
    let answer = choose_eligible(&bridge(), "bridge", 1, &mut words, |model| {
        if model == "other/model" {
            Some(Unavailable {
                reason: Reason::Capacity,
                retry_at: Some(70),
            })
        } else {
            Some(Unavailable {
                reason: Reason::AdmissionBusy,
                retry_at: None,
            })
        }
    })
    .unwrap();
    assert_eq!(
        answer,
        Selection::Unavailable(Unavailable {
            reason: Reason::Capacity,
            retry_at: Some(70),
        })
    );
}

#[test]
fn exclusions_without_a_deadline_stay_retry_less() {
    let mut words = Injected(Fixed::new(&[0]));
    let answer = choose_eligible(&bridge(), "bridge", 1, &mut words, |model| {
        Some(Unavailable {
            reason: if model == "other/model" {
                Reason::Capacity
            } else {
                Reason::AdmissionBusy
            },
            retry_at: None,
        })
    })
    .unwrap();
    // The fold is stable: list order breaks the tie, no deadline invented.
    assert_eq!(
        answer,
        Selection::Unavailable(Unavailable {
            reason: Reason::AdmissionBusy,
            retry_at: None,
        })
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
    let config = bridge();
    let text = toml::to_string(&config).unwrap();
    let again: PoolConfig = toml::from_str(&text).unwrap();
    again.validate(&PoolId::new("space", "", "main")).unwrap();

    // The same P1 entries survive the round trip, weights included:
    let pairs = |config: &PoolConfig| -> Vec<(String, String, u32)> {
        config.bands.entries["P1"]
            .iter()
            .map(|entry| (entry.model.clone(), entry.reasoning.clone(), entry.weight))
            .collect()
    };
    assert_eq!(pairs(&again), pairs(&config));

    // Driven by the same injected words, both sides choose alike — never
    // two independent random draws:
    let mut left = Injected(Fixed::new(&[1]));
    let mut right = Injected(Fixed::new(&[1]));
    assert_eq!(
        choose_eligible(&again, "bridge", 1, &mut left, |_| None).unwrap(),
        choose_eligible(&config, "bridge", 1, &mut right, |_| None).unwrap()
    );
}
