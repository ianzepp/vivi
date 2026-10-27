use super::*;
use crate::pool::record::{DispatchRecord, OpenDispatch};
use crate::pool::slot::SlotStatus;

/// An open dispatch of `pool` running `model` on machine `machine`, with the
/// optional target and kind of an old record left unset unless told.
fn open_of(
    task: &str,
    pool: &str,
    machine: &str,
    job: &str,
    model: &str,
    kind: Option<&str>,
    target: Option<&str>,
) -> OpenDispatch {
    OpenDispatch {
        task: task.into(),
        record: DispatchRecord {
            target: target.map(str::to_string),
            repository: None,
            pool: pool.into(),
            need: None,
            kind: kind.map(str::to_string),
            slot: format!("{pool}-slot-1"),
            machine: machine.into(),
            job: job.into(),
            model: model.into(),
            from: "main".into(),
            branch: "factory/a".into(),
            start: "f".repeat(40),
            with: Vec::new(),
            budget: 600,
            at: 1_700_000_000,
            read_only: false,
            hardware: String::new(),
            abandoned: 0,
            abandoned_cost_micros: 0,
        },
    }
}

/// A census slot of `pool`, carrying `task`'s claim when told.
fn slot_of(pool: &str, machine: &str, task: Option<&str>) -> SlotStatus {
    SlotStatus {
        pool: pool.into(),
        name: format!("{pool}-slot-1"),
        id: machine.into(),
        state: if task.is_some() {
            "busy".into()
        } else {
            "free".into()
        },
        provider_state: "started".into(),
        index: Some(1),
        base: None,
        base_ref: None,
        task: task.map(str::to_string),
        claimed_at: task.map(|_| 1_700_000_000),
        running_secs: None,
        budget_secs: None,
        read_only: None,
        created_at: "2026-01-01T00:00:00Z".into(),
        region: "ewr".into(),
        arch: None,
        space_mark: None,
    }
}

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

#[test]
fn model_caps_span_roles_targets_and_reasoning_but_not_pools() {
    let hand = open_of(
        "t1",
        "alpha",
        "m1",
        "j1",
        "zai/glm-5.3-flash",
        Some("hand"),
        Some("workspace"),
    );
    let analyst = open_of(
        "t2",
        "alpha",
        "m2",
        "j2",
        "zai/glm-5.3-flash",
        Some("analyst"),
        None,
    );
    // A record written before target and kind existed still spends the cap.
    let old = open_of("t3", "alpha", "m3", "j3", "zai/glm-5.3-flash", None, None);
    let other_model = open_of("t4", "alpha", "m4", "j4", "glm-5.3-flash", None, None);
    // Another pool's identical model never shares alpha's cap.
    let elsewhere = open_of(
        "t5",
        "beta",
        "m5",
        "j5",
        "zai/glm-5.3-flash",
        Some("hand"),
        None,
    );
    let open = [hand, analyst, old, other_model, elsewhere];
    let mut deadlines = BTreeMap::new();
    deadlines.insert("zai".to_string(), 1_700_000_600);
    let alpha = Availability::from_evidence("alpha", &open, &[], deadlines, 1_700_000_100);
    assert_eq!(alpha.model_counts.get("zai/glm-5.3-flash"), Some(&3));
    assert_eq!(alpha.model_counts.get("glm-5.3-flash"), Some(&1));
    // A recorded open job stays counted with no census slot behind it.
    assert_eq!(alpha.unresolved_claims, 0);
    assert_eq!(alpha.now, 1_700_000_100);
    let mut kept = BTreeMap::new();
    kept.insert("zai".to_string(), 1_700_000_600);
    assert_eq!(alpha.provider_until, kept);
    let beta = Availability::from_evidence("beta", &open, &[], BTreeMap::new(), 1_700_000_100);
    assert_eq!(beta.model_counts.get("zai/glm-5.3-flash"), Some(&1));
    assert_eq!(beta.model_counts.get("glm-5.3-flash"), None);
}

#[test]
fn unrecorded_claim_is_counted_conservatively() {
    // This claim's dispatch is recorded: counted once in its model's cap,
    // never again as unresolved.
    let matched = open_of(
        "t1",
        "alpha",
        "m1",
        "j1",
        "zai/glm-5.3-flash",
        Some("hand"),
        None,
    );
    let open = [matched];
    let slots = [
        slot_of("alpha", "m1", Some("t1")),
        // A claim with no open record: an unresolved occupancy.
        slot_of("alpha", "m2", Some("t2")),
        // A free slot claims nothing; another pool's claim is not ours.
        slot_of("alpha", "m3", None),
        slot_of("beta", "m4", Some("t3")),
    ];
    let availability =
        Availability::from_evidence("alpha", &open, &slots, BTreeMap::new(), 1_700_000_100);
    assert_eq!(availability.model_counts.get("zai/glm-5.3-flash"), Some(&1));
    assert_eq!(availability.unresolved_claims, 1);
}
