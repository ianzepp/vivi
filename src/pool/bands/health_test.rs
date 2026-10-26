use super::*;

use std::collections::BTreeMap;

use crate::mailspace::PoolEventRow;
use crate::pool::bands::availability::{Availability, WaitingReason};
use crate::pool::collect::{Collected, JobResult, Outcome};
use crate::pool::config::PoolConfig;
use crate::pool::record::DispatchRecord;
use chrono::TimeZone;

/// The exact provider throttle line a rate-limited zai call leaves behind.
const KNOWN_1302: &str = "429: {\"code\":\"1302\",\"message\":\"Rate limit reached for requests\"}";

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

/// A dispatch record for `model` on one slot.
fn record(model: &str) -> DispatchRecord {
    DispatchRecord {
        target: None,
        repository: None,
        pool: "p1".into(),
        need: None,
        kind: None,
        slot: "slot-a".into(),
        machine: "mach".into(),
        job: "job000000001".into(),
        model: model.into(),
        from: "main".into(),
        branch: "work".into(),
        start: "abcd1234".into(),
        with: vec![],
        budget: 600,
        at: 1_700_000_000,
        read_only: false,
        hardware: String::new(),
        abandoned: 0,
        abandoned_cost_micros: 0,
    }
}

/// A finished outcome whose receipt/stderr carry the given text and whose
/// result parses from `result` (`key=value` lines).
fn finished(result: &str, receipt: &str, stderr: &str) -> Outcome {
    Outcome::Finished(Collected {
        result: JobResult::parse(result),
        receipt: format!("{receipt}\n"),
        stderr: format!("{stderr}\n"),
        log: String::new(),
    })
}

/// The classification of one run of `model`, collected at second 0.
fn classified(model: &str, outcome: &Outcome) -> Option<Classification> {
    observation(&record(model), outcome, at(0)).map(|seen| seen.classification)
}

/// An observation of `provider` seen at second `seen_secs`.
fn observed(provider: &str, seen_secs: i64) -> HealthObservation {
    HealthObservation::at(provider, Classification::RateLimit, at(seen_secs))
}

/// The JSON collected note of `pool`, carrying `seen` under the namespaced
/// health key when present.
fn collected_note(pool: &str, seen: Option<&HealthObservation>) -> String {
    let mut note = serde_json::json!({
        "job": "job000000001",
        "pool": pool,
        "slot": "slot-a",
        "outcome": "finished",
    });
    if let Some(seen) = seen {
        note[COLLECTED_NOTE_KEY] = serde_json::to_value(seen).unwrap();
    }
    note.to_string()
}

/// A `pool_collected` event row of `pool` with the given observation.
fn collected(pool: &str, seen: Option<&HealthObservation>) -> PoolEventRow {
    PoolEventRow {
        handle: "t1".into(),
        event_type: "pool_collected".into(),
        note: collected_note(pool, seen),
    }
}

/// The availability view answering at `now_secs` with `pool`'s replayed
/// deadlines from `events`.
fn view_at(events: &[PoolEventRow], pool: &str, now_secs: i64) -> Availability {
    Availability {
        provider_deadlines: cooldowns(events, pool),
        ..Availability::at(at(now_secs))
    }
}

#[test]
fn rate_limit_and_quota_responses_start_provider_cooldown() {
    let throttled = finished("job=j9\nagent_rc=1\n", "", KNOWN_1302);
    assert_eq!(
        observation(&record("zai/glm-5.3-flash"), &throttled, at(0)),
        Some(HealthObservation::at(
            "zai",
            Classification::RateLimit,
            at(0)
        ))
    );
    // An unqualified slug keys to itself.
    assert_eq!(
        classified("glm-5.3-flash", &throttled),
        Some(Classification::RateLimit)
    );
    assert_eq!(
        classified(
            "openai/gpt-5.2",
            &finished(
                "agent_rc=1\n",
                "",
                "{\"error\":{\"code\":\"rate_limit_exceeded\"}}"
            )
        ),
        Some(Classification::RateLimit)
    );
    assert_eq!(
        classified(
            "openai/gpt-5.2",
            &finished(
                "agent_rc=1\n",
                "{\"error\":{\"code\":\"insufficient_quota\"}}",
                ""
            )
        ),
        Some(Classification::Quota)
    );
    assert_eq!(
        classified(
            "openai/gpt-5.2",
            &finished(
                "agent_rc=1\n",
                "",
                "{\"error\":{\"code\":\"quota_exceeded\"}}"
            )
        ),
        Some(Classification::Quota)
    );
    assert_eq!(
        classified(
            "zai/glm-5.3-flash",
            &finished(
                "agent_rc=1\n",
                "",
                "error sending request: HTTP 429 Too Many Requests"
            )
        ),
        Some(Classification::RateLimit)
    );
    assert_eq!(
        classified(
            "zai/glm-5.3-flash",
            &finished(
                "agent_rc=1\n",
                "",
                "server said HTTP/1.1 429 Too Many Requests"
            )
        ),
        Some(Classification::RateLimit)
    );
    assert_eq!(
        classified(
            "zai/glm-5.3-flash",
            &finished("agent_rc=1\n", "", "request failed with status 429")
        ),
        Some(Classification::RateLimit)
    );
}

