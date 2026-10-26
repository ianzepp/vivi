use super::*;

fn micros(stamp: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(stamp)
        .unwrap()
        .timestamp_micros()
}

#[test]
fn an_event_is_stamped_at_the_clock_when_nothing_is_newer() {
    let now = micros("2026-10-12T12:00:00Z");
    assert_eq!(event_at(now, None), "2026-10-12T12:00:00.000000Z");
    // The newest counted event is older than the clock: the clock wins.
    assert_eq!(
        event_at(now, Some(micros("2026-10-12T11:59:00Z"))),
        "2026-10-12T12:00:00.000000Z"
    );
}

#[test]
fn an_event_is_stamped_one_microsecond_past_the_newest() {
    let now = micros("2026-10-12T12:00:00Z");
    let ahead = micros("2026-10-12T12:03:00Z");
    assert_eq!(event_at(now, Some(ahead)), "2026-10-12T12:03:00.000001Z");
    // Equal to the clock is not enough: strictly above.
    assert_eq!(event_at(now, Some(now)), "2026-10-12T12:00:00.000001Z");
}

#[test]
fn a_stamp_that_cannot_be_read_back_falls_back_to_the_clock() {
    // Year 10000 formats with a five-digit year; a reader would not
    // parse it, so the writer stamps the clock instead.
    let year_10000 = 253_402_300_800_000_000;
    let stamp = event_at(year_10000, None);
    assert!(
        chrono::DateTime::parse_from_rfc3339(&stamp).is_ok(),
        "{stamp}"
    );
    assert!(stamp.starts_with("20"), "{stamp}");
    // So does an instant chrono cannot hold at all.
    let stamp = event_at(i64::MAX, None);
    assert!(
        chrono::DateTime::parse_from_rfc3339(&stamp).is_ok(),
        "{stamp}"
    );
    assert!(stamp.starts_with("20"), "{stamp}");
}

fn record(raw: Vec<u8>) -> ProjectRecord {
    ProjectRecord {
        sequence: 1,
        message_id: "msg_abcd12340000000000000000".to_owned(),
        from: "a".to_owned(),
        from_prefix: None,
        to: None,
        kind: Some("need".to_owned()),
        content_sha256: String::new(),
        appended_at: "2026-10-23T00:00:00Z".to_owned(),
        raw_eml: raw,
    }
}

/// Headers in front of a composed letter, the shape `of_message` reads.
fn drafted(headers: &str, body: &str) -> Vec<u8> {
    let drafted = crate::message::build_compose_draft(&crate::message::ComposeDraft {
        from: "a@b.local".to_owned(),
        to: vec!["a@b.local".to_owned()],
        cc: Vec::new(),
        bcc: Vec::new(),
        subject: "s".to_owned(),
        body: body.to_owned(),
        html_body: None,
    })
    .expect("compose");
    format!("{headers}{drafted}").into_bytes()
}

#[test]
fn graph_dependencies_come_from_front_matter() {
    let need = "+++\ndone_when = [\"landed\"]\ndepends_on = [\"abcd1234\"]\n+++\n\nprose\n";
    let formatted = drafted("X-Vivi-Format: 1\r\nX-Vivi-Kind: need\r\n", need);
    assert_eq!(depends_of(&record(formatted)), vec!["abcd1234".to_string()]);

    let task = drafted(
        "X-Vivi-Format: 1\r\nX-Vivi-Kind: task\r\n",
        "+++\ndepends_on = [\"task00aa\"]\n+++\n\nstep\n",
    );
    assert_eq!(depends_of(&record(task)), vec!["task00aa".to_string()]);

    let want = drafted(
        "X-Vivi-Format: 1\r\nX-Vivi-Kind: want\r\n",
        "+++\nquestion = \"q\"\ndepends_on = [\"want00bb\"]\n+++\n\nwhy\n",
    );
    assert_eq!(depends_of(&record(want)), vec!["want00bb".to_string()]);

    // No format header: legacy, even when the body carries a front.
    let legacy = drafted("X-Vivi-Kind: need\r\n", need);
    assert!(depends_of(&record(legacy)).is_empty());

    // A claimed format that does not parse contributes nothing, and does
    // not error.
    let broken = drafted(
        "X-Vivi-Format: 1\r\nX-Vivi-Kind: need\r\n",
        "not a letter\n",
    );
    assert!(depends_of(&record(broken)).is_empty());
    let unknown = drafted("X-Vivi-Format: 2\r\nX-Vivi-Kind: need\r\n", need);
    assert!(depends_of(&record(unknown)).is_empty());
}
