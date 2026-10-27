use super::*;
use rand_core::RngCore;

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

/// A pool with four equally weighted choices for `P1` and one for `P2`.
fn four_bands() -> PoolConfig {
    toml::from_str(
        r#"
provider = "local"
slots = 4
[[bands.P1]]
model = "a/one"
reasoning = "low"
[[bands.P1]]
model = "a/two"
reasoning = "low"
[[bands.P1]]
model = "a/three"
reasoning = "low"
[[bands.P1]]
model = "a/four"
reasoning = "low"
[[bands.P2]]
model = "a/solo"
reasoning = "low"
[local]
root = "~/lanes"
"#,
    )
    .unwrap()
}

/// A pool whose two `P1` choices carry weights 1 and 3.
fn weighted_bands() -> PoolConfig {
    toml::from_str(
        r#"
provider = "local"
slots = 4
[[bands.P1]]
model = "a/light"
reasoning = "low"
weight = 1
[[bands.P1]]
model = "a/heavy"
reasoning = "low"
weight = 3
[local]
root = "~/lanes"
"#,
    )
    .unwrap()
}

/// A splitmix64 stream: deterministic, uniform enough for distribution
/// counts, and the same on every run.
struct SplitMix(u64);

impl RngCore for SplitMix {
    fn next_u32(&mut self) -> u32 {
        (self.next_u64() >> 32) as u32
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        rand_core::impls::fill_bytes_via_next(self, dest);
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        self.fill_bytes(dest);
        Ok(())
    }
}

/// A source that replays a scripted list of `u64` draws, one per eight
/// bytes the sampler asks for.
struct Scripted(Vec<u64>);

impl RngCore for Scripted {
    fn next_u32(&mut self) -> u32 {
        (self.next_u64() >> 32) as u32
    }

    fn next_u64(&mut self) -> u64 {
        if self.0.is_empty() {
            panic!("the scripted draws ran dry");
        }
        self.0.remove(0)
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        rand_core::impls::fill_bytes_via_next(self, dest);
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        self.fill_bytes(dest);
        Ok(())
    }
}

/// A source whose entropy is gone: every draw fails.
struct Dry;

impl RngCore for Dry {
    fn next_u32(&mut self) -> u32 {
        unreachable!("the sampler only asks try_fill_bytes")
    }

    fn next_u64(&mut self) -> u64 {
        unreachable!("the sampler only asks try_fill_bytes")
    }

    fn fill_bytes(&mut self, _dest: &mut [u8]) {
        unreachable!("the sampler only asks try_fill_bytes")
    }

    fn try_fill_bytes(&mut self, _dest: &mut [u8]) -> Result<(), rand_core::Error> {
        Err(rand_core::Error::from(
            std::num::NonZeroU32::new(rand_core::Error::CUSTOM_START + 1).unwrap(),
        ))
    }
}

#[test]
fn choose_returns_a_complete_pair_sampled_from_the_bands_list() {
    let choice = choose(&bridge(), "bridge", 1).unwrap();
    assert_eq!(choice.band, 1);
    assert!(
        [("zai/glm-5.3-flash", "xhigh"), ("other/model", "low"),]
            .contains(&(choice.model.as_str(), choice.reasoning.as_str())),
        "got {} at {}",
        choice.model,
        choice.reasoning
    );
    let choice = choose(&bridge(), "bridge", 2).unwrap();
    assert_eq!(choice.model, "zai/glm-5.3-flash");
    assert_eq!(choice.reasoning, "low");
}

#[test]
fn single_entry_preserves_pair_without_randomness() {
    let config = bridge();
    let choice = sample_pair(&config, "bridge", 2, &mut Dry).unwrap();
    assert_eq!(
        choice,
        Choice {
            band: 2,
            model: "zai/glm-5.3-flash".into(),
            reasoning: "low".into(),
        }
    );
}