#[test]
fn structured_provider_error_with_zero_exit_is_recognized() {
    // An error-only provider response: the agent exited 0 and pushed, but
    // the whole receipt is the provider's error payload.
    assert_eq!(
        classified(
            "zai/glm-5.3-flash",
            &finished("agent_rc=0\npush=pushed\n", KNOWN_1302, "")
        ),
        Some(Classification::RateLimit)
    );
    assert_eq!(
        classified(
            "openai/gpt-5.2",
            &finished(
                "agent_rc=0\n",
                "",
                "{\"error\":{\"code\": \"quota_exceeded\"}}"
            )
        ),
        Some(Classification::Quota)
    );
}

#[test]
fn ordinary_receipt_and_filesystem_quota_do_not_cool_provider() {
    // An ordinary successful run, however wordy.
    let ordinary = finished(
        "agent_rc=0\npush=pushed\nsha=abcd1234\n",
        "All checks passed; the table renders as specified.\n",
        "",
    );
    assert_eq!(classified("zai/glm-5.3-flash", &ordinary), None);
    // A filesystem quota is not a provider quota.
    let disk = finished(
        "agent_rc=1\npush=ramdisk_full\nramdisk_size_gib=4\n",
        "",
        "No space left on device: disk quota exceeded\n",
    );
    assert_eq!(classified("zai/glm-5.3-flash", &disk), None);
}

#[test]
fn bare_429_numbers_do_not_cool_a_provider() {
    let bare = finished("agent_rc=1\n", "", "attempt 429 of 500 failed\n");
    assert_eq!(classified("zai/glm-5.3-flash", &bare), None);
}

#[test]
fn prose_and_code_examples_do_not_cool_a_provider() {
    let prose = finished(
        "agent_rc=1\n",
        "Retry when err.code == \"rate_limit_exceeded\" or status == 429. Example:\n\
         if response.status == 429 { backoff(); }\n",
        "",
    );
    assert_eq!(classified("zai/glm-5.3-flash", &prose), None);
}

#[test]
fn timeout_lost_and_empty_model_produce_no_observation() {
    assert_eq!(
        observation(&record("zai/glm-5.3-flash"), &Outcome::Timeout, at(0)),
        None
    );
    assert_eq!(
        observation(&record("zai/glm-5.3-flash"), &Outcome::Lost, at(0)),
        None
    );
    // A no-model command job has no provider to cool.
    let throttled = finished("agent_rc=1\n", "", KNOWN_1302);
    assert_eq!(observation(&record(""), &throttled, at(0)), None);
}

#[test]
fn evidence_in_the_runner_log_alone_is_never_inspected() {
    let logged = Collected {
        result: JobResult::parse("agent_rc=1\n"),
        receipt: String::new(),
        stderr: String::new(),
        log: format!("{KNOWN_1302}\n"),
    };
    assert_eq!(
        observation(
            &record("zai/glm-5.3-flash"),
            &Outcome::Finished(logged),
            at(0)
        ),
        None
    );
}

#[test]
fn other_failures_do_not_cool_a_provider() {
    // Authentication failure.
    assert_eq!(
        classified(
            "zai/glm-5.3-flash",
            &finished("agent_rc=1\n", "", "401 Unauthorized: invalid api key\n")
        ),
        None
    );
    // Connection error: real, but not a rate limit or quota.
    assert_eq!(
        classified(
            "zai/glm-5.3-flash",
            &finished(
                "agent_rc=1\n",
                "",
                "Connection error: connection reset by peer\n"
            )
        ),
        None
    );
    // An unrelated failing test run whose output merely mentions 429s.
    assert_eq!(
        classified(
            "zai/glm-5.3-flash",
            &finished(
                "agent_rc=1\n",
                "",
                "running 429 tests\ntest rate_limit_exceeded_is_reported ... FAILED\n"
            )
        ),
        None
    );
    // A publication problem is not a provider failure: the agent itself
    // exited 0.
    assert_eq!(
        classified(
            "zai/glm-5.3-flash",
            &finished(
                "agent_rc=0\npush=push_refused\n",
                "the branch was refused\n",
                ""
            )
        ),
        None
    );
}

