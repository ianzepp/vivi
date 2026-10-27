use std::collections::BTreeMap;

use chrono::{TimeZone, Utc};

use super::*;
use crate::mailspace::PoolEventRow;
use crate::pool::collect::{Collected, JobResult, Outcome};

/// The checked-in Z.AI-style provider-error excerpt: the refusal stays in
/// its own stderr section, never inside the agent's message.
const ZAI_STDERR: &str =
    "post to provider failed: status 429\nupstream error code 1302: rate limited\n";

/// A finished dispatch with the given exit status, stderr and receipt.
fn finished(agent_rc: &str, stderr: &str, receipt: &str) -> Outcome {
    Outcome::Finished(Collected {
        result: JobResult::parse(&format!("agent_rc={agent_rc}\n")),
        receipt: format!("{receipt}\n"),
        stderr: format!("{stderr}\n"),
        log: String::new(),
    })
}

/// A fixed collection instant.
fn collected_at() -> chrono::DateTime<chrono::Utc> {
    Utc.timestamp_opt(1_792_000_000, 0).unwrap()
}

#[test]
fn the_checked_in_provider_error_cools_its_provider_for_sixty_seconds() {
    let evidence = classify(
        "zai/glm-5.3-flash",
        &finished("1", ZAI_STDERR, ""),
        collected_at(),
    )
    .unwrap();
    assert_eq!(evidence.class, FailureClass::RateLimit);
    assert_eq!(evidence.provider, "zai");
    assert_eq!(evidence.until, 1_792_000_060);
    // A later collection moves the absolute deadline with it.
    let later = collected_at() + chrono::Duration::seconds(300);
    let evidence = classify("zai/glm-5.3-flash", &finished("1", ZAI_STDERR, ""), later).unwrap();
    assert_eq!(evidence.until, 1_792_000_360);
}

#[test]
fn structured_quota_codes_classify_as_quota_for_the_slug_prefix() {
    for stderr in [
        "post to provider failed: {\"error\":{\"code\":\"insufficient_quota\"}}\n",
        "provider reply: {\"code\": \"quota-exceeded\"}\n",
        "{\"code\":\"usage_limit_reached\"}\n",
    ] {
        let evidence = classify(
            "openrouter/qwen3-max",
            &finished("1", stderr, ""),
            collected_at(),
        )
        .unwrap();
        assert_eq!(evidence.class, FailureClass::Quota);
        assert_eq!(evidence.provider, "openrouter");
        assert_eq!(evidence.until, 1_792_000_060);
    }
}

#[test]
fn the_one_line_json_body_of_the_refusal_still_classifies() {
    let stderr =
        "post to provider failed: status 429: {\"code\":\"1302\",\"message\":\"rate limited\"}\n";
    let evidence = classify("glm-5.3-flash", &finished("1", stderr, ""), collected_at()).unwrap();
    assert_eq!(evidence.class, FailureClass::RateLimit);
    assert_eq!(evidence.provider, "glm-5.3-flash");
}

#[test]
fn harness_output_evidence_classifies_like_stderr() {
    let outcome = finished("2", "", &format!("head\n--- stderr tail\n{ZAI_STDERR}"));
    let evidence = classify("zai/glm-5.3-flash", &outcome, collected_at()).unwrap();
    assert_eq!(evidence.class, FailureClass::RateLimit);
    assert_eq!(evidence.provider, "zai");
}

#[test]
fn ordinary_failures_and_successful_mentions_do_not_cool_providers() {
    let plain_code_failure = finished(
        "1",
        "error[E0308]: mismatched types\ntest result: FAILED\n",
        "",
    );
    let successful_mention = finished("0", "", &format!("report\n{ZAI_STDERR}"));
    let connection_failure = finished("1", "post to provider failed: connection refused\n", "");
    let overload_alone = finished("1", "provider is overloaded_error, try later\n", "");
    let quota_prose = finished("1", "my quota is nearly exhausted, work carefully\n", "");
    for outcome in [
        &plain_code_failure,
        &successful_mention,
        &connection_failure,
        &overload_alone,
        &quota_prose,
    ] {
        assert!(classify("zai/glm-5.3-flash", outcome, collected_at()).is_none());
    }
}

