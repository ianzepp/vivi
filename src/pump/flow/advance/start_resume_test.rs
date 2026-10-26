//! Idempotent run start: each write of `start` fails once, the `starting`
//! run is resumed, and the need ends with one run and no duplicate tasks.

use super::{SENTINEL_NODE, resume, start};
use crate::error::VivariumError;
use crate::letter::read::step_of;
use crate::pump::fixture::{Project, project};
use crate::pump::flow::continuation::input::new_inputs;
use crate::pump::flow::fake_mail::{Call, FakeMail};
use crate::pump::flow::index::StepState;
use crate::pump::flow::index::run_state;
use crate::pump::flow::index::{FLOW_VERSION_NUMBER, RunRow};
use crate::pump::flow::mail::MailReader;
use crate::pump::host::Ctx;
use crate::pump::store::{DB_FILENAME, PumpStore};

const PUMP: &str = "mind";

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
        assert_eq!(runs[0].state, run_state::STARTING, "write {write}");

        let row = resume(&ctx, &store, &mut mail, &runs[0]).unwrap();
        assert_eq!(row.state, "live", "write {write}");

        let runs = store.runs_of(NEED).unwrap();
        assert_eq!(runs.len(), 1, "write {write}");
        assert_eq!(runs[0].state, "live", "write {write}");
        assert_eq!(count(&mail, |c| matches!(c, Call::SendStep { .. })), 2);
        let sentinels = mail
            .tasks
            .values()
            .filter(|t| step_of(&t.body).is_some_and(|s| s.sentinel))
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

/// A reply by the operator on the need's real thread.
fn reply_on(project: &Project, need: &str, body: &str) -> Result<(), VivariumError> {
    project
        .mailspace
        .reply(
            need,
            "operator",
            vec![PUMP.into()],
            Vec::new(),
            None,
            body.into(),
        )
        .map(|_| ())
}

#[test]
fn an_interrupted_start_keeps_its_baseline_when_it_resumes() -> Result<(), VivariumError> {
    let (project, ctx, store, mut mail) = setup();
    let need = project.need("interrupt me");
    reply_on(&project, &need, "before the run")?;
    // The sentinel send fails: the run row and its baseline are in, no step is.
    mail.fail_at = Some(0);
    start(&ctx, &store, &mut mail, &need, None).unwrap_err();

    // A reply that arrives while the run sits between the two attempts.
    reply_on(&project, &need, "while stuck")?;
    let runs = store.runs_of(&need).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].state, run_state::STARTING);

    resume(&ctx, &store, &mut mail, &runs[0]).unwrap();
    let baseline = store.run_inputs(&need, 1).unwrap();
    let bodies: Vec<&str> = baseline.iter().map(|input| input.body.as_str()).collect();
    assert_eq!(
        bodies,
        vec![
            "+++\ndone_when = [\"it works\"]\n+++\n\n",
            "before the run\n"
        ],
        "the reply from the crash window must not be swallowed into the baseline"
    );

    // And it is still pending input for the run, by identity.
    let thread = project
        .mailspace
        .thread(&need, false, usize::MAX, usize::MAX)
        .unwrap();
    let news = new_inputs(&thread, PUMP, &store, &need, 1).unwrap();
    let bodies: Vec<&str> = news.iter().map(|snap| snap.input.body.as_str()).collect();
    assert_eq!(bodies, vec!["while stuck\n"]);
    Ok(())
}

#[test]
fn a_recovery_that_never_captured_journals_before_it_sends() -> Result<(), VivariumError> {
    let (project, ctx, store, mut mail) = setup();
    let need = project.need("recover me");
    reply_on(&project, &need, "waiting on the thread")?;
    let row = RunRow {
        need: need.clone(),
        run: 1,
        flow: "build".into(),
        flow_version: FLOW_VERSION_NUMBER,
        kind: "build".into(),
        entry: "plan".into(),
        state: run_state::STARTING.to_string(),
    };
    store.insert_run(&row).unwrap();
    assert!(store.run_inputs(&need, 1).unwrap().is_empty());

    resume(&ctx, &store, &mut mail, &row).unwrap();
    let baseline = store.run_inputs(&need, 1).unwrap();
    let bodies: Vec<&str> = baseline.iter().map(|input| input.body.as_str()).collect();
    assert_eq!(
        bodies,
        vec![
            "+++\ndone_when = [\"it works\"]\n+++\n\n",
            "waiting on the thread\n"
        ]
    );
    Ok(())
}
