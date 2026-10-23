//! Prerequisite classification over a fake mailspace and a temp pump store.

use tempfile::{TempDir, tempdir};

use super::{Prereq, PrereqState, prerequisites};
use crate::error::VivariumError;
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::mail::MailWriter;
use crate::pump::store::{DB_FILENAME, PumpStore};

type R = Result<(), VivariumError>;

const REL: &str = "rel1";
const DEP: &str = "dep1";

struct Rig {
    _dir: TempDir,
    store: PumpStore,
    mail: FakeMail,
}

fn rig() -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let mut mail = FakeMail::new("mind");
    mail.set_prerequisites(REL, &[DEP]);
    Ok(Rig {
        _dir: dir,
        store,
        mail,
    })
}

impl Rig {
    fn classify(&self) -> Result<Vec<Prereq>, VivariumError> {
        prerequisites(&self.store, &self.mail, REL)
    }

    fn state(&self) -> Result<PrereqState, VivariumError> {
        let all = self.classify()?;
        all.into_iter()
            .find(|p| p.handle == DEP)
            .map(|p| p.state)
            .ok_or_else(|| VivariumError::Message("no prerequisite".into()))
    }

    /// A live build run of the prerequisite with one waiting step.
    fn waiting_at(&self, node: &str, verdict: Option<&str>) -> R {
        self.store.insert_run(&RunRow {
            need: DEP.into(),
            run: 1,
            flow: "build".into(),
            flow_version: 1,
            kind: "build".into(),
            entry: "plan".into(),
            state: "live".into(),
        })?;
        self.store.insert_step(
            DEP,
            &StepRow {
                run: 1,
                node: node.into(),
                task: format!("t-{node}"),
                state: StepState::Waiting,
                verdict: verdict.map(str::to_owned),
            },
        )
    }
}

#[test]
fn a_landed_record_is_landed() -> R {
    let mut rig = rig()?;
    rig.mail.open_record(DEP, "kind: build")?;
    rig.mail.close_record(DEP, "landed")?;
    rig.mail.close_need(DEP, "done")?;
    assert_eq!(rig.state()?, PrereqState::Landed);
    Ok(())
}

#[test]
fn a_done_need_with_no_record_is_landed() -> R {
    let mut rig = rig()?;
    rig.mail.close_need(DEP, "done")?;
    assert_eq!(rig.state()?, PrereqState::Landed);
    Ok(())
}

#[test]
fn a_record_closed_withdrawn_is_closed_unlanded() -> R {
    let mut rig = rig()?;
    rig.mail.open_record(DEP, "kind: build")?;
    rig.mail.close_record(DEP, "withdrawn")?;
    rig.mail.close_need(DEP, "done")?;
    assert_eq!(
        rig.state()?,
        PrereqState::ClosedUnlanded("withdrawn".into())
    );
    Ok(())
}

#[test]
fn an_open_need_with_a_run_waiting_at_locked_is_held() -> R {
    let rig = rig()?;
    rig.waiting_at("locked", None)?;
    match rig.state()? {
        PrereqState::Held(what) => assert!(what.contains("locked"), "{what}"),
        other => panic!("expected Held, got {other:?}"),
    }
    Ok(())
}

#[test]
fn an_open_need_with_no_run_is_open() -> R {
    let rig = rig()?;
    assert_eq!(rig.state()?, PrereqState::Open);
    Ok(())
}
