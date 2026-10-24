//! Tests for the `vivi pump replan` CLI half: it posts a replan action on the
//! need's open pump record and refuses a need with none.

use std::collections::BTreeSet;

use crate::cli::PumpReplanCommand;
use crate::mailspace::PumpAction;
use crate::pump::fixture::{Project, project};
use crate::pump::replan::run;
use crate::pump::state::State;
use crate::pump::store::{DB_FILENAME, PumpStore};

/// Create the project's `pump.db`.
fn seed_db(project: &Project) {
    let mut store = PumpStore::open(&project.home().join(DB_FILENAME)).unwrap();
    store.save(&State::default()).unwrap();
}

fn command(need: &str) -> PumpReplanCommand {
    PumpReplanCommand {
        need: need.to_owned(),
        project: None,
    }
}

/// The actions posted and not yet applied.
fn posted(project: &Project) -> Vec<PumpAction> {
    project
        .mailspace
        .pump_pending_actions(&BTreeSet::new())
        .unwrap()
}

#[test]
fn run_replan_posts_one_action_on_the_open_record() {
    let project = project();
    let need = project.need("fix auth");
    let record = project.mailspace.pump_send(None, &need, None).unwrap();
    seed_db(&project);

    run(project.root(), &command(&need)).unwrap();

    let actions = posted(&project);
    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0].record, record.handle);
    assert_eq!(actions[0].need, need);
}

#[test]
fn run_replan_without_an_open_record_is_refused_and_says_how_to_open_one() {
    let project = project();
    let need = project.need("fix auth");
    seed_db(&project);

    let error = run(project.root(), &command(&need)).unwrap_err();

    let text = error.to_string();
    assert!(text.contains("no open pump record"), "{text}");
    assert!(
        text.contains(&format!("vivi pump send --need {need}")),
        "{text}"
    );
    assert!(posted(&project).is_empty());
}

#[test]
fn run_replan_without_a_pump_database_is_refused() {
    let project = project();

    let error = run(project.root(), &command("n1")).unwrap_err();

    assert!(error.to_string().contains("no pump.db at"));
}
