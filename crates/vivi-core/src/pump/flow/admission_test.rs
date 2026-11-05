//! Tests for side-effect-free prerequisite admission evaluation.

use tempfile::{TempDir, tempdir};

use super::{AdmissionState, evaluate};
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::mail::MailWriter;
use crate::pump::store::{DB_FILENAME, PumpStore};
use vivi_error::VivariumError;

type R = Result<(), VivariumError>;
const NEED: &str = "need";

struct Rig {
    _dir: TempDir,
    store: PumpStore,
    mail: FakeMail,
}

fn rig(deps: &[&str]) -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|error| VivariumError::Other(error.to_string()))?;
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let mut mail = FakeMail::new("pump");
    mail.set_prerequisites(NEED, deps);
    Ok(Rig {
        _dir: dir,
        store,
        mail,
    })
}

fn waiting_run(rig: &Rig, need: &str, kind: &str, node: &str) -> R {
    rig.store.insert_run(&RunRow {
        need: need.into(),
        run: 1,
        flow: kind.into(),
        flow_version: 2,
        kind: kind.into(),
        entry: node.into(),
        state: "live".into(),
    })?;
    rig.store.insert_step(
        need,
        &StepRow {
            run: 1,
            node: node.into(),
            task: format!("task-{need}"),
            state: StepState::Waiting,
            verdict: None,
        },
    )?;
    Ok(())
}

#[test]
fn empty_and_all_landed_dependencies_are_ready() -> R {
    let empty = rig(&[])?;
    assert_eq!(
        evaluate(&empty.store, &empty.mail, NEED).state,
        AdmissionState::Ready
    );

    let mut landed = rig(&["done"])?;
    landed.mail.close_need("done", "operator completed")?;
    assert_eq!(
        evaluate(&landed.store, &landed.mail, NEED).state,
        AdmissionState::Ready
    );
    Ok(())
}

#[test]
fn a_done_prerequisite_without_a_pump_record_is_landed() -> R {
    let mut rig = rig(&["done"])?;
    rig.mail.close_need("done", "done")?;

    let admission = evaluate(&rig.store, &rig.mail, NEED);

    assert_eq!(admission.state, AdmissionState::Ready);
    assert!(admission.details.is_empty());
    Ok(())
}

#[test]
fn held_dependency_blocks_even_when_another_dependency_is_open() -> R {
    let rig = rig(&["open", "held"])?;
    waiting_run(&rig, "held", "build", "locked")?;

    let admission = evaluate(&rig.store, &rig.mail, NEED);

    assert_eq!(admission.state, AdmissionState::Blocked);
    assert_eq!(admission.details.len(), 2);
    assert_eq!(admission.details[0].handle, "held");
    assert_eq!(admission.details[1].handle, "open");
    Ok(())
}

#[test]
fn closed_unlanded_dependency_blocks_with_its_outcome() -> R {
    let mut rig = rig(&["open", "closed"])?;
    rig.mail.open_record("closed", "build")?;
    rig.mail.close_record("closed", "withdrawn")?;
    rig.mail.close_need("closed", "closed")?;

    let admission = evaluate(&rig.store, &rig.mail, NEED);

    assert_eq!(admission.state, AdmissionState::Blocked);
    assert_eq!(admission.details.len(), 2);
    assert_eq!(admission.details[0].handle, "closed");
    assert_eq!(admission.details[1].handle, "open");
    Ok(())
}

#[test]
fn dependencies_are_sorted_and_deduplicated() -> R {
    let rig = rig(&["z", "a", "z"])?;

    let admission = evaluate(&rig.store, &rig.mail, NEED);

    assert_eq!(admission.state, AdmissionState::Waiting);
    assert_eq!(
        admission
            .details
            .iter()
            .map(|p| p.handle.as_str())
            .collect::<Vec<_>>(),
        ["a", "z"]
    );
    Ok(())
}

#[test]
fn evaluation_errors_are_unreadable_not_ready_or_waiting() -> R {
    let rig = rig(&["bad"])?;
    waiting_run(&rig, "bad", "unknown-kind", "locked")?;

    let admission = evaluate(&rig.store, &rig.mail, NEED);

    assert!(matches!(admission.state, AdmissionState::Unreadable(_)));
    assert!(admission.details.is_empty());
    Ok(())
}
