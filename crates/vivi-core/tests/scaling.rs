//! Row-count scaling guard.
//!
//! A read that is fine at 40 items and ruinous at 40,000 shows up here as a
//! ratio. The test seeds a mailspace with N and then 2N needs (a fifth as
//! many wants, each served by a need, half of those served needs done), runs
//! one operation on each, and compares the rows the bulk loaders returned
//! (`Storage::scan_snapshot`). A linear operation reads twice the rows at 2N;
//! a quadratic one reads four times. No timers are involved, so the result is
//! the same on every machine.

use vivi_core::mailspace::board::BoardOptions;
use vivi_core::mailspace::{Mailspace, SendRequest, WantListOptions};
use vivi_mail::letter::fixture::{need_body_with, want_body};
use vivi_mail::letter::work::NeedFront;

/// Needs in the small mailspace; the large one has twice as many.
const SMALL: usize = 40;
/// Largest `ops(2N) / ops(N)` a linear operation may show. Exactly linear is
/// 2.0; the headroom covers fixed per-operation reads.
const MAX_RATIO: f64 = 2.5;

fn send(mailspace: &Mailspace, kind: &str, body: String) -> String {
    let result = mailspace
        .send(SendRequest {
            from: "mind".into(),
            to: vec!["hand".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: format!("{kind} subject"),
            body,
            role: format!("{kind}s"),
            kind: Some(kind.into()),
            reply_to: None,
        })
        .unwrap();
    result.delivered[0].handle.clone()
}

fn send_need(mailspace: &Mailspace, serves: Option<&str>) -> String {
    let mut front = NeedFront::new(vec!["done".to_string()]);
    front.serves = serves.map(str::to_string);
    send(mailspace, "need", need_body_with(&front, "prose"))
}

/// A mailspace with `needs` needs and `needs / 5` wants. Each want is served
/// by one of the needs, and every second serving need is closed.
fn seeded(needs: usize) -> (tempfile::TempDir, Mailspace, Vec<String>) {
    let tmp = tempfile::tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(tmp.path())).unwrap();
    mailspace.add_identity("mind").unwrap();
    mailspace.add_identity("hand").unwrap();
    let wants: Vec<String> = (0..needs / 5)
        .map(|_| send(&mailspace, "want", want_body("q", "p")))
        .collect();
    for index in 0..needs {
        let serves = wants.get(index).map(String::as_str);
        let handle = send_need(&mailspace, serves);
        if serves.is_some() && index % 2 == 0 {
            mailspace
                .move_item("mind", &handle, "done", None, "close", None)
                .unwrap();
        }
    }
    (tmp, mailspace, wants)
}

/// Rows and blobs read while `operation` runs against a mailspace of `needs`.
fn cost(needs: usize, operation: impl Fn(&Mailspace, &[String])) -> u64 {
    let (_tmp, mailspace, wants) = seeded(needs);
    let before = mailspace.storage().unwrap().scan_snapshot();
    operation(&mailspace, &wants);
    let after = mailspace.storage().unwrap().scan_snapshot();
    after.since(&before).total()
}

fn assert_scales_linearly(name: &str, operation: impl Fn(&Mailspace, &[String]) + Copy) {
    let small = cost(SMALL, operation);
    let large = cost(SMALL * 2, operation);
    let ratio = large as f64 / small.max(1) as f64;
    eprintln!(
        "{name}: ops({SMALL}) = {small}, ops({}) = {large}, ratio {ratio:.2}",
        SMALL * 2
    );
    assert!(
        ratio <= MAX_RATIO,
        "{name} reads {small} rows at {SMALL} needs and {large} at {}: ratio {ratio:.2} \
         exceeds {MAX_RATIO}, so the cost grows faster than the mailspace",
        SMALL * 2
    );
}

fn board_options(with_graph: bool) -> BoardOptions {
    BoardOptions {
        wants_cap: 10_000,
        with_graph,
        ..BoardOptions::default()
    }
}

#[test]
fn board_reads_rows_linearly() {
    assert_scales_linearly("board", |mailspace, _| {
        mailspace.board(&board_options(false)).unwrap();
    });
}

#[test]
fn board_with_graph_reads_rows_linearly() {
    assert_scales_linearly("board --graph", |mailspace, _| {
        mailspace.board(&board_options(true)).unwrap();
    });
}

#[test]
fn want_list_reads_rows_linearly() {
    assert_scales_linearly("want list", |mailspace, _| {
        let records = mailspace
            .list_wants_by_state(None, None, WantListOptions::default())
            .unwrap();
        assert!(!records.is_empty());
    });
}

#[test]
fn one_more_need_send_reads_rows_linearly() {
    assert_scales_linearly("need send", |mailspace, wants| {
        send_need(mailspace, Some(&wants[0]));
    });
}
