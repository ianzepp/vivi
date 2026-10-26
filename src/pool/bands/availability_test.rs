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

// --- from_events: cap snapshots from durable open dispatches ---

use crate::pool::record::{DispatchRecord, dispatch_note, fold_open};

fn record(pool: &str, model: &str) -> DispatchRecord {
    DispatchRecord {
        target: None,
        repository: None,
        pool: pool.into(),
        need: None,
        kind: None,
        slot: "p-slot-1".into(),
        machine: "m1".into(),
        job: "j1".into(),
        model: model.into(),
        from: "main".into(),
        branch: "factory/a".into(),
        start: "a".repeat(40),
        with: Vec::new(),
        budget: 600,
        at: 1000,
        read_only: false,
        hardware: String::new(),
        abandoned: 0,
        abandoned_cost_micros: 0,
    }
}

fn row(handle: &str, kind: &str, note: String) -> PoolEventRow {
    PoolEventRow {
        handle: handle.into(),
        event_type: kind.into(),
        note,
    }
}

fn dispatched(handle: &str, pool: &str, model: &str) -> PoolEventRow {
    row(
        handle,
        "pool_dispatched",
        dispatch_note(&record(pool, model)),
    )
}

fn collected(handle: &str, outcome: &str) -> PoolEventRow {
    row(
        handle,
        "pool_collected",
        format!("{{\"outcome\":\"{outcome}\"}}"),
    )
}

/// A dispatch note written the way a tool outside this binary writes one:
/// only the fields every record has always carried, the rest left to serde
/// defaults.
fn external_row(handle: &str, pool: &str, model: &str) -> PoolEventRow {
    row(
        handle,
        "pool_dispatched",
        format!(
            "{{\"pool\":\"{pool}\",\"slot\":\"s\",\"machine\":\"m\",\"job\":\"j\",\
             \"model\":\"{model}\",\"from\":\"main\",\"branch\":\"factory/a\",\
             \"start\":\"{}\",\"budget\":600,\"at\":1000}}",
            "a".repeat(40)
        ),
    )
}

fn config(caps: &str) -> PoolConfig {
    toml::from_str(&format!("slots = 1\n\n{caps}")).unwrap()
}

#[test]
fn same_model_entries_share_a_cap() {
    let config = config("[caps]\n\"a/m\" = 2\n");
    let both = vec![
        dispatched("aaaa1111", "p", "a/m"),
        dispatched("bbbb2222", "p", "a/m"),
    ];
    let view = Availability::from_events(&config, "p", &both, at(0));
    assert!(view.capped_models.contains("a/m"));
    assert_eq!(view.now, at(0));
    assert!(view.provider_deadlines.is_empty());

    // Two entries of the same slug reach the shared cap, so one entry of
    // two leaves room: the slug stays available.
    let one = vec![dispatched("aaaa1111", "p", "a/m")];
    let view = Availability::from_events(&config, "p", &one, at(0));
    assert!(view.model_available("a/m"));
    assert_eq!(view.unavailable(["a/m"]), None);
}

#[test]
fn other_pools_do_not_consume_model_cap() {
    let config = config("[caps]\n\"a/m\" = 1\n");
    let events = vec![dispatched("aaaa1111", "q", "a/m")];
    let view = Availability::from_events(&config, "p", &events, at(0));
    assert!(view.model_available("a/m"));

    // The same record counts when the snapshot is asked about its own pool.
    let view = Availability::from_events(&config, "q", &events, at(0));
    assert!(!view.model_available("a/m"));
}

#[test]
fn collected_dispatch_releases_model_capacity() {
    let config = config("[caps]\n\"a/m\" = 1\n");
    let events = vec![
        dispatched("aaaa1111", "p", "a/m"),
        collected("aaaa1111", "finished"),
    ];
    let view = Availability::from_events(&config, "p", &events, at(0));
    assert!(view.model_available("a/m"));
    assert!(fold_open(&events).is_empty());
}

#[test]
fn timeout_and_lost_closure_also_release_model_capacity() {
    let config = config("[caps]\n\"a/m\" = 1\n");
    for outcome in ["timeout", "lost"] {
        let events = vec![
            dispatched("aaaa1111", "p", "a/m"),
            collected("aaaa1111", outcome),
        ];
        let view = Availability::from_events(&config, "p", &events, at(0));
        assert!(view.model_available("a/m"), "{outcome} releases the cap");
    }
}

#[test]
fn an_omitted_cap_is_unlimited() {
    let config = config("");
    let events = vec![
        dispatched("aaaa1111", "p", "a/m"),
        dispatched("bbbb2222", "p", "a/m"),
    ];
    let view = Availability::from_events(&config, "p", &events, at(0));
    assert!(view.capped_models.is_empty());
    assert_eq!(view.unavailable(["a/m"]), None);
}

#[test]
fn a_zero_cap_blocks_even_with_no_dispatches() {
    let config = config("[caps]\n\"a/m\" = 0\n");
    let view = Availability::from_events(&config, "p", &[], at(0));
    assert!(!view.model_available("a/m"));
    let gate = view.unavailable(["a/m"]).expect("zero cap blocks");
    assert_eq!(gate.reason, WaitingReason::ModelCap);
    assert_eq!(gate.blocked_models, vec!["a/m".to_string()]);
    assert_eq!(gate.until, None);
}

#[test]
fn a_finished_but_uncollected_record_still_counts() {
    let config = config("[caps]\n\"a/m\" = 1\n");
    let events = vec![dispatched("aaaa1111", "p", "a/m")];
    let view = Availability::from_events(&config, "p", &events, at(0));
    assert!(!view.model_available("a/m"));
}

#[test]
fn a_dispatch_record_written_outside_the_pump_counts() {
    let config = config("[caps]\n\"a/m\" = 1\n");
    let events = vec![external_row("aaaa1111", "p", "a/m")];
    assert_eq!(fold_open(&events).len(), 1);
    let view = Availability::from_events(&config, "p", &events, at(0));
    assert!(!view.model_available("a/m"));
}

#[test]
fn an_empty_model_record_consumes_no_capacity() {
    let config = config("[caps]\n\"a/m\" = 1\n\"b/m\" = 1\n\"\" = 1\n");
    let events = vec![
        dispatched("aaaa1111", "p", ""),
        dispatched("bbbb2222", "p", "a/m"),
    ];
    let view = Availability::from_events(&config, "p", &events, at(0));
    assert!(view.capped_models.contains("a/m"));
    assert!(!view.capped_models.contains("b/m"));
    assert!(!view.capped_models.contains(""));
}
