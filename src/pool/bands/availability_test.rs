use super::*;

use chrono::TimeZone;

/// An instant `secs` seconds after a fixed epoch second.
fn at(secs: i64) -> chrono::DateTime<chrono::Utc> {
    chrono::Utc
        .timestamp_opt(1_700_000_000 + secs, 0)
        .single()
        .unwrap()
}

#[test]
fn the_default_view_has_every_model_available() {
    let view = Availability::default();
    assert!(view.capped_models.is_empty());
    assert!(view.provider_deadlines.is_empty());
    assert!(view.model_available("zai/glm-5.3-flash"));
    assert!(view.model_available("glm-5.3-flash"));
    assert_eq!(view.unavailable(["zai/glm-5.3-flash", "other/model"]), None);
}

#[test]
fn at_names_the_instant_the_view_answers_at() {
    let view = Availability::at(at(10));
    assert_eq!(view.now, at(10));
    assert!(view.capped_models.is_empty());
    assert!(view.provider_deadlines.is_empty());
}

#[test]
fn a_cap_blocks_its_own_slug_only() {
    let mut view = Availability::at(at(0));
    view.capped_models.insert("a/model".into());
    assert!(!view.model_available("a/model"));
    assert!(view.model_available("b/model"));
}

#[test]
fn a_cap_aggregates_as_model_cap_with_no_until() {
    let mut view = Availability::at(at(0));
    view.capped_models.insert("a/model".into());
    view.capped_models.insert("a/other".into());
    let gate = view
        .unavailable(["a/model", "a/other", "b/model"])
        .expect("cap blocks");
    assert_eq!(gate.reason, WaitingReason::ModelCap);
    assert_eq!(
        gate.blocked_models,
        vec!["a/model".to_string(), "a/other".to_string()]
    );
    assert_eq!(gate.until, None);
}

#[test]
fn a_provider_cooldown_blocks_every_slug_of_that_provider() {
    let mut view = Availability::at(at(0));
    view.provider_deadlines.insert("zai".into(), at(60));
    assert!(!view.model_available("zai/glm-5.3-flash"));
    assert!(!view.model_available("zai/glm-5.3"));
    assert!(view.model_available("other/model"));
}

#[test]
fn cooldown_only_blockers_aggregate_as_provider_cooldown() {
    let mut view = Availability::at(at(0));
    view.provider_deadlines.insert("zai".into(), at(60));
    let gate = view
        .unavailable(["zai/glm-5.3", "zai/glm-5.3-flash"])
        .expect("cooldown blocks");
    assert_eq!(gate.reason, WaitingReason::ProviderCooldown);
    assert_eq!(
        gate.blocked_models,
        vec!["zai/glm-5.3".to_string(), "zai/glm-5.3-flash".to_string()]
    );
    assert_eq!(gate.until, Some(at(60)));
}

#[test]
fn mixed_blockers_aggregate_as_models_unavailable_with_the_earliest_deadline() {
    let mut view = Availability::at(at(0));
    view.capped_models.insert("a/model".into());
    view.provider_deadlines.insert("zai".into(), at(60));
    view.provider_deadlines.insert("openai".into(), at(30));
    let gate = view
        .unavailable(["a/model", "zai/glm-5.3-flash", "openai/gpt-5.2", "ok/model"])
        .expect("mixed blockers");
    assert_eq!(gate.reason, WaitingReason::ModelsUnavailable);
    assert_eq!(
        gate.blocked_models,
        vec![
            "a/model".to_string(),
            "openai/gpt-5.2".to_string(),
            "zai/glm-5.3-flash".to_string()
        ]
    );
    assert_eq!(gate.until, Some(at(30)));
}

#[test]
fn a_deadline_at_now_is_available_and_one_before_now_is_not() {
    let mut view = Availability::at(at(60));
    view.provider_deadlines.insert("zai".into(), at(60));
    assert!(view.model_available("zai/glm-5.3-flash"));
    assert_eq!(view.unavailable(["zai/glm-5.3-flash"]), None);

    let mut before = Availability::at(at(59));
    before.provider_deadlines = view.provider_deadlines;
    assert!(!before.model_available("zai/glm-5.3-flash"));
    let gate = before
        .unavailable(["zai/glm-5.3-flash"])
        .expect("still cooling");
    assert_eq!(gate.until, Some(at(60)));
}

#[test]
fn an_expired_deadline_neither_blocks_nor_sets_until() {
    let mut view = Availability::at(at(1));
    view.provider_deadlines.insert("zai".into(), at(0));
    assert!(view.model_available("zai/glm-5.3-flash"));
    assert_eq!(view.unavailable(["zai/glm-5.3-flash"]), None);
}

#[test]
fn the_availability_metadata_round_trips_through_toml() {
    let mut view = Availability::at(at(0));
    view.capped_models.insert("a/model".into());
    view.provider_deadlines.insert("zai".into(), at(60));
    let text = toml::to_string(&view).unwrap();
    let round: Availability = toml::from_str(&text).unwrap();
    assert_eq!(round, view);
}

#[test]
fn the_unavailable_metadata_round_trips_through_toml() {
    let gate = Unavailable {
        blocked_models: vec!["zai/glm-5.3-flash".into()],
        reason: WaitingReason::ProviderCooldown,
        until: Some(at(60)),
    };
    let text = toml::to_string(&gate).unwrap();
    assert!(text.contains("provider_cooldown"));
    let round: Unavailable = toml::from_str(&text).unwrap();
    assert_eq!(round, gate);
}

#[test]
fn every_waiting_reason_round_trips_through_json() {
    for reason in [
        WaitingReason::ModelCap,
        WaitingReason::ProviderCooldown,
        WaitingReason::ModelsUnavailable,
        WaitingReason::AdmissionBusy,
    ] {
        let text = serde_json::to_string(&reason).unwrap();
        assert_eq!(
            serde_json::from_str::<WaitingReason>(&text).unwrap(),
            reason
        );
    }
    assert_eq!(
        serde_json::to_string(&WaitingReason::AdmissionBusy).unwrap(),
        "\"admission_busy\""
    );
}
