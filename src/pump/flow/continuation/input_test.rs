//! Tests for the run-input snapshots: the reply filter, same-second
//! ordering, the baseline diff, the conservative boundary of a run without
//! a baseline, and the baseline a start journals.

use super::input::{baseline, new_inputs, snapshots};
use crate::error::VivariumError;
use crate::mailspace::MailspaceThreadMessage;
use crate::pump::fixture::{Project, project};
use crate::pump::flow::advance::start::start;
use crate::pump::flow::mail::{MailWriter, MailspacePort};
use crate::pump::store::DB_FILENAME;
use crate::pump::store::PumpStore;
use crate::pump::store::flow_continuation::ThreadInput;

type R = Result<(), VivariumError>;

const PUMP: &str = "mind";
const T: &str = "2026-10-20T10:00:00Z";

/// A thread message with only the fields the snapshots read.
fn msg(
    id: &str,
    from: &str,
    date: &str,
    subject: &str,
    body: &str,
    kind: Option<&str>,
    event: Option<&str>,
) -> MailspaceThreadMessage {
    MailspaceThreadMessage {
        handle: format!("h-{id}"),
        message_id: id.to_string(),
        content_id: format!("c-{id}"),
        account: "fixture".to_string(),
        role: "inbox".to_string(),
        kind: kind.map(str::to_string),
        event: event.map(str::to_string),
        date: date.to_string(),
        from: from.to_string(),
        to: "mind".to_string(),
        cc: String::new(),
        subject: subject.to_string(),
        body: body.to_string(),
        parent_content_id: None,
        link_source: None,
        inferred: false,
        marks: Vec::new(),
        close_branch: None,
        close_commit: None,
    }
}

/// An ordinary reply on the thread.
fn reply(id: &str, from: &str, date: &str, body: &str) -> MailspaceThreadMessage {
    msg(id, from, date, "need1", body, None, None)
}

fn store_of(project: &Project) -> PumpStore {
    PumpStore::open(&project.home().join(DB_FILENAME)).unwrap()
}

fn ids(snaps: &[super::input::ThreadSnapshot]) -> Vec<&str> {
    snaps
        .iter()
        .map(|snapshot| snapshot.input.message_id.as_str())
        .collect()
}

#[test]
fn snapshots_keep_replies_and_drop_pump_traffic() {
    let thread = vec![
        reply("a", "operator", T, "first"),
        reply("b", "mind", T, "pump progress on the run"),
        msg(
            "c",
            "operator",
            T,
            "need1",
            "marked done",
            None,
            Some("done"),
        ),
        msg(
            "d",
            "operator",
            T,
            "need1",
            "record body",
            Some("pump"),
            None,
        ),
        msg(
            "e",
            "operator",
            T,
            "pump close action",
            "action body",
            None,
            None,
        ),
        reply("f", "elsewhere", T, "second"),
    ];
    let snaps = snapshots(&thread, PUMP).unwrap();
    assert_eq!(ids(&snaps), vec!["a", "f"], "only the two replies survive");
    assert_eq!(snaps[0].position, 0);
    assert_eq!(snaps[1].position, 5, "positions count every message");
    assert_eq!(snaps[0].input.author, "operator");
    assert_eq!(snaps[0].input.body, "first");
}

#[test]
fn same_second_replies_stay_distinct_in_thread_order() {
    let thread = vec![
        reply("a", "operator", T, "first"),
        reply("b", "elsewhere", T, "second"),
        reply("c", "operator", T, "third"),
    ];
    let snaps = snapshots(&thread, PUMP).unwrap();
    assert_eq!(snaps.len(), 3, "one second, three distinct replies");
    assert_eq!(ids(&snaps), vec!["a", "b", "c"], "thread order is kept");
    assert_eq!(snaps[0].input.sent_at, snaps[2].input.sent_at);
    assert_ne!(
        snaps[0].input.message_id, snaps[2].input.message_id,
        "the message id, not the timestamp, tells the replies apart"
    );
}

#[test]
fn an_unparsable_date_is_an_error_naming_the_text() {
    let thread = vec![reply("a", "operator", "not a date", "hello")];
    let error = snapshots(&thread, PUMP).unwrap_err();
    assert!(error.to_string().contains("not a date"), "{error}");
}

#[test]
fn new_inputs_diffs_the_baseline_by_message_id() {
    let project = project();
    let store = store_of(&project);
    let thread = vec![
        reply("a", "operator", T, "shown at start"),
        reply("b", "operator", T, "also shown at start"),
        reply("c", "elsewhere", T, "arrived later"),
    ];
    // Journalled out of thread order: the diff follows identities, not ordinals.
    store
        .journal_run_inputs("need1", 1, &[input("b"), input("a")])
        .unwrap();
    let news = new_inputs(&thread, PUMP, &store, "need1", 1).unwrap();
    assert_eq!(ids(&news), vec!["c"], "a and b are in the baseline");
}

#[test]
fn a_historical_run_bounds_new_input_at_its_start_post() {
    let project = project();
    let store = store_of(&project);
    let thread = vec![
        reply("old", "operator", T, "before the run"),
        reply(
            "start",
            "mind",
            T,
            "pump: run r1 started (build, entry plan)",
        ),
        reply("new1", "operator", T, "after the run began"),
        msg(
            "event",
            "operator",
            T,
            "need1",
            "state change",
            None,
            Some("marked"),
        ),
        reply("new2", "elsewhere", T, "also after"),
    ];
    let news = new_inputs(&thread, PUMP, &store, "need1", 1).unwrap();
    assert_eq!(ids(&news), vec!["new1", "new2"]);
    assert_eq!(
        baseline(&store, "need1", 1).unwrap(),
        None,
        "the earlier reply is left alone, not claimed consumed"
    );
}

#[test]
fn without_baseline_and_start_post_the_boundary_is_an_error() {
    let project = project();
    let store = store_of(&project);
    let thread = vec![reply("a", "operator", T, "no start post anywhere")];
    let error = new_inputs(&thread, PUMP, &store, "need1", 1).unwrap_err();
    assert!(
        error.to_string().contains("flow-thread-check-failed"),
        "{error}"
    );
}

#[test]
fn a_start_journals_the_replies_visible_before_it_began() -> R {
    let project = project();
    let ctx = project.pump_ctx();
    let need = project.need("baseline me");
    project.mailspace.reply(
        &need,
        "operator",
        vec!["mind".into()],
        Vec::new(),
        None,
        "before the run".into(),
    )?;
    let store = store_of(&project);
    let mut port = MailspacePort {
        mailspace: project.mailspace.clone(),
        pump_identity: PUMP.into(),
    };
    let row = start(&ctx, &store, &mut port, &need, None)?;
    assert_eq!(row.state, "live");
    let shown = baseline(&store, &need, row.run)?.unwrap();
    let bodies: Vec<&str> = shown.iter().map(|input| input.body.as_str()).collect();
    // The need letter itself and the reply: everything on the thread that is
    // not pump traffic, in thread order.
    assert_eq!(bodies.len(), 2, "{bodies:?}");
    assert!(bodies[0].contains("done_when"), "{bodies:?}");
    assert_eq!(bodies[1], "before the run\n");
    assert!(shown[1].author.starts_with("operator"), "{shown:?}");
    Ok(())
}

/// One journalled baseline input.
fn input(id: &str) -> ThreadInput {
    ThreadInput {
        message_id: id.to_string(),
        author: "operator".to_string(),
        body: "journalled".to_string(),
        sent_at: 0,
    }
}