#[test]
fn unfinished_and_exitless_dispatches_do_not_cool_providers() {
    assert!(classify("zai/glm-5.3-flash", &Outcome::Timeout, collected_at()).is_none());
    assert!(classify("zai/glm-5.3-flash", &Outcome::Lost, collected_at()).is_none());
    let missing_exit = finished("", ZAI_STDERR, "");
    assert!(classify("zai/glm-5.3-flash", &missing_exit, collected_at()).is_none());
    let unparsable_exit = finished("terminated", ZAI_STDERR, "");
    assert!(classify("zai/glm-5.3-flash", &unparsable_exit, collected_at()).is_none());
}

#[test]
fn the_provider_key_is_the_prefix_before_the_first_slash() {
    assert_eq!(provider_key("zai/glm-5.3-flash"), "zai");
    assert_eq!(provider_key("openrouter/anthropic/claude"), "openrouter");
}

#[test]
fn an_unqualified_slug_is_its_own_provider_key() {
    assert_eq!(provider_key("glm-5.3-flash"), "glm-5.3-flash");
    assert_eq!(provider_key(""), "");
}

#[test]
fn slugs_of_one_provider_group_under_one_key() {
    let models = ["zai/glm-5.3-flash", "zai/glm-5-turbo", "glm-5.3-flash"];
    let mut groups: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for model in models {
        groups.entry(provider_key(model)).or_default().push(model);
    }
    assert_eq!(groups.get("zai").map(Vec::len), Some(2));
    assert_eq!(groups.get("glm-5.3-flash").map(Vec::len), Some(1));
}

#[test]
fn failure_classes_answer_with_their_stable_words_and_round_trip() {
    assert_eq!(FailureClass::RateLimit.word(), "rate-limit");
    assert_eq!(FailureClass::Quota.word(), "quota");
    for class in [FailureClass::RateLimit, FailureClass::Quota] {
        let text = serde_json::to_string(&class).unwrap();
        assert_eq!(serde_json::from_str::<FailureClass>(&text).unwrap(), class);
    }
}

#[test]
fn evidence_carries_the_provider_the_deadline_and_the_class() {
    let evidence = Evidence {
        provider: "zai".into(),
        until: 1_700_000_100,
        class: FailureClass::RateLimit,
    };
    let text = serde_json::to_string(&evidence).unwrap();
    assert_eq!(serde_json::from_str::<Evidence>(&text).unwrap(), evidence);
    assert_eq!(evidence.until, 1_700_000_100);
}

/// A fixed epoch second every replay test anchors its deadlines at.
const BASE: u64 = 1_792_000_000;

/// A `pool_collected` record from `pool` whose note carries cooldown
/// metadata for `provider`, ending at `until`, classified `class`.
fn collected(handle: &str, pool: &str, provider: &str, until: u64, class: &str) -> PoolEventRow {
    PoolEventRow {
        handle: handle.to_string(),
        event_type: "pool_collected".to_string(),
        note: serde_json::json!({
            "job": "j-1",
            "pool": pool,
            "slot": "slot-1",
            "outcome": "failed",
            "band_cooldown": { "provider": provider, "until": until, "class": class },
        })
        .to_string(),
    }
}

/// A pool event whose note is arbitrary text, carrying no cooldown
/// metadata at all.
fn plain(handle: &str, event_type: &str, note: &str) -> PoolEventRow {
    PoolEventRow {
        handle: handle.to_string(),
        event_type: event_type.to_string(),
        note: note.to_string(),
    }
}

