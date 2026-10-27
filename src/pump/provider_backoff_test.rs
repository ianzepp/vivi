//! Tests for [`super::provider_backoff`].

use std::collections::BTreeSet;

use super::provider_backoff::{
    AdmissionProviderView, AdmissionSnapshot, DEFAULT_ALLOWANCE, ProviderBucketMap,
    ProviderBucketState, ProviderBuckets, ProviderKey, ProviderObservation, ProviderScope,
    admits_fresh_start, clamp_to_capacity, configured_provider_ceiling,
    configured_provider_ceilings, effective_allowance, observation_is_distinct, record_rate_limit,
    recover_one_slot,
};
use super::transient::jitter;
use crate::pool::config::PoolConfig;
use crate::pool::slot::PoolId;

fn set(models: &[&str]) -> BTreeSet<String> {
    models.iter().map(|m| (*m).to_string()).collect()
}

/// A pool config parsed from a narrow TOML table, so tests name only the
/// fields they mean.
fn pool(provider: &str, slots: u32, models: &[&str], caps: &str) -> PoolConfig {
    let listed = models
        .iter()
        .map(|m| format!("\"{m}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let mut text = format!(
        "provider = \"{provider}\"\nslots = {slots}\n\n[harness]\nname = \"codex\"\nrun = \"run {{{{model}}}}\"\nmodels = [{listed}]\n"
    );
    if !caps.is_empty() {
        text.push_str("\n[caps]\n");
        text.push_str(caps);
        text.push('\n');
    }
    toml::from_str(&text).unwrap()
}

#[test]
fn provider_key_groups_models_not_compute_adapters() {
    // Qualified slugs share one namespace bucket.
    let flash = ProviderKey::from_model("zai/glm-5.3-flash");
    let plain = ProviderKey::from_model("zai/glm-5.3");
    assert_eq!(flash, plain);
    assert_eq!(flash.namespace(), Some("zai"));

    // A bare slug is opaque and never groups with a qualified namespace of
    // the same spelling.
    let bare = ProviderKey::from_model("glm-5.3");
    let qualified = ProviderKey::from_model("glm-5.3/turbo");
    assert_ne!(bare, qualified);
    assert_eq!(bare.namespace(), None);
    assert_eq!(qualified.namespace(), Some("glm-5.3"));

    // The compute adapter (`PoolConfig.provider`) and its machine settings
    // never change an identity or a ceiling, and credentials are never
    // consulted: same slots, models and caps, different host, same answer.
    let fly = pool(
        "fly",
        4,
        &["zai/glm-5.3-flash", "zai/glm-5.3", "glm-5.3"],
        "",
    );
    let docker = pool(
        "docker",
        4,
        &["zai/glm-5.3-flash", "zai/glm-5.3", "glm-5.3"],
        "",
    );
    assert_eq!(
        configured_provider_ceilings(&fly),
        configured_provider_ceilings(&docker)
    );
    // A compute adapter name is never a provider key.
    for adapter in ["fly", "docker", "ssh", "local", "runpod"] {
        assert_ne!(flash, ProviderKey::from_model(adapter));
        assert_ne!(plain, ProviderKey::from_model(adapter));
    }
}

#[test]
fn scope_is_mailspace_and_pool_never_target() {
    let id = PoolId::new("space", "018f-uuid", "main");
    let scope = ProviderScope::from(&id);
    assert_eq!(scope.mailspace, "018f-uuid");
    assert_eq!(scope.pool, "main");
    // Two jobs with different repository targets in one scope share it.
    assert_eq!(
        ProviderScope::from(&id),
        ProviderScope::new("018f-uuid", "main")
    );
    assert_ne!(scope, ProviderScope::new("018f-uuid", "fallback"));
    assert_eq!(scope.to_string(), "018f-uuid/main");
}

#[test]
fn ceiling_counts_distinct_offerings_once() {
    // The same slug listed twice is offered once.
    let duplicated = pool("fly", 4, &["zai/glm-5.3", "zai/glm-5.3", "glm-5.3"], "");
    let ceilings = configured_provider_ceilings(&duplicated);
    assert_eq!(ceilings.len(), 2);
    let zai = ProviderKey::from_model("zai/glm-5.3");
    let bare = ProviderKey::from_model("glm-5.3");
    assert_eq!(ceilings[&zai], 4);
    assert_eq!(ceilings[&bare], 4);
    // The whole pool still admits at most its slots.
    assert_eq!(
        configured_provider_ceiling(4, &set(&["zai/glm-5.3", "glm-5.3"]), &duplicated.caps),
        4
    );
}

#[test]
fn ceiling_uncapped_models_ride_pool_slots() {
    // Every uncapped model contributes the pool's slots; the sum binds.
    let models = set(&["zai/glm-5.3", "bare/model", "other"]);
    assert_eq!(
        configured_provider_ceiling(3, &models, &Default::default()),
        3
    );
    let uncapped = pool("fly", 3, &["zai/glm-5.3", "glm-5.3"], "");
    assert_eq!(
        configured_provider_ceiling(3, &set(&["zai/glm-5.3", "glm-5.3"]), &uncapped.caps),
        3
    );
    // One capped model alone may sit under the slots.
    let capped_low = pool("fly", 5, &["zai/glm-5.3"], "\"zai/glm-5.3\" = 3\n");
    assert_eq!(
        configured_provider_ceiling(5, &set(&["zai/glm-5.3"]), &capped_low.caps),
        3
    );
}

#[test]
fn ceiling_zero_capacity_stays_zero() {
    // No offered models: zero provider capacity, however large the pool.
    assert_eq!(
        configured_provider_ceiling(4, &set(&[]), &Default::default()),
        0
    );
    // Explicit zero caps keep the model disabled; capacity stays zero.
    let disabled = pool("fly", 4, &["zai/glm-5.3"], "\"zai/glm-5.3\" = 0\n");
    assert_eq!(
        configured_provider_ceiling(4, &set(&["zai/glm-5.3"]), &disabled.caps),
        0
    );
    let ceilings = configured_provider_ceilings(&disabled);
    assert_eq!(ceilings[&ProviderKey::from_model("zai/glm-5.3")], 0);
    // Zero pool slots bound everything, too.
    assert_eq!(
        configured_provider_ceiling(0, &set(&["zai/glm-5.3"]), &Default::default()),
        0
    );
}

#[test]
fn ceiling_saturates_then_binds_to_slots() {
    // A saturating sum that overflows u32 must not panic, and still binds.
    let huge = u32::MAX;
    let models = set(&["zai/glm-5.3", "glm-5.3", "other"]);
    assert_eq!(
        configured_provider_ceiling(huge, &models, &Default::default()),
        huge
    );
    // Capped shares above the slots bind down.
    let capped = pool("fly", 2, &["zai/glm-5.3"], "\"zai/glm-5.3\" = 999\n");
    assert_eq!(
        configured_provider_ceiling(2, &set(&["zai/glm-5.3"]), &capped.caps),
        2
    );
}

#[test]
fn bucket_state_defaults_and_cools() {
    let fresh = ProviderBucketState::new();
    assert_eq!(fresh.allowance, DEFAULT_ALLOWANCE);
    assert!(!fresh.cooling(0));
    let cooling = ProviderBucketState {
        cooldown_until: Some(100),
        recover_at: Some(200),
        ..ProviderBucketState::default()
    };
    assert!(cooling.cooling(99));
    assert!(!cooling.cooling(100));
}

#[test]
fn bucket_state_survives_json_roundtrip() {
    let state = ProviderBucketState {
        allowance: 2.5,
        observation: Some(ProviderObservation {
            at: Some(12),
            kind: "refusal".into(),
            detail: "429 too many requests".into(),
        }),
        cooldown_until: Some(60),
        recover_at: None,
        retry_step: 2,
    };
    let text = serde_json::to_string(&state).unwrap();
    let back: ProviderBucketState = serde_json::from_str(&text).unwrap();
    assert_eq!(back, state);
    // A partial older record still loads with defaults.
    let old: ProviderBucketState = serde_json::from_str("{}").unwrap();
    assert_eq!(old.allowance, DEFAULT_ALLOWANCE);
}

#[test]
fn nested_buckets_and_snapshot_roundtrip_through_json() {
    let mut buckets = super::provider_backoff::ProviderBuckets::new();
    buckets.insert(
        "main".into(),
        [(
            ProviderKey::from_model("zai/glm-5.3"),
            ProviderBucketState::new(),
        )]
        .into_iter()
        .collect(),
    );
    let text = serde_json::to_string(&buckets).unwrap();
    let back: super::provider_backoff::ProviderBuckets = serde_json::from_str(&text).unwrap();
    assert_eq!(back, buckets);

    let snapshot = AdmissionSnapshot {
        scope: ProviderScope::new("space", "main"),
        taken_at: 7,
        providers: [(
            ProviderKey::from_model("zai/glm-5.3"),
            AdmissionProviderView {
                ceiling: 4,
                in_flight: 1,
                allowance: 2.0,
                cooling: false,
            },
        )]
        .into_iter()
        .collect(),
    };
    let text = serde_json::to_string(&snapshot).unwrap();
    let back: AdmissionSnapshot = serde_json::from_str(&text).unwrap();
    assert_eq!(back, snapshot);
}

/// Assert one bucket is still factory-fresh: nothing was learned there.
fn assert_untouched(state: &ProviderBucketState) {
    assert_eq!(state.allowance, DEFAULT_ALLOWANCE);
    assert_eq!(state.observation, None);
    assert_eq!(state.cooldown_until, None);
    assert_eq!(state.recover_at, None);
    assert_eq!(state.retry_step, 0);
}

#[test]
fn rate_limit_halves_the_effective_provider_limit() {
    let seed = "zai/glm-5.3";
    let spread = jitter(seed);
    assert!(spread <= 14, "jitter is bounded at 14 extra seconds");
    // A bucket whose allowance has recovered to four slots.
    let mut bucket = ProviderBucketState::new();
    bucket.allowance = 4.0;

    // First distinct limit: four halves to exactly two, the cooldown
    // earns the 60-second exponential base plus the deterministic jitter,
    // and the quiet-recovery deadline moves to now + 300.
    assert!(record_rate_limit(
        &mut bucket,
        1_000,
        "limit",
        "429 too many requests",
        seed,
        4
    ));
    assert_eq!(bucket.allowance, 2.0);
    assert_eq!(bucket.cooldown_until, Some(1_000 + 60 + spread));
    assert_eq!(bucket.recover_at, Some(1_300));

    // A different detail is a distinct observation and halves again; the
    // second consecutive observation doubles the base to 120.
    assert!(record_rate_limit(
        &mut bucket,
        1_100,
        "refusal",
        "overloaded",
        seed,
        4
    ));
    assert_eq!(bucket.allowance, 1.0);
    assert_eq!(bucket.cooldown_until, Some(1_100 + 120 + spread));

    // The floor of one holds for enabled capacity, however many distinct
    // limits follow: minimum-limit learning stops at one slot.
    assert!(record_rate_limit(
        &mut bucket,
        1_200,
        "refusal",
        "quota exhausted",
        seed,
        4
    ));
    assert!(record_rate_limit(
        &mut bucket,
        1_300,
        "refusal",
        "hard cap",
        seed,
        4
    ));
    assert_eq!(bucket.allowance, 1.0);
    // Successive observations keep doubling the base: 240, then 480, all
    // exact and still short of the 900-second base ceiling.
    assert_eq!(bucket.cooldown_until, Some(1_300 + 480 + spread));
}

#[test]
fn quiet_period_recovers_one_slot_at_a_time() {
    let mut bucket = ProviderBucketState::new();
    assert!(record_rate_limit(
        &mut bucket,
        1_000,
        "limit",
        "429",
        "quiet",
        4
    ));
    assert_eq!(bucket.allowance, 1.0);
    assert_eq!(bucket.recover_at, Some(1_300));

    // One second short of the quiet interval: nothing moves.
    assert!(!recover_one_slot(&mut bucket, 1_299, 4));
    assert_eq!(bucket.allowance, 1.0);
    assert_eq!(bucket.recover_at, Some(1_300));

    // Exactly at the deadline: exactly one slot, the next interval
    // counted from now.
    assert!(recover_one_slot(&mut bucket, 1_300, 4));
    assert_eq!(bucket.allowance, 2.0);
    assert_eq!(bucket.recover_at, Some(1_600));

    // Missed downtime never replays: four intervals pass at once and
    // still exactly one slot is recovered, with the next interval from
    // now, not from the long-missed deadline.
    assert!(recover_one_slot(&mut bucket, 2_500, 4));
    assert_eq!(bucket.allowance, 3.0);
    assert_eq!(bucket.recover_at, Some(2_800));

    // The last increment reaches the ceiling; past it recovery stops and
    // the deadline stops moving.
    assert!(recover_one_slot(&mut bucket, 2_800, 4));
    assert_eq!(bucket.allowance, 4.0);
    assert!(!recover_one_slot(&mut bucket, 9_999, 4));
    assert_eq!(bucket.recover_at, Some(3_100));
}

#[test]
fn repeated_limits_reset_quiet_time_and_bound_delay() {
    let seed = "repeat";
    let spread = jitter(seed);
    let mut bucket = ProviderBucketState::new();
    assert!(record_rate_limit(
        &mut bucket,
        1_000,
        "limit",
        "429",
        seed,
        4
    ));
    let first = bucket.cooldown_until;
    assert_eq!(first, Some(1_060 + spread));

    // The same observation again: not distinct, so no further halving,
    // but quiet time resets and the cooldown extends on the doubled base.
    assert!(!observation_is_distinct(
        bucket.observation.as_ref(),
        "limit",
        "429"
    ));
    assert!(!record_rate_limit(
        &mut bucket,
        1_100,
        "limit",
        "429",
        seed,
        4
    ));
    assert_eq!(bucket.allowance, 1.0);
    assert_eq!(bucket.recover_at, Some(1_400));
    assert_eq!(bucket.cooldown_until, Some(1_220 + spread));
    assert!(bucket.cooldown_until.unwrap() >= first.unwrap());

    // A cooldown already earned further out is never shortened by a new
    // observation whose computed delay lands earlier.
    let mut far = ProviderBucketState::new();
    far.cooldown_until = Some(5_000);
    assert!(record_rate_limit(&mut far, 1_000, "limit", "429", seed, 4));
    assert_eq!(far.cooldown_until, Some(5_000));

    // Deduplication inputs stay explicit, and only what was seen decides:
    // a repeat at a later second is the same observation; a different
    // detail or kind is not.
    let seen = ProviderObservation {
        at: Some(7),
        kind: "limit".into(),
        detail: "429".into(),
    };
    assert!(observation_is_distinct(None, "limit", "429"));
    assert!(!observation_is_distinct(Some(&seen), "limit", "429"));
    assert!(observation_is_distinct(Some(&seen), "limit", "500"));
    assert!(observation_is_distinct(Some(&seen), "refusal", "429"));

    // Every spread is bounded: any seed adds at most 14 seconds.
    for probe in ["a", "bb", "ccc", "zai/glm-5.3"] {
        assert!(jitter(probe) <= 14);
    }
}

#[test]
fn recovery_respects_reloaded_caps_and_zero_capacity() {
    // A learned allowance of four meets a reloaded, tighter config.
    let mut bucket = ProviderBucketState::new();
    bucket.allowance = 4.0;
    clamp_to_capacity(&mut bucket, 2);
    assert_eq!(bucket.allowance, 2.0);
    // A later reload raises the ceiling: the learned reduction is not
    // erased, and admission still sees only the learned two.
    clamp_to_capacity(&mut bucket, 8);
    assert_eq!(bucket.allowance, 2.0);
    assert_eq!(effective_allowance(&bucket, 8), 2.0);

    // A cooldown blocks every fresh start, whatever the allowance; once
    // it passes, admission counts in-flight against the effective
    // allowance — and the policy never cancels running jobs.
    bucket.cooldown_until = Some(5_000);
    assert!(!admits_fresh_start(&bucket, 4_999, 0, 8));
    assert!(admits_fresh_start(&bucket, 5_000, 1, 8));
    assert!(!admits_fresh_start(&bucket, 5_000, 2, 8));

    // Zero capacity: a limit floors the allowance at zero, not one, and
    // a disabled provider never recovers a slot.
    let mut disabled = ProviderBucketState::new();
    assert!(record_rate_limit(
        &mut disabled,
        1_000,
        "limit",
        "429",
        "off",
        0
    ));
    assert_eq!(disabled.allowance, 0.0);
    assert_eq!(disabled.cooldown_until, Some(1_060 + jitter("off")));
    assert!(!recover_one_slot(&mut disabled, 9_999, 0));
    assert_eq!(disabled.allowance, 0.0);
}

#[test]
fn pools_and_providers_keep_isolated_buckets() {
    let zai = ProviderKey::from_model("zai/glm-5.3");
    let bare = ProviderKey::from_model("glm-5.3");
    let mut pools = ProviderBuckets::new();
    for name in ["main", "fallback"] {
        let mut map = ProviderBucketMap::new();
        map.insert(zai.clone(), ProviderBucketState::new());
        map.insert(bare.clone(), ProviderBucketState::new());
        pools.insert(name.into(), map);
    }
    // A limit learned in one pool's provider bucket stays there.
    let limited = pools.get_mut("main").and_then(|m| m.get_mut(&zai)).unwrap();
    assert!(record_rate_limit(limited, 1_000, "limit", "429", "iso", 4));
    assert_untouched(&pools["main"][&bare]);
    assert_untouched(&pools["fallback"][&zai]);
    assert_untouched(&pools["fallback"][&bare]);
    assert!(pools["main"][&zai].cooldown_until.is_some());
}
