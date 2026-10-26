//! Provider-error classification over finished-job evidence.
//!
//! Consumes a finished job's [`Collected`] and answers the shared
//! [`ProviderErrorClass`] from [`super::types`] — nothing more: no cooldown
//! timer, no pool-host identity, and never a rerun or a receipt rewrite.
//!
//! Evidence rules:
//! - Structured result evidence must say the harness ended terminally and
//!   unsuccessfully: `state=done` with a parseable nonzero `agent_rc`.
//!   Absent or unparsable evidence counts as not failed. A
//!   `push=ramdisk_full` result is a disk-quota failure, never a provider
//!   error, and refuses classification outright.
//! - The stderr tail and the runner log tail are searched for recognizable
//!   HTTP 429 responses and provider JSON `code` values only, so prose
//!   ("retried 429 times", "rate limits") and example code never match.
//! - The receipt (agent stdout) is inspected only when it is recognizably a
//!   raw provider error response: a JSON object, an HTTP status line, or a
//!   status-prefixed JSON body such as the Z.AI `429:{"code":"1302"}`.
//!   Arbitrary task prose is never searched.

use serde_json::Value;

use super::types::ProviderErrorClass;
use crate::pool::collect::{Collected, JobResult};

/// Provider JSON `code` values that name an exhausted budget.
const QUOTA_CODES: &[&str] = &["insufficient_quota", "quota_exceeded"];
/// Provider JSON `code` values that name throttling, Z.AI's `1302` among
/// them.
const RATE_LIMIT_CODES: &[&str] = &["1302", "429", "rate_limit", "rate_limit_exceeded"];
/// Lowercased HTTP status lines that say throttling.
const HTTP_429_LINES: &[&str] = &["http/1.0 429", "http/1.1 429", "http/2 429", "http/2.0 429"];

/// The provider-error class of a finished job's left-behind evidence, or
/// `None` when the evidence does not name provider throttling or quota.
/// Quota codes win over rate-limit evidence when both appear.
#[must_use]
pub fn classify(collected: &Collected) -> Option<ProviderErrorClass> {
    if !harness_failed(&collected.result) {
        return None;
    }
    classify_text(&collected.stderr)
        .or_else(|| classify_text(&collected.log))
        .or_else(|| classify_raw_response(&collected.receipt))
}

/// Structured evidence of a terminal, unsuccessful harness run: `state=done`
/// with a parseable nonzero `agent_rc`.
fn harness_failed(result: &JobResult) -> bool {
    if result.get("push") == "ramdisk_full" {
        return false;
    }
    result.get("state") == "done"
        && result
            .get("agent_rc")
            .parse::<i64>()
            .is_ok_and(|rc| rc != 0)
}

/// Classify a log tail: provider JSON `code` values first, then HTTP 429
/// status lines. Plain prose never matches.
fn classify_text(text: &str) -> Option<ProviderErrorClass> {
    if QUOTA_CODES.iter().any(|code| has_json_code(text, code)) {
        return Some(ProviderErrorClass::Quota);
    }
    if RATE_LIMIT_CODES
        .iter()
        .any(|code| has_json_code(text, code))
    {
        return Some(ProviderErrorClass::RateLimit);
    }
    http_429(text).then_some(ProviderErrorClass::RateLimit)
}

/// `"code":"<word>"` or `"code": "<word>"` appearing literally in a log tail.
fn has_json_code(text: &str, word: &str) -> bool {
    text.contains(&format!(r#""code":"{word}""#)) || text.contains(&format!(r#""code": "{word}""#))
}

/// A recognizable HTTP 429 response line in a log tail.
fn http_429(text: &str) -> bool {
    let lower = text.to_lowercase();
    HTTP_429_LINES.iter().any(|line| lower.contains(line))
}

/// Classify the receipt only when it is recognizably a raw provider error
/// response: a JSON object, an HTTP status line, or a status-prefixed JSON
/// body such as the Z.AI `429:{"code":"1302"}`. Task prose is never read.
fn classify_raw_response(receipt: &str) -> Option<ProviderErrorClass> {
    let body = receipt.trim();
    if body.starts_with('{') {
        return json_codes(body);
    }
    if body.starts_with("429:") {
        return json_codes(body.trim_start_matches("429:").trim_start());
    }
    if body.to_lowercase().starts_with("http/") {
        return http_429(body).then_some(ProviderErrorClass::RateLimit);
    }
    None
}

/// The provider-error class of a raw JSON response body, when it parses.
fn json_codes(text: &str) -> Option<ProviderErrorClass> {
    match serde_json::from_str::<Value>(text) {
        Ok(response) => codes(&response),
        Err(_) => None,
    }
}

/// The provider `code` values of a raw JSON response, at any depth. Quota
/// codes win over rate-limit codes when both appear.
fn codes(response: &Value) -> Option<ProviderErrorClass> {
    let mut found = Vec::new();
    collect_codes(response, &mut found);
    if found
        .iter()
        .any(|code| QUOTA_CODES.contains(&code.as_str()))
    {
        return Some(ProviderErrorClass::Quota);
    }
    found
        .iter()
        .any(|code| RATE_LIMIT_CODES.contains(&code.as_str()))
        .then_some(ProviderErrorClass::RateLimit)
}

/// Gather every `code` field of a JSON response, string or number, walking
/// objects and arrays.
fn collect_codes(value: &Value, found: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            if let Some(code) = map.get("code") {
                found.push(match code {
                    Value::String(text) => text.clone(),
                    Value::Number(number) => number.to_string(),
                    _ => String::new(),
                });
            }
            map.values().for_each(|nested| collect_codes(nested, found));
        }
        Value::Array(items) => items.iter().for_each(|item| collect_codes(item, found)),
        _ => {}
    }
}

#[cfg(test)]
#[path = "provider_error_test.rs"]
mod tests;
