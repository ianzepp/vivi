use super::*;

#[test]
fn wait_reasons_answer_with_their_stable_words() {
    assert_eq!(WaitReason::ModelCap.word(), "model-cap");
    assert_eq!(WaitReason::ProviderCooldown.word(), "provider-cooldown");
    assert_eq!(WaitReason::BandUnavailable.word(), "band-unavailable");
    assert_eq!(WaitReason::DispatchBusy.word(), "dispatch-busy");
}

#[test]
fn a_wait_may_carry_a_deadline_or_none() {
    let timed = Decision::<()>::Waiting(Wait {
        reason: WaitReason::ProviderCooldown,
        retry_at: Some(1_700_000_100),
    });
    let untimed = Decision::<()>::Waiting(Wait {
        reason: WaitReason::DispatchBusy,
        retry_at: None,
    });
    assert_eq!(
        timed,
        Decision::Waiting(Wait {
            reason: WaitReason::ProviderCooldown,
            retry_at: Some(1_700_000_100)
        })
    );
    assert_ne!(timed, untimed);
    match untimed {
        Decision::Waiting(wait) => {
            assert_eq!(wait.reason.word(), "dispatch-busy");
            assert_eq!(wait.retry_at, None);
        }
        Decision::Ready(()) => unreachable!("built as Waiting"),
    }
}

#[test]
fn availability_defaults_to_an_empty_pool_at_the_epoch() {
    let availability = Availability::default();
    assert!(availability.model_counts.is_empty());
    assert!(availability.provider_until.is_empty());
    assert_eq!(availability.unresolved_claims, 0);
    assert_eq!(availability.now, 0);
}
