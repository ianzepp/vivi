//! Idempotent run start: each write of `start` fails once, the `starting`
//! run is resumed, and the need ends with one run and no duplicate tasks.

use super::{RUN_STARTING, SENTINEL_NODE, resume, start};
use crate::pump::fixture::{Project, project};
use crate::pump::flow::def::SENTINEL_MARKER;
use crate::pump::flow::fake_mail::{Call, FakeMail};
use crate::pump::flow::index::StepState;
use crate::pump::flow::mail::MailReader;
use crate::pump::host::Ctx;
use crate::pump::store::{DB_FILENAME, PumpStore};

const NEED: &str = "need1";

fn setup() -> (Project, Ctx, PumpStore, FakeMail) {
    let project = project();
    let ctx = project.pump_ctx();
    let store = PumpStore::open(&project.home().join(DB_FILENAME)).unwrap();
    let mail = FakeMail::new("pump");
    (project, ctx, store, mail)
}

fn count(mail: &FakeMail, pred: impl Fn(&Call) -> bool) -> usize {
    mail.calls.iter().filter(|c| pred(c)).count()
}

#[test]
fn a_failure_at_any_write_resumes_to_one_live_run() {
    // The four writes of a start, in order: sentinel send, step send, bind, post.
    for write in 0..4 {
        let (_project, ctx, store, mut mail) = setup();
        mail.fail_at = Some(write);
        assert!(
            start(&ctx, &store, &mut mail, NEED, None).is_err(),
            "write {write} should fail"
        );
        let runs = store.runs_of(NEED).unwrap();
        assert_eq!(runs.len(), 1, "write {write}");
        assert_eq!(runs[0].state, RUN_STARTING, "write {write}");

        let row = resume(&ctx, &store, &mut mail, &runs[0]).unwrap();
        assert_eq!(row.state, "live", "write {write}");

        let runs = store.runs_of(NEED).unwrap();
        assert_eq!(runs.len(), 1, "write {write}");
        assert_eq!(runs[0].state, "live", "write {write}");
        assert_eq!(count(&mail, |c| matches!(c, Call::SendStep { .. })), 2);
        let sentinels = mail
            .tasks
            .values()
            .filter(|t| t.body.starts_with(SENTINEL_MARKER))
            .count();
        assert_eq!(sentinels, 1, "write {write}");
        let steps = store.steps_of(NEED, 1).unwrap();
        assert_eq!(steps.len(), 2, "write {write}");
        let sentinel = steps.iter().find(|s| s.node == SENTINEL_NODE).unwrap();
        let entry = steps.iter().find(|s| s.node == "plan").unwrap();
        assert_eq!(entry.state, StepState::Pending);
        let bound = mail.bound_units(NEED).unwrap();
        assert!(bound.contains(&sentinel.task), "write {write}");
        assert!(bound.contains(&entry.task), "write {write}");
        assert_eq!(mail.sentinel_count(NEED).unwrap(), 1, "write {write}");
    }
}

#[test]
fn a_second_start_resumes_the_starting_run_instead_of_opening_run_two() {
    let (_project, ctx, store, mut mail) = setup();
    mail.fail_at = Some(2);
    start(&ctx, &store, &mut mail, NEED, None).unwrap_err();

    let row = start(&ctx, &store, &mut mail, NEED, Some("research")).unwrap();
    assert_eq!(row.run, 1);
    assert_eq!(row.entry, "plan", "a resume keeps the recorded entry");
    assert_eq!(row.state, "live");
    assert_eq!(store.runs_of(NEED).unwrap().len(), 1);
    assert_eq!(count(&mail, |c| matches!(c, Call::SendStep { .. })), 2);
}

#[test]
fn the_run_number_follows_the_store_when_nothing_was_bound() {
    let (_project, ctx, store, mut mail) = setup();
    mail.fail_at = Some(0);
    start(&ctx, &store, &mut mail, NEED, None).unwrap_err();
    let runs = store.runs_of(NEED).unwrap();
    store.set_run_state(NEED, 1, "superseded").unwrap();
    assert_eq!(runs.len(), 1);

    let next = start(&ctx, &store, &mut mail, NEED, None).unwrap();
    assert_eq!(
        next.run, 2,
        "no sentinel was bound, the store still has run 1"
    );
}