#[test]
fn equal_weights_distribute_across_all_four_entries() {
    let list = four_bands().bands.entries.get("P1").unwrap().clone();
    let mut counts = [0usize; 4];
    let mut rng = SplitMix(42);
    for _ in 0..4000 {
        let model = &sample("P1", &list, &mut rng).unwrap().model;
        let slot = list.iter().position(|c| &c.model == model).unwrap();
        counts[slot] += 1;
    }
    for (n, count) in counts.iter().enumerate() {
        assert!(*count > 0, "entry {n} was never sampled: {:?}", counts);
        assert!(
            (800..1200).contains(count),
            "entry {n} took {count} of 4000: {:?}",
            counts
        );
    }
}

#[test]
fn weighted_distribution_matches_relative_weights() {
    let list = weighted_bands().bands.entries.get("P1").unwrap().clone();
    // With weights 1:3 the weight total is 4, so draws 0 and 1 straddle
    // the boundary between the light and the heavy entry.
    let mut rng = Scripted(vec![0]);
    assert_eq!(sample("P1", &list, &mut rng).unwrap().model, "a/light");
    let mut rng = Scripted(vec![1]);
    assert_eq!(sample("P1", &list, &mut rng).unwrap().model, "a/heavy");
    let mut light = 0;
    let mut rng = SplitMix(7);
    for _ in 0..4000 {
        if sample("P1", &list, &mut rng).unwrap().model == "a/light" {
            light += 1;
        }
    }
    // 1:3 over 4000 draws is 1000/3000; a weight-ignoring pick would sit
    // near 2000/2000, far outside both windows.
    assert!((850..=1150).contains(&light), "light took {light} of 4000");
    assert!(
        (2850..=3150).contains(&(4000 - light)),
        "heavy took {} of 4000",
        4000 - light
    );
}

#[test]
fn a_rejected_draw_is_redrawn_not_reduced() {
    let list = four_bands().bands.entries.get("P1").unwrap().clone();
    let total = 4u64;
    let zone = u64::MAX - u64::MAX % total;
    // zone - 1 is the last acceptable draw: it lands on the last entry.
    let mut rng = Scripted(vec![zone - 1]);
    assert_eq!(sample("P1", &list, &mut rng).unwrap().model, "a/four");
    // zone itself is the first rejected draw: it is redrawn, never
    // reduced modulo the total.
    let mut rng = Scripted(vec![zone, 0]);
    assert_eq!(sample("P1", &list, &mut rng).unwrap().model, "a/one");
}

#[test]
fn an_entropy_failure_is_an_error_not_a_panic() {
    let list = bridge().bands.entries.get("P1").unwrap().clone();
    let err = sample("P1", &list, &mut Dry).unwrap_err().to_string();
    assert!(err.contains("randomness"), "got: {err}");
}

#[test]
fn an_oversized_list_is_an_error_not_a_truncation() {
    let mut oversized = four_bands().bands.entries.get("P1").unwrap().clone();
    oversized.push(BandChoice {
        model: "a/five".into(),
        reasoning: "low".into(),
        unknown: Default::default(),
    });
    let err = sample("P1", &oversized, &mut SplitMix(1))
        .unwrap_err()
        .to_string();
    assert!(err.contains("holds 5 choices"), "got: {err}");
    assert!(err.contains("at most 4"), "got: {err}");
}

#[test]
fn unserved_band_remains_a_configuration_error() {
    let config: PoolConfig = toml::from_str(
        "provider = \"local\"\nslots = 4\nbands = { P3 = [] }\n[local]\nroot = \"~/lanes\"\n",
    )
    .unwrap();
    let err = choose(&config, "bare", 3).unwrap_err();
    assert!(matches!(err, VivariumError::Config(_)), "got: {err}");
    let text = err.to_string();
    assert!(
        text.contains("pool bare has no model for band P3"),
        "got: {text}"
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
    for (key, choices) in &config.bands.entries {
        let round = &again.bands.entries[key];
        assert_eq!(choices.len(), round.len(), "band {key} changed size");
        for (a, b) in choices.iter().zip(round) {
            assert_eq!(a.model, b.model);
            assert_eq!(a.reasoning, b.reasoning);
            assert_eq!(a.weight(), b.weight());
        }
    }
}
