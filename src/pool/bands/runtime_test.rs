use super::*;
use rand_core::RngCore;

use crate::pool::record::{DispatchRecord, OpenDispatch};
use crate::pool::slot::SlotStatus;

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
/// bytes the sampler asks for. An empty script panics the moment the
/// sampler draws, so a test asserting a no-draw path fails loudly if a
/// draw happens.
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

/// A pool from its TOML: the `bands` tables and optional `caps` the test
/// names, over a minimal `local` pool.
fn pool(body: &str) -> PoolConfig {
    toml::from_str(&format!(
        "provider = \"local\"\nslots = 4\n{body}\n[local]\nroot = \"~/lanes\"\n"
    ))
    .unwrap()
}

/// A snapshot of one pool's freedom: `counts` live per model, `claims`
/// unresolved, `cooling` provider deadlines, all taken at `now`.
fn snapshot(
    now: u64,
    counts: &[(&str, u64)],
    claims: u64,
    cooling: &[(&str, u64)],
) -> Availability {
    let mut availability = Availability {
        unresolved_claims: claims,
        now,
        ..Availability::default()
    };
    for (model, count) in counts {
        availability.model_counts.insert((*model).into(), *count);
    }
    for (provider, until) in cooling {
        availability
            .provider_until
            .insert((*provider).into(), *until);
    }
    availability
}

#[test]
fn capped_entry_is_removed_before_sampling() {
    let config = pool(
        r#"
[[bands.P1]]
model = "zai/glm-5.3-flash"
reasoning = "low"
weight = 9
[[bands.P1]]
model = "other/model"
reasoning = "low"
[caps]
"zai/glm-5.3-flash" = 1
"#,
    );
    // The heavy entry's model sits at its cap; with it out the sampler has
    // one survivor and must not draw at all (an empty script panics if it
    // does), so the huge weight is never renormalized back in.
    let availability = snapshot(100, &[("zai/glm-5.3-flash", 1)], 0, &[]);
    assert_eq!(
        select_available_with(&config, "lab", 1, &availability, &mut Scripted(vec![])).unwrap(),
        Decision::Ready(Choice {
            band: 1,
            model: "other/model".into(),
            reasoning: "low".into(),
        })
    );
}

#[test]
fn all_entries_capped_returns_wait() {
    let config = pool(
        r#"
[[bands.P1]]
model = "zai/glm-5.3-flash"
reasoning = "low"
[[bands.P1]]
model = "other/model"
reasoning = "low"
[caps]
"zai/glm-5.3-flash" = 1
"other/model" = 1
"#,
    );
    // Every entry's cap spent by recorded occupancy alone: a model-cap
    // wait, with no invented time for when the pool frees up.
    let busy = snapshot(100, &[("zai/glm-5.3-flash", 1), ("other/model", 1)], 0, &[]);
    let decision = select_available(&config, "lab", 1, &busy).unwrap();
    assert_eq!(
        decision,
        Decision::Waiting(Wait {
            reason: WaitReason::ModelCap,
            retry_at: None,
        })
    );
    // Unresolved claims count conservatively against every capped model,
    // so a single unsettled claim spends both one-entry caps.
    let claimed = snapshot(100, &[], 1, &[]);
    let decision = select_available(&config, "lab", 1, &claimed).unwrap();
    assert_eq!(
        decision,
        Decision::Waiting(Wait {
            reason: WaitReason::ModelCap,
            retry_at: None,
        })
    );
}

