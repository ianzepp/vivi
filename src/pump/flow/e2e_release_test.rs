//! End to end: the release flow on a real mailspace. A release need waits at
//! its `wait` milestone for the needs it depends on; each test is one outcome
//! of that milestone, driven only by the real tick.

use crate::error::VivariumError;
use crate::pump::config::PumpConfig;
use crate::pump::flow::e2e_rig::{Rig, rig};
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::mail::{MailReader, MailWriter, MailspacePort, NeedStatus};
use crate::pump::flow::milestone::{MILESTONE_PREREQ_HELD, MILESTONE_UNLANDED};
use crate::pump::flow::tick::OPERATOR_HOLD;

type R = Result<(), VivariumError>;

/// A release rig at its `wait` milestone, depending on one open prerequisite
/// need per subject (none has a pump record yet).
fn release(subjects: &[&str]) -> Result<(Rig, Vec<String>), VivariumError> {
    let mut rig = rig(PumpConfig::default(), "kind: release", "wait")?;
    let prereqs: Vec<String> = subjects
        .iter()
        .map(|subject| rig.project.need(subject))
        .collect();
    rig.project
        .mailspace
        .set_dependencies("mind", &rig.need, "need", &prereqs, &[])?;
    // The rig's port holds a mailspace cloned before the prerequisites were
    // sent; the pump would see them on its next open.
    rig.port = MailspacePort {
        mailspace: rig.project.mailspace.clone(),
        pump_identity: rig.port.pump_identity.clone(),
    };
    Ok((rig, prereqs))
}

/// Close `prereq` as the pump would: its pump record opened, the need
/// closed, then the record closed with `outcome`.
fn close_as(rig: &mut Rig, prereq: &str, outcome: &str) -> R {
    MailWriter::open_record(&mut rig.port, prereq, "kind: build")?;
    rig.port.close_need(prereq, "done")?;
    rig.port.close_record(prereq, outcome)
}

fn alarms(rig: &Rig, kind: &str) -> usize {
    rig.exceptions()
        .lines()
        .filter(|line| line.contains(kind))
        .count()
}

fn wait_step(rig: &Rig) -> Result<StepRow, VivariumError> {
    rig.row("wait")?
        .ok_or_else(|| VivariumError::Message("no wait step".into()))
}

fn run_state(rig: &Rig) -> Result<String, VivariumError> {
    rig.runs()?
        .last()
        .map(|run| run.state.clone())
        .ok_or_else(|| VivariumError::Message("no run".into()))
}

#[test]
fn a_release_lands_when_every_prerequisite_landed() -> R {
    let (mut rig, prereqs) = release(&["first prerequisite", "second prerequisite"])?;
    close_as(&mut rig, &prereqs[0], "landed")?;
    close_as(&mut rig, &prereqs[1], "landed")?;
    rig.tick_until("run done", |rig| run_state(rig).is_ok_and(|s| s == "done"))?;
    let step = wait_step(&rig)?;
    assert_eq!(step.state, StepState::Done);
    assert_eq!(step.verdict.as_deref(), Some("ready"));
    assert_eq!(rig.port.need_state(&rig.need)?, NeedStatus::Done);
    assert_eq!(alarms(&rig, MILESTONE_UNLANDED), 0, "{}", rig.exceptions());
    assert_eq!(
        alarms(&rig, MILESTONE_PREREQ_HELD),
        0,
        "{}",
        rig.exceptions()
    );
    Ok(())
}

#[test]
fn an_open_prerequisite_keeps_the_release_waiting_without_an_alarm() -> R {
    let (mut rig, prereqs) = release(&["landed prerequisite", "open prerequisite"])?;
    close_as(&mut rig, &prereqs[0], "landed")?;
    rig.tick()?;
    rig.tick()?;
    assert_eq!(run_state(&rig)?, "live");
    assert_eq!(wait_step(&rig)?.state, StepState::Waiting);
    assert_eq!(rig.port.need_state(&rig.need)?, NeedStatus::Open);
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

#[test]
fn a_withdrawn_prerequisite_holds_the_wait_step_with_one_alarm() -> R {
    let (mut rig, prereqs) = release(&["withdrawn prerequisite"])?;
    close_as(&mut rig, &prereqs[0], "withdrawn")?;
    rig.tick()?;
    rig.tick()?;
    let step = wait_step(&rig)?;
    assert_eq!(step.state, StepState::Waiting);
    assert_eq!(step.verdict.as_deref(), Some(OPERATOR_HOLD));
    assert_eq!(alarms(&rig, MILESTONE_UNLANDED), 1, "{}", rig.exceptions());
    let exceptions = rig.exceptions();
    assert!(exceptions.contains(&prereqs[0]), "{exceptions}");
    assert!(exceptions.contains("withdrawn"), "{exceptions}");
    rig.tick()?;
    rig.tick()?;
    assert_eq!(alarms(&rig, MILESTONE_UNLANDED), 1, "{}", rig.exceptions());
    assert_eq!(run_state(&rig)?, "live");
    assert_eq!(rig.port.need_state(&rig.need)?, NeedStatus::Open);
    Ok(())
}

#[test]
fn a_prerequisite_held_at_a_lock_alarms_once_and_the_release_keeps_waiting() -> R {
    let (mut rig, prereqs) = release(&["locked prerequisite"])?;
    let prereq = &prereqs[0];
    rig.store.insert_run(&RunRow {
        need: prereq.clone(),
        run: 1,
        flow: "build".into(),
        flow_version: 1,
        kind: "build".into(),
        entry: "locked".into(),
        state: "live".into(),
    })?;
    rig.store.insert_step(
        prereq,
        &StepRow {
            run: 1,
            node: "locked".into(),
            task: "t-locked".into(),
            state: StepState::Waiting,
            verdict: None,
        },
    )?;
    rig.tick()?;
    rig.tick()?;
    assert_eq!(
        alarms(&rig, MILESTONE_PREREQ_HELD),
        1,
        "{}",
        rig.exceptions()
    );
    let exceptions = rig.exceptions();
    assert!(exceptions.contains(prereq), "{exceptions}");
    assert!(exceptions.contains("held at locked"), "{exceptions}");
    assert_eq!(alarms(&rig, MILESTONE_UNLANDED), 0, "{exceptions}");
    let step = wait_step(&rig)?;
    assert_eq!(step.state, StepState::Waiting);
    assert_ne!(step.verdict.as_deref(), Some(OPERATOR_HOLD));
    assert_eq!(run_state(&rig)?, "live");
    Ok(())
}