#[test]
fn replaying_failure_does_not_extend_cooldown() {
    // One failure observed at second 0: the deadline is the stored one —
    // its own observed_at plus the default cooldown — never now plus 60.
    let events = [collected("p1", Some(&observed("zai", 0)))];
    assert_eq!(
        cooldowns(&events, "p1"),
        BTreeMap::from([("zai".to_string(), at(COOLDOWN_SECS as i64))])
    );
    // Several failures of one provider keep the maximum stored deadline,
    // in any event order.
    let events = [
        collected("p1", Some(&observed("zai", 30))),
        collected("p1", Some(&observed("zai", 0))),
        collected("p1", Some(&observed("zai", 10))),
    ];
    assert_eq!(
        cooldowns(&events, "p1").get("zai"),
        Some(&at(30 + COOLDOWN_SECS as i64))
    );
}

#[test]
fn cooldown_excludes_sibling_models_but_not_other_providers() {
    // A failure of one qualified slug cools every slug of its provider.
    let events = [collected("p1", Some(&observed("zai", 0)))];
    let view = view_at(&events, "p1", COOLDOWN_SECS as i64 / 2);
    assert!(!view.model_available("zai/glm-5.3-flash"));
    assert!(!view.model_available("zai/glm-5.3"));
    assert!(view.model_available("openai/gpt-5.2"));
    // An unqualified slug is its own provider key: other slugs keep
    // working.
    let events = [collected("p1", Some(&observed("glm-5.3-flash", 0)))];
    let view = view_at(&events, "p1", COOLDOWN_SECS as i64 / 2);
    assert!(!view.model_available("glm-5.3-flash"));
    assert!(view.model_available("zai/glm-5.3-flash"));
}

#[test]
fn cooldown_is_pool_scoped() {
    // The observation was recorded on pool p2; pool p1 replays nothing.
    let events = [collected("p2", Some(&observed("zai", 0)))];
    assert!(cooldowns(&events, "p1").is_empty());
    assert_eq!(
        cooldowns(&events, "p2").get("zai"),
        Some(&at(COOLDOWN_SECS as i64))
    );
    // Only collected events count: a dispatched event carrying the same
    // payload contributes nothing.
    let dispatched = PoolEventRow {
        event_type: "pool_dispatched".into(),
        ..collected("p1", Some(&observed("zai", 0)))
    };
    assert!(cooldowns(&[dispatched], "p1").is_empty());
}

#[test]
fn older_success_does_not_clear_active_cooldown() {
    // A job dispatched before the failure but collected after it carries no
    // observation in its note and never clears what the failure set.
    let events = [
        collected("p1", Some(&observed("zai", 0))),
        collected("p1", None),
    ];
    let view = view_at(&events, "p1", 30);
    assert_eq!(view.provider_deadlines.get("zai"), Some(&at(60)));
    let blocked = view.unavailable(["zai/glm-5.3-flash"]).unwrap();
    assert_eq!(blocked.reason, WaitingReason::ProviderCooldown);
    assert_eq!(blocked.until, Some(at(60)));
}

#[test]
fn legacy_and_malformed_notes_imply_no_cooldown() {
    // Older notes carry no health metadata at all.
    assert!(cooldowns(&[collected("p1", None)], "p1").is_empty());
    // Malformed metadata invents no deadline and poisons no other
    // provider's stored one.
    let mut string_note = collected("p1", None);
    let mut payload = serde_json::from_str::<serde_json::Value>(&string_note.note).unwrap();
    payload[COLLECTED_NOTE_KEY] = serde_json::json!("not an observation");
    string_note.note = payload.to_string();
    let mut partial_note = collected("p1", None);
    let mut payload = serde_json::from_str::<serde_json::Value>(&partial_note.note).unwrap();
    payload[COLLECTED_NOTE_KEY] = serde_json::json!({"provider": "zai"});
    partial_note.note = payload.to_string();
    let events = [
        string_note,
        partial_note,
        collected("p1", Some(&observed("openai", 0))),
    ];
    let deadlines = cooldowns(&events, "p1");
    assert_eq!(deadlines.len(), 1);
    assert_eq!(deadlines.get("openai"), Some(&at(COOLDOWN_SECS as i64)));
}

#[test]
fn eligibility_turns_exactly_at_the_deadline() {
    let events = [collected("p1", Some(&observed("zai", 0)))];
    let view = view_at(&events, "p1", COOLDOWN_SECS as i64 - 1);
    assert!(!view.model_available("zai/glm-5.3-flash"));
    // At the deadline itself the provider is eligible again.
    let view = view_at(&events, "p1", COOLDOWN_SECS as i64);
    assert!(view.model_available("zai/glm-5.3-flash"));
    assert_eq!(view.unavailable(["zai/glm-5.3-flash"]), None);
}