#[test]
fn uncapped_models_and_zero_caps_have_defined_behavior() {
    let uncapped = pool(
        r#"
[[bands.P1]]
model = "zai/glm-5.3-flash"
reasoning = "low"
[[bands.P1]]
model = "other/model"
reasoning = "low"
"#,
    );
    // No cap configured: however busy the model looks, occupancy is no
    // limit. The scripted first draw lands in the first entry's zone.
    let crowded = snapshot(100, &[("zai/glm-5.3-flash", 9_000)], 500, &[]);
    assert_eq!(
        select_available_with(&uncapped, "lab", 1, &crowded, &mut Scripted(vec![0])).unwrap(),
        Decision::Ready(Choice {
            band: 1,
            model: "zai/glm-5.3-flash".into(),
            reasoning: "low".into(),
        })
    );

    let zeroed = pool(
        r#"
[[bands.P1]]
model = "zai/glm-5.3-flash"
reasoning = "low"
[[bands.P1]]
model = "other/model"
reasoning = "low"
[caps]
"zai/glm-5.3-flash" = 0
"#,
    );
    // A zero cap excludes immediately, with nothing running at all; the
    // uncapped sibling keeps the band alive without a draw.
    let idle = snapshot(100, &[], 0, &[]);
    assert_eq!(
        select_available_with(&zeroed, "lab", 1, &idle, &mut Scripted(vec![])).unwrap(),
        Decision::Ready(Choice {
            band: 1,
            model: "other/model".into(),
            reasoning: "low".into(),
        })
    );
}

#[test]
fn provider_siblings_are_excluded_but_other_providers_remain_eligible() {
    let config = pool(
        r#"
[[bands.P1]]
model = "zai/glm-5.3-flash"
reasoning = "low"
[[bands.P1]]
model = "zai/other-slug"
reasoning = "low"
[[bands.P1]]
model = "other/model"
reasoning = "low"
"#,
    );
    // `zai` cools every slug it serves together; `other` keeps its entry
    // free. One survivor: the sampler must not draw (an empty script
    // panics if it does).
    let cooling = snapshot(100, &[], 0, &[("zai", 200)]);
    assert_eq!(
        select_available_with(&config, "lab", 1, &cooling, &mut Scripted(vec![])).unwrap(),
        Decision::Ready(Choice {
            band: 1,
            model: "other/model".into(),
            reasoning: "low".into(),
        })
    );
    // At `now == until` the cooldown is over: `zai` is eligible again and
    // the first draw lands in its first entry.
    let recovered = snapshot(200, &[], 0, &[("zai", 200)]);
    assert_eq!(
        select_available_with(&config, "lab", 1, &recovered, &mut Scripted(vec![0])).unwrap(),
        Decision::Ready(Choice {
            band: 1,
            model: "zai/glm-5.3-flash".into(),
            reasoning: "low".into(),
        })
    );
}

#[test]
fn all_cooling_and_mixed_blocked_bands_return_wait() {
    let all_cooling = pool(
        r#"
[[bands.P1]]
model = "zai/glm-5.3-flash"
reasoning = "low"
[[bands.P1]]
model = "limo/free"
reasoning = "low"
"#,
    );
    // Cooldown-only: the wait carries the earliest deadline still running.
    let waiting = snapshot(100, &[], 0, &[("zai", 250), ("limo", 180)]);
    let decision = select_available(&all_cooling, "lab", 1, &waiting).unwrap();
    assert_eq!(
        decision,
        Decision::Waiting(Wait {
            reason: WaitReason::ProviderCooldown,
            retry_at: Some(180),
        })
    );

    let mixed = pool(
        r#"
[[bands.P1]]
model = "zai/glm-5.3-flash"
reasoning = "low"
[[bands.P1]]
model = "other/model"
reasoning = "low"
[caps]
"zai/glm-5.3-flash" = 1
"#,
    );
    // One entry capped, one cooling: the whole band is unavailable, and
    // the known cooldown never stands in for the unknown capacity release.
    let blocked = snapshot(100, &[("zai/glm-5.3-flash", 1)], 0, &[("other", 250)]);
    let decision = select_available(&mixed, "lab", 1, &blocked).unwrap();
    assert_eq!(
        decision,
        Decision::Waiting(Wait {
            reason: WaitReason::BandUnavailable,
            retry_at: None,
        })
    );

    // One choice doubly blocked — spent cap and running cooldown on the
    // same model — is mixed blocking on its own.
    let doubly = pool(
        r#"
[[bands.P1]]
model = "zai/glm-5.3-flash"
reasoning = "low"
[caps]
"zai/glm-5.3-flash" = 1
"#,
    );
    let both = snapshot(100, &[("zai/glm-5.3-flash", 1)], 0, &[("zai", 250)]);
    let decision = select_available(&doubly, "lab", 1, &both).unwrap();
    assert_eq!(
        decision,
        Decision::Waiting(Wait {
            reason: WaitReason::BandUnavailable,
            retry_at: None,
        })
    );
}

