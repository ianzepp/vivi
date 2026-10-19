//! Tests for memo tags, filters, since cutoffs, window labels, and the
//! `[memo]` config table.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, TimeZone, Utc};

use super::{MemoConfig, MemoFilter, hidden_tags, keep, parse_tags, since_cutoff, window_label};

fn eml(headers: &[(&str, &str)]) -> Vec<u8> {
    let mut raw = String::from("From: mind@local\r\nTo: memo@local\r\nSubject: t\r\n");
    for (name, value) in headers {
        raw.push_str(&format!("{name}: {value}\r\n"));
    }
    raw.push_str("\r\nbody\r\n");
    raw.into_bytes()
}

fn date(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(secs, 0).single().expect("valid timestamp")
}

fn hidden(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|tag| (*tag).to_string()).collect()
}

#[test]
fn parse_tags_round_trips_repeated_headers() {
    let raw = eml(&[
        ("X-Vivi-Tag", "alpha"),
        ("X-Vivi-Tag", "beta"),
        ("X-Vivi-Tag", "alpha"),
        ("X-VIVI-TAG", "  spaced  "),
        ("X-Vivi-Tag", ""),
        ("X-Other", "gamma"),
    ]);
    assert_eq!(
        parse_tags(&raw),
        vec!["alpha".to_string(), "beta".to_string(), "spaced".to_string()]
    );
}

#[test]
fn parse_tags_tolerates_garbage() {
    assert!(parse_tags(b"not an eml at all").is_empty());
}

#[test]
fn keep_hides_digest_by_default() {
    let filter = MemoFilter::default();
    assert!(!keep(
        Some(date(100)),
        &["digest".to_string()],
        &filter,
        &hidden(&["digest"])
    ));
}

#[test]
fn keep_tag_digest_unhides_and_selects() {
    let filter = MemoFilter {
        tags: vec!["digest".to_string()],
        include_hidden: true,
        ..MemoFilter::default()
    };
    assert!(keep(
        Some(date(100)),
        &["digest".to_string()],
        &filter,
        &hidden(&["digest"])
    ));
}

#[test]
fn keep_exclude_drops_matching_memos() {
    let filter = MemoFilter {
        exclude_tags: vec!["B".to_string()],
        ..MemoFilter::default()
    };
    assert!(!keep(
        Some(date(100)),
        &["a".to_string(), "b".to_string()],
        &filter,
        &hidden(&[])
    ));
    assert!(keep(
        Some(date(100)),
        &["a".to_string()],
        &filter,
        &hidden(&[])
    ));
}

#[test]
fn keep_requires_every_wanted_tag() {
    let filter = MemoFilter {
        tags: vec!["a".to_string(), "b".to_string()],
        ..MemoFilter::default()
    };
    assert!(!keep(Some(date(100)), &["a".to_string()], &filter, &hidden(&[])));
    assert!(keep(
        Some(date(100)),
        &["b".to_string(), "a".to_string()],
        &filter,
        &hidden(&[])
    ));
}

#[test]
fn keep_since_filters_on_view_date() {
    let filter = MemoFilter {
        since: Some(date(50)),
        ..MemoFilter::default()
    };
    assert!(keep(Some(date(50)), &[], &filter, &hidden(&[])));
    assert!(keep(Some(date(100)), &[], &filter, &hidden(&[])));
    assert!(!keep(Some(date(49)), &[], &filter, &hidden(&[])));
    assert!(!keep(None, &[], &filter, &hidden(&[])));
    let open = MemoFilter::default();
    assert!(keep(None, &[], &open, &hidden(&[])));
}

#[test]
fn keep_include_hidden_bypasses_the_hidden_set() {
    let filter = MemoFilter {
        include_hidden: true,
        ..MemoFilter::default()
    };
    assert!(keep(
        Some(date(100)),
        &["digest".to_string(), "noise".to_string()],
        &filter,
        &hidden(&["digest", "noise"])
    ));
}

#[test]
fn since_cutoff_accepts_duration_date_and_rfc3339() {
    let now = date(1_792_425_600);
    assert_eq!(
        since_cutoff("24h", now).expect("duration"),
        date(1_792_425_600 - 86_400)
    );
    assert_eq!(
        since_cutoff("2026-10-01", now).expect("date"),
        Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).single().expect("midnight")
    );
    assert_eq!(
        since_cutoff("2026-09-01T08:30:00+02:00", now).expect("rfc3339"),
        Utc.with_ymd_and_hms(2026, 9, 1, 6, 30, 0).single().expect("utc")
    );
}

#[test]
fn since_cutoff_rejects_unknown_forms_naming_the_argument() {
    let now = date(1_792_425_600);
    let error = since_cutoff("yesterday", now).expect_err("rejected");
    assert!(error.to_string().contains("yesterday"), "{error}");
}

#[test]
fn window_label_prefers_largest_clean_unit() {
    assert_eq!(window_label(172_800), "2d");
    assert_eq!(window_label(86_400), "1d");
    assert_eq!(window_label(7_200), "2h");
    assert_eq!(window_label(9_000), "150m");
    assert_eq!(window_label(59), "59s");
}

#[test]
fn memo_config_round_trips_through_mailspace_config() {
    let raw = "name = \"demo\"\n\n[memo]\nhidden_tags = [\"noise\"]\n\n[memo.retain]\ndigest = \"90d\"\n";
    let config: crate::mailspace::MailspaceConfig = toml::from_str(raw).expect("parse");
    let memo = config.memo.clone().expect("memo table");
    assert_eq!(memo.hidden_tags, vec!["noise".to_string()]);
    assert_eq!(memo.retain.get("digest").map(String::as_str), Some("90d"));

    let text = toml::to_string(&config).expect("serialize");
    let again: crate::mailspace::MailspaceConfig = toml::from_str(&text).expect("reparse");
    assert_eq!(again.memo, Some(memo));
}

#[test]
fn config_without_memo_table_stays_memoless_and_serializes_without_one() {
    let config: crate::mailspace::MailspaceConfig =
        toml::from_str("name = \"demo\"").expect("parse");
    assert_eq!(config.memo, None);
    let text = toml::to_string(&config).expect("serialize");
    assert!(!text.contains("memo"), "{text}");
}

#[test]
fn hidden_tags_always_includes_digest_plus_configured_entries() {
    assert_eq!(hidden_tags(&None), hidden(&["digest"]));
    let configured = Some(MemoConfig {
        hidden_tags: vec!["noise".to_string()],
        retain: BTreeMap::new(),
    });
    assert_eq!(hidden_tags(&configured), hidden(&["digest", "noise"]));
}
