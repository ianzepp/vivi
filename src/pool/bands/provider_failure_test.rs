use super::*;

use crate::pool::bands::reservations::ReservationRow;

#[test]
fn a_qualified_model_keys_to_the_prefix_before_the_first_slash() {
    assert_eq!(provider_key("zai/glm-5.3-flash"), "zai");
    assert_eq!(provider_key("a/b/c"), "a");
}

#[test]
fn an_unqualified_model_is_its_own_provider() {
    assert_eq!(provider_key("glm-5.3-flash"), "glm-5.3-flash");
    assert_eq!(provider_key(""), "");
}

#[test]
fn rate_limit_cools_down_for_60_seconds_and_quota_for_300() {
    assert_eq!(FailureClass::RateLimit.cooldown_secs(), 60);
    assert_eq!(FailureClass::Quota.cooldown_secs(), 300);
}

#[test]
fn failure_classes_round_trip_through_their_codes() {
    for (class, code) in [
        (FailureClass::RateLimit, "rate-limit"),
        (FailureClass::Quota, "quota"),
    ] {
        assert_eq!(
            serde_json::to_string(&class).unwrap(),
            format!("\"{code}\"")
        );
        assert_eq!(
            serde_json::from_str::<FailureClass>(&format!("\"{code}\"")).unwrap(),
            class
        );
    }
}

#[test]
fn a_cooldown_round_trips_through_json() {
    let cooldown = ProviderCooldown {
        provider: "zai".into(),
        class: FailureClass::RateLimit,
        observed_at: 1_000,
        expires_at: 1_000 + FailureClass::RateLimit.cooldown_secs(),
    };
    let text = serde_json::to_string(&cooldown).unwrap();
    assert_eq!(
        serde_json::from_str::<ProviderCooldown>(&text).unwrap(),
        cooldown
    );
}

#[test]
fn a_reservation_row_round_trips_through_json() {
    // reservations.rs ships without a companion of its own, so its metadata
    // round trip is exercised here with the other serializable metadata.
    let row = ReservationRow {
        pool: "bridge".into(),
        job: "job-1".into(),
        model: "zai/glm-5.3-flash".into(),
    };
    let text = serde_json::to_string(&row).unwrap();
    assert_eq!(serde_json::from_str::<ReservationRow>(&text).unwrap(), row);
}