#[test]
fn same_model_different_reasoning_entries_remain_distinct_choices() {
    let config = pool(
        r#"
[[bands.P1]]
model = "zai/glm-5.3-flash"
reasoning = "low"
[[bands.P1]]
model = "zai/glm-5.3-flash"
reasoning = "xhigh"
"#,
    );
    // Both entries survive as distinct choices: the draw lands in one
    // reasoning effort or the other, never collapsing the pair.
    let free = snapshot(100, &[], 0, &[]);
    let draw = |values: &[u64]| {
        select_available_with(&config, "lab", 1, &free, &mut Scripted(values.to_vec())).unwrap()
    };
    assert_eq!(
        draw(&[0]),
        Decision::Ready(Choice {
            band: 1,
            model: "zai/glm-5.3-flash".into(),
            reasoning: "low".into(),
        })
    );
    assert_eq!(
        draw(&[1]),
        Decision::Ready(Choice {
            band: 1,
            model: "zai/glm-5.3-flash".into(),
            reasoning: "xhigh".into(),
        })
    );

    // The model's cap is spent by the model alone: both reasoning efforts
    // leave together, and no surviving sibling reasoning is substituted.
    let capped = pool(
        r#"
[[bands.P1]]
model = "zai/glm-5.3-flash"
reasoning = "low"
[[bands.P1]]
model = "zai/glm-5.3-flash"
reasoning = "xhigh"
[caps]
"zai/glm-5.3-flash" = 2
"#,
    );
    let spent = snapshot(100, &[("zai/glm-5.3-flash", 2)], 0, &[]);
    let decision = select_available(&capped, "lab", 1, &spent).unwrap();
    assert_eq!(
        decision,
        Decision::Waiting(Wait {
            reason: WaitReason::ModelCap,
            retry_at: None,
        })
    );
}

#[test]
fn pinned_available_judges_only_the_pinned_model() {
    let config = pool(
        r#"
[[bands.P1]]
model = "zai/glm-5.3-flash"
reasoning = "low"
[[bands.P1]]
model = "other/model"
reasoning = "low"
[caps]
"zai/glm-5.3-flash" = 1
"#,
    );
    // The pinned model's own cap is spent even though a sibling model is
    // free; no substitute answers for it.
    let spent = snapshot(100, &[("zai/glm-5.3-flash", 1)], 0, &[("zai", 400)]);
    assert_eq!(
        pinned_available(&config, "zai/glm-5.3-flash", &spent),
        Decision::Waiting(Wait {
            reason: WaitReason::ModelCap,
            retry_at: None,
        })
    );
    // Its provider's running cooldown names its own deadline.
    let cooling = snapshot(100, &[], 0, &[("zai", 400)]);
    assert_eq!(
        pinned_available(&config, "zai/glm-5.3-flash", &cooling),
        Decision::Waiting(Wait {
            reason: WaitReason::ProviderCooldown,
            retry_at: Some(400),
        })
    );
    // At `now >= until` the provider is eligible again.
    let recovered = snapshot(400, &[], 0, &[("zai", 400)]);
    assert_eq!(
        pinned_available(&config, "zai/glm-5.3-flash", &recovered),
        Decision::Ready(())
    );
    // A sibling provider's deadline and a sibling model's spent cap never
    // touch the pinned model, and an uncapped model is free however busy
    // the pool looks.
    let elsewhere = snapshot(100, &[("zai/glm-5.3-flash", 1)], 7, &[("zai", 400)]);
    assert_eq!(
        pinned_available(&config, "other/model", &elsewhere),
        Decision::Ready(())
    );
}