#[test]
fn cooldown_is_shared_across_pools_in_one_mailspace() {
    let events = vec![
        collected("task-1", "alpha", "zai", BASE + 60, "rate-limit"),
        collected("task-2", "beta", "zai", BASE + 120, "quota"),
        collected("task-3", "alpha", "openrouter", BASE + 30, "quota"),
    ];
    let folded = fold(&events, BASE);
    // The later deadline of the shared provider wins; which pool wrote
    // the record is never a scope.
    assert_eq!(
        folded,
        BTreeMap::from([
            ("openrouter".to_string(), BASE + 30),
            ("zai".to_string(), BASE + 120),
        ])
    );
    // Reordered events fold to the same deadlines.
    let mut reordered = events.clone();
    reordered.reverse();
    assert_eq!(fold(&reordered, BASE), folded);
}

#[test]
fn cooldown_expires_at_its_deadline() {
    let events = vec![collected("task-1", "alpha", "zai", BASE + 60, "rate-limit")];
    assert_eq!(fold(&events, BASE).get("zai"), Some(&(BASE + 60)));
    // One second short of the deadline the provider is still cooled down.
    assert_eq!(fold(&events, BASE + 59).get("zai"), Some(&(BASE + 60)));
    // At now == until the evidence is expired; after it nothing remains.
    assert!(fold(&events, BASE + 60).is_empty());
    assert!(fold(&events, BASE + 61).is_empty());
}

#[test]
fn replaying_collection_does_not_extend_cooldown() {
    let events = vec![
        collected("task-1", "alpha", "zai", BASE + 60, "rate-limit"),
        // The same collection replayed, and an older one replayed after
        // it: neither restarts the deadline from now nor lowers it.
        collected("task-1", "alpha", "zai", BASE + 60, "rate-limit"),
        collected("task-0", "beta", "zai", BASE + 30, "quota"),
    ];
    let folded = fold(&events, BASE);
    assert_eq!(folded.get("zai"), Some(&(BASE + 60)));
    // A replay at a later now never moves the absolute deadline.
    assert_eq!(fold(&events, BASE + 10).get("zai"), Some(&(BASE + 60)));
    // A success collected between the failures carries no metadata and
    // clears nothing; replaying the replay is a fixed point.
    let with_success = vec![
        events[0].clone(),
        plain(
            "task-2",
            "pool_collected",
            r#"{"job":"j-2","pool":"alpha","outcome":"succeeded"}"#,
        ),
        events[1].clone(),
    ];
    assert_eq!(fold(&with_success, BASE), folded);
    assert_eq!(fold(&with_success, BASE), fold(&with_success, BASE));
}

#[test]
fn old_pool_events_need_no_cooldown_metadata() {
    let old_notes = [
        "{}",
        "pool result j-1: failed\nsome receipt prose mentioning status 429\n",
        "not json at all",
        r#"{"job":"j-1","pool":"alpha","outcome":"failed"}"#,
        // Metadata that is not an object, or not shaped like Evidence.
        r#"{"band_cooldown":"status 429 rate limited"}"#,
        r#"{"band_cooldown":42}"#,
        r#"{"band_cooldown":[]}"#,
        r#"{"band_cooldown":{"provider":"zai","until":"soon","class":"rate-limit"}}"#,
        r#"{"band_cooldown":{"provider":"zai","until":1792000060}}"#,
    ];
    for note in old_notes {
        let events = vec![plain("task-1", "pool_collected", note)];
        assert!(fold(&events, BASE).is_empty(), "note classified: {note}");
    }
    // Another event type is not evidence either, metadata or not.
    let dispatched = plain(
        "task-1",
        "pool_dispatched",
        r#"{"band_cooldown":{"provider":"zai","until":1792000060,"class":"rate-limit"}}"#,
    );
    assert!(fold(&[dispatched], BASE).is_empty());
    // Old notes beside fresh evidence change nothing about it.
    let mixed = vec![
        plain("task-0", "pool_collected", "not json at all"),
        collected("task-1", "alpha", "zai", BASE + 60, "rate-limit"),
        plain("task-2", "pool_collected", "{}"),
    ];
    assert_eq!(fold(&mixed, BASE).get("zai"), Some(&(BASE + 60)));
}
