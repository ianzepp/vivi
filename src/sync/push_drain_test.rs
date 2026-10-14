// Drain tests kept beside `push_test` so neither file nears the line ceiling.

use std::cell::Cell;

use crate::sync::fake_server::{Checkout, FakeServer, checkout};
use crate::sync::push;

fn alice() -> Checkout {
    checkout("alice", &["mind", "hand", "peer"])
}

/// A push that stops rate limited after sending some letters is a limit, not
/// a failure: it exits 0 and says how many letters are left.
#[test]
fn a_rate_limited_push_after_sent_letters_is_not_a_failure() {
    let server = FakeServer::new();
    let a = alice();
    let client = a.client(&server);
    for subject in ["one", "two", "three", "four"] {
        a.send_need("mind", "hand", subject);
    }
    server.rate_limit_after(2);

    let report = a.push(&client);

    assert_eq!(report.sent, 2, "letters before the limit still went");
    assert!(
        report.failure().is_none(),
        "a rate limit is a limit, not a failure: {:?}",
        report.lines()
    );
    assert_eq!(
        report.lines()[1],
        "stopped: rate limited; 2 letter(s) left unsent; run again later or use --drain"
    );
}

/// A drain whose every pass is rate limited with nothing sent gives up after
/// three passes and reports the stuck server as a failure.
#[test]
fn a_drain_that_stalls_thrice_reports_a_failure() {
    let server = FakeServer::new();
    let a = alice();
    let client = a.client(&server);
    for n in 0..4 {
        a.send_need("mind", "hand", &format!("stuck-{n}"));
    }
    server.rate_limit_after(0);
    let passes = Cell::new(0usize);
    let waits = Cell::new(0usize);

    let report = push::push_drain(
        &a.mailspace,
        &a.settings,
        &client,
        None,
        &mut |_| waits.set(waits.get() + 1),
        &mut |_, _| passes.set(passes.get() + 1),
    )
    .unwrap();

    assert_eq!(passes.get(), 3, "three stalled passes end the drain");
    assert_eq!(
        waits.get(),
        2,
        "a wait between stalled passes, none after the last"
    );
    assert_eq!(report.sent, 0);
    let stopped = report.stopped.as_ref().expect("the drain stopped");
    assert!(
        !stopped.rate_limited,
        "the giving-up stop reports as a failure"
    );
    assert!(stopped.reason.contains("429"), "{}", stopped.reason);
    assert!(report.failure().unwrap().contains("4 letter(s) unsent"));
}
