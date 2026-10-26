use super::*;

#[test]
fn each_reason_reports_a_stable_code() {
    assert_eq!(Reason::Capacity.code(), "band-capacity");
    assert_eq!(Reason::ProviderCooldown.code(), "provider-cooldown");
    assert_eq!(Reason::BandUnavailable.code(), "band-unavailable");
    assert_eq!(Reason::AdmissionBusy.code(), "admission-busy");
}

#[test]
fn a_selection_is_the_choice_or_why_not_and_when_to_ask_again() {
    let picked: Selection<&str> = Selection::Selected("zai/glm-5.3-flash");
    assert_eq!(picked, Selection::Selected("zai/glm-5.3-flash"));

    let busy = Selection::<&str>::Unavailable(Unavailable {
        reason: Reason::AdmissionBusy,
        retry_at: Some(1_800),
    });
    assert_eq!(
        busy,
        Selection::Unavailable(Unavailable {
            reason: Reason::AdmissionBusy,
            retry_at: Some(1_800),
        })
    );
}

#[test]
fn a_band_a_pool_does_not_serve_carries_no_retry() {
    let gone = Unavailable {
        reason: Reason::BandUnavailable,
        retry_at: None,
    };
    assert_eq!(gone.reason.code(), "band-unavailable");
    assert_eq!(gone.retry_at, None);
}

#[test]
fn a_snapshot_defaults_to_an_idle_world() {
    let snapshot = Snapshot::default();
    assert!(snapshot.pool_model_occupancy.is_empty());
    assert!(snapshot.provider_expiry.is_empty());
}

#[test]
fn a_snapshot_holds_occupancy_and_provider_expiry() {
    let snapshot = Snapshot {
        pool_model_occupancy: HashMap::from([(("bridge".into(), "zai/glm-5.3-flash".into()), 2)]),
        provider_expiry: HashMap::from([("zai".into(), 1_800)]),
    };
    assert_eq!(
        snapshot
            .pool_model_occupancy
            .get(&("bridge".into(), "zai/glm-5.3-flash".into())),
        Some(&2)
    );
    assert_eq!(snapshot.provider_expiry.get("zai"), Some(&1_800));
}
