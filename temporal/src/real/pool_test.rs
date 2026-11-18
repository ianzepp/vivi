use super::*;

/// Captured from `vivi task show --latest` on studio (vivi 22.0.0).
const SHOWN: &str = r#"
## 2026-11-18T16:42:54+00:00 - 1cfb4ad2 [mail captured]
From: operator@029e8925.local
Subject: Re: probe review

+++
[pool]
event = "pool_collected"
record = '{"agent_rc":"0","agent_s":"13","branch":"","job":"4a1a332a517c","outcome":"finished","pool":"agent","push":"read_only_unchanged","read_only":true,"sha":"","state":"done"}'
+++

pool result 4a1a332a517c: finished
result: read_only_unchanged

--- receipt
VERDICT: APPROVE

--- runner log tail
from origin/vivi/main at ee8ad6672a61 prepared_s 0
agent_rc 0 agent_s 13
"#;

#[test]
fn collected_record_receipt_and_log() {
    let c = parse_collected(SHOWN).unwrap();
    assert_eq!(c.push, "read_only_unchanged");
    assert_eq!(c.agent_rc, "0");
    assert!(c.agent_ok());
    assert_eq!(c.receipt, "VERDICT: APPROVE");
    assert!(c.log.starts_with("from origin/vivi/main"));
}

#[test]
fn committed_job_carries_the_full_sha() {
    let shown = SHOWN
        .replace("\"push\":\"read_only_unchanged\"", "\"push\":\"committed\"")
        .replace(
            "\"sha\":\"\"",
            "\"sha\":\"80a76646e07591c493b616771f08f74a161c5608\"",
        );
    let c = parse_collected(&shown).unwrap();
    assert_eq!(c.push, "committed");
    assert_eq!(c.sha, "80a76646e07591c493b616771f08f74a161c5608");
}

#[test]
fn a_letter_without_a_record_is_none() {
    assert!(parse_collected("## a task\n\nbody").is_none());
}

#[test]
fn sent_handle() {
    assert_eq!(
        parse_sent("created hand e7024646\nsent 7fe3140e\nnode e7024646\n"),
        Some("7fe3140e".to_string())
    );
    assert_eq!(parse_sent("error"), None);
}

#[test]
fn a_throttled_agent_fails_as_provider() {
    let c = Collected {
        agent_rc: "1".into(),
        outcome: "finished".into(),
        receipt: "Error: 429 Too Many Requests (retry-after: 45)".into(),
        ..Collected::default()
    };
    assert!(!c.agent_ok());
    let err = format!("{:?}", agent_failure("implement", &c));
    assert!(err.contains("provider"), "{err}");
}
