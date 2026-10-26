use super::*;

use crate::pool::config::PoolConfig;
use chrono::TimeZone;

/// An instant `secs` seconds after a fixed epoch second.
fn at(secs: i64) -> chrono::DateTime<chrono::Utc> {
    chrono::Utc
        .timestamp_opt(1_700_000_000 + secs, 0)
        .single()
        .unwrap()
}

#[test]
fn a_qualified_slug_keys_to_the_component_before_the_first_slash() {
    assert_eq!(provider_key("zai/glm-5.3-flash"), "zai");
    assert_eq!(provider_key("openai/gpt-5.2"), "openai");
    assert_eq!(provider_key("a/b/c"), "a");
}

#[test]
fn an_unqualified_slug_keys_to_itself() {
    assert_eq!(provider_key("glm-5.3-flash"), "glm-5.3-flash");
}

#[test]
fn slugs_of_one_provider_share_a_key() {
    assert_eq!(
        provider_key("zai/glm-5.3-flash"),
        provider_key("zai/glm-5.3")
    );
}

#[test]
fn the_provider_key_is_never_the_pools_configured_provider() {
    let config: PoolConfig = toml::from_str(
        "provider = \"local\"\nslots = 1\n[[bands.P1]]\nmodel = \"zai/glm-5.3-flash\"\nreasoning = \"low\"\n[local]\nroot = \"~/lanes\"\n",
    )
    .unwrap();
    let model = &config.bands.entries["P1"][0].model;
    assert_ne!(provider_key(model), config.provider);
    assert_eq!(provider_key(model), "zai");
}

#[test]
fn the_default_cooldown_is_a_minute() {
    assert_eq!(COOLDOWN_SECS, 60);
}

#[test]
fn the_collected_note_key_is_namespaced_away_from_plain_result_fields() {
    assert_eq!(COLLECTED_NOTE_KEY, "pool.health");
    for plain in ["job", "pool", "slot", "outcome", "read_only"] {
        assert_ne!(COLLECTED_NOTE_KEY, plain);
    }
}

#[test]
fn an_observation_cools_its_provider_for_the_default_cooldown() {
    let seen = HealthObservation::at("zai", Classification::Quota, at(0));
    assert_eq!(seen.provider, "zai");
    assert_eq!(seen.classification, Classification::Quota);
    assert_eq!(seen.observed_at, at(0));
    assert_eq!(seen.cooldown_until, at(COOLDOWN_SECS as i64));
}

#[test]
fn an_observation_round_trips_through_toml() {
    let seen = HealthObservation::at("zai", Classification::RateLimit, at(0));
    let text = toml::to_string(&seen).unwrap();
    assert!(text.contains("rate_limit"));
    let round: HealthObservation = toml::from_str(&text).unwrap();
    assert_eq!(round, seen);
}
