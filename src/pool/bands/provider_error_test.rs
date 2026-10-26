//! Tests for provider-error classification over finished-job evidence.

use super::*;

/// A collected finished job: `state=done` with the given push word and
/// agent_rc, and the given stderr tail, runner log tail and receipt.
fn collected(push: &str, agent_rc: &str, stderr: &str, log: &str, receipt: &str) -> Collected {
    Collected {
        result: JobResult::parse(&format!(
            "job=abc123def456\nstate=done\npush={push}\nagent_rc={agent_rc}\n"
        )),
        receipt: format!("{receipt}\n"),
        stderr: format!("{stderr}\n"),
        log: format!("{log}\n"),
    }
}

/// The known Z.AI throttling response — HTTP 429 with provider code 1302 —
/// classifies as a rate limit in stderr, in the runner log (with the
/// optional space after the status colon), and as a raw stdout body.
#[test]
fn zai_1302_is_rate_limit() {
    let stderr = r#"request failed: 429:{"code":"1302","msg":"requests too frequent"}"#;
    assert_eq!(
        classify(&collected("no_changes", "1", stderr, "", "")),
        Some(ProviderErrorClass::RateLimit)
    );
    assert_eq!(
        classify(&collected(
            "pushed",
            "1",
            "",
            r#"harness: 429: {"code":"1302","msg":"requests too frequent"}"#,
            ""
        )),
        Some(ProviderErrorClass::RateLimit)
    );
    assert_eq!(
        classify(&collected(
            "no_changes",
            "1",
            "",
            "",
            r#"429:{"code":"1302","msg":"requests too frequent"}"#
        )),
        Some(ProviderErrorClass::RateLimit)
    );
}

/// Provider quota codes cool the provider: as JSON code values in stderr or
/// the runner log, and as raw JSON on stdout at any depth.
#[test]
fn quota_error_cools_provider() {
    assert_eq!(
        classify(&collected(
            "no_changes",
            "1",
            r#"upstream error {"error":{"code":"insufficient_quota","message":"budget empty"}}"#,
            "",
            ""
        )),
        Some(ProviderErrorClass::Quota)
    );
    assert_eq!(
        classify(&collected(
            "pushed",
            "1",
            "",
            r#"retry refused: {"code": "quota_exceeded"}"#,
            ""
        )),
        Some(ProviderErrorClass::Quota)
    );
    assert_eq!(
        classify(&collected(
            "no_changes",
            "1",
            "",
            "",
            r#"{"error":{"code":"insufficient_quota"}}"#
        )),
        Some(ProviderErrorClass::Quota)
    );
}

/// A successful harness result never cools the provider, whatever its text
/// says, and a disk-quota (`ramdisk_full`) result is a host problem that
/// refuses classification even when it dumps provider-shaped output.
#[test]
fn success_and_disk_quota_do_not_cool_provider() {
    assert_eq!(
        classify(&collected(
            "pushed",
            "0",
            "HTTP/1.1 429 Too Many Requests; learned rate limits the hard way",
            "",
            ""
        )),
        None
    );
    assert_eq!(
        classify(&collected(
            "ramdisk_full",
            "1",
            "No space left on device\nagent output: {\"code\":\"insufficient_quota\"}",
            "",
            ""
        )),
        None
    );
}

/// Arbitrary occurrences of 429 and successful prose about rate limits are
/// never throttling evidence.
#[test]
fn prose_429_and_rate_limit_talk_do_not_cool_provider() {
    assert_eq!(
        classify(&collected(
            "push_refused",
            "1",
            "retried the request 429 times before giving up",
            "",
            ""
        )),
        None
    );
    assert_eq!(
        classify(&collected(
            "pushed",
            "1",
            "",
            "the plan discusses rate limits at length",
            ""
        )),
        None
    );
    assert_eq!(
        classify(&collected(
            "no_changes",
            "1",
            "",
            "",
            "we hit 429 and rate limit issues in this task; see notes"
        )),
        None
    );
}

/// Missing tools and connection failures are real failures, not provider
/// throttling.
#[test]
fn missing_tools_and_connection_failures_do_not_cool_provider() {
    assert_eq!(
        classify(&collected(
            "push_refused",
            "1",
            "tool_not_found: no such tool patch_file",
            "",
            ""
        )),
        None
    );
    assert_eq!(
        classify(&collected(
            "no_changes",
            "1",
            "connection error: os error 111 connection refused",
            "",
            ""
        )),
        None
    );
}

/// Recognizable HTTP 429 status lines classify as rate limits in the runner
/// log and as raw HTTP responses on stdout.
#[test]
fn http_429_status_lines_are_rate_limit() {
    assert_eq!(
        classify(&collected(
            "no_changes",
            "1",
            "",
            "HTTP/1.1 429 Too Many Requests",
            ""
        )),
        Some(ProviderErrorClass::RateLimit)
    );
    assert_eq!(
        classify(&collected("no_changes", "1", "", "http/2 429", "")),
        Some(ProviderErrorClass::RateLimit)
    );
    assert_eq!(
        classify(&collected(
            "no_changes",
            "1",
            "",
            "",
            "HTTP/1.1 429 Too Many Requests\nRetry-After: 30\n"
        )),
        Some(ProviderErrorClass::RateLimit)
    );
}

/// Classification requires the structured evidence of a terminal,
/// unsuccessful harness run: a non-terminal state, or an absent or
/// unparsable agent_rc, leaves the provider uncooled even with throttling
/// text in the tails.
#[test]
fn terminal_unsuccessful_evidence_is_required() {
    let failed_stderr = r#"429:{"code":"1302"}"#;
    let killed = Collected {
        result: JobResult::parse("job=abc123def456\nstate=killed\npush=none\n"),
        receipt: String::new(),
        stderr: format!("{failed_stderr}\n"),
        log: String::new(),
    };
    assert_eq!(classify(&killed), None);
    assert_eq!(
        classify(&collected("no_changes", "", failed_stderr, "", "")),
        None
    );
    assert_eq!(
        classify(&collected("no_changes", "", failed_stderr, "", "")),
        None
    );
    assert_eq!(
        classify(&collected("no_changes", "crashed", failed_stderr, "", "")),
        None
    );
}
