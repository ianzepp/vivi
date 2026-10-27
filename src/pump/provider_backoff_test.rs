//! Tests for [`super::provider_backoff`].

use std::collections::BTreeSet;

use super::provider_backoff::{
    AdmissionProviderView, AdmissionSnapshot, DEFAULT_ALLOWANCE, ProviderBucketState, ProviderKey,
    ProviderObservation, ProviderScope, configured_provider_ceiling, configured_provider_ceilings,
};
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
