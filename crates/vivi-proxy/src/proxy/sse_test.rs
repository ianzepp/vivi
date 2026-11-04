use super::sse::{self, Event};

#[test]
fn splits_events_on_blank_line() {
    let mut buffer = b"data: one\n\ndata: two\n\npartial".to_vec();
    let events = sse::take_events(&mut buffer);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].data().as_deref(), Some("one"));
    assert_eq!(events[1].data().as_deref(), Some("two"));
    assert_eq!(buffer, b"partial");
}

#[test]
fn reads_named_event() {
    let event = Event {
        raw: "event: ping\ndata: {}\n\n".into(),
    };
    assert_eq!(event.name(), Some("ping"));
}
