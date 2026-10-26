use super::*;

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
