use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use super::*;

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

#[test]
fn the_heartbeat_is_thirty_seconds() {
    assert_eq!(HEARTBEAT, secs(30));
}

#[test]
fn no_beat_comes_due_before_the_interval() {
    let mut beat = Beat::new(HEARTBEAT);
    for s in [0, 1, 8, 16, 29] {
        assert!(!beat.due(secs(s)), "at {s}s");
    }
}

#[test]
fn a_beat_comes_due_once_per_interval() {
    let mut beat = Beat::new(HEARTBEAT);
    let due: Vec<u64> = (0..=100)
        .step_by(8)
        .filter(|s| beat.due(secs(*s)))
        .collect();
    assert_eq!(due, [32, 64, 96], "polls every 8s, so beats follow a poll");
}

#[test]
fn progress_lines_reach_the_sink_unless_quiet() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&seen);
    let loud = Progress::to(false, move |l| sink.borrow_mut().push(l.to_string()));
    loud.say("one");
    assert_eq!(*seen.borrow(), ["one"]);
    let sink = Rc::clone(&seen);
    let quiet = Progress::to(true, move |l| sink.borrow_mut().push(l.to_string()));
    quiet.say("two");
    Progress::silent().say("three");
    assert_eq!(*seen.borrow(), ["one"]);
}
