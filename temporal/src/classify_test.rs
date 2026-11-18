use super::*;

fn provider(secs: Option<u64>) -> FailureClass {
    FailureClass::Provider {
        retry_after: secs.map(Duration::from_secs),
    }
}

#[test]
fn agent_429_and_quota_text_is_provider() {
    let cases = [
        ("Error: 429 Too Many Requests", None),
        ("HTTP 429; retry-after: 30", Some(30)),
        (
            "provider error: Rate limit reached. Please try again in 20s.",
            Some(20),
        ),
        (
            "You exceeded your current quota, please check your plan",
            None,
        ),
        ("error: insufficient_quota", None),
        ("upstream status 503 Service Unavailable", None),
        ("anthropic: overloaded_error", None),
        ("429 Too Many Requests. Retry-After: 2 min", Some(120)),
    ];
    for (text, secs) in cases {
        assert_eq!(classify_agent(text), provider(secs), "{text}");
    }
}

#[test]
fn retry_after_in_milliseconds_and_capped() {
    assert_eq!(
        retry_after("try again in 1500ms"),
        Some(Duration::from_millis(1_500))
    );
    assert_eq!(retry_after("retry-after: 999999"), Some(MAX_RETRY_AFTER));
    assert_eq!(retry_after("retry later"), None);
}

#[test]
fn a_429_inside_a_sha_is_not_provider() {
    assert_eq!(
        classify_agent("pushed a1429fe to the branch"),
        FailureClass::Infra
    );
}

#[test]
fn rejected_reply_is_format() {
    let text = "vivi-submit: rejected: the envelope has no answer";
    assert_eq!(classify_agent(text), FailureClass::Format);
}

#[test]
fn agent_failure_without_a_signature_is_infra() {
    assert_eq!(classify_agent("agent exit 1 in 3s"), FailureClass::Infra);
    assert_eq!(
        classify_agent("ssh: connect to host studio port 22"),
        FailureClass::Infra
    );
}

#[test]
fn check_exit_codes() {
    assert_eq!(
        classify_check(Some(101), "test foo ... FAILED"),
        FailureClass::Work
    );
    assert_eq!(
        classify_check(Some(1), "error[E0308]: mismatched types"),
        FailureClass::Work
    );
    assert_eq!(classify_check(None, ""), FailureClass::Infra);
    assert_eq!(
        classify_check(Some(127), "cargo: command not found"),
        FailureClass::Infra
    );
    assert_eq!(
        classify_check(Some(128), "fatal: Could not read from remote repository."),
        FailureClass::Infra
    );
}

#[test]
fn plumbing_is_infra_unless_the_provider_says_otherwise() {
    assert_eq!(
        classify_plumbing("fatal: unable to access: Connection refused"),
        FailureClass::Infra
    );
    assert_eq!(
        classify_plumbing("error: no free slot in pool agent"),
        FailureClass::Infra
    );
    assert_eq!(
        classify_plumbing("HTTP 429 from the model proxy"),
        provider(None)
    );
}
