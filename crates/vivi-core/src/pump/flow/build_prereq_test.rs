//! The shared build prerequisite assessment over a fake mailspace and a temp
//! pump store.

use tempfile::{TempDir, tempdir};

use super::{Assessment, PREREQ_HELD, PREREQ_UNLANDED, PREREQ_UNREADABLE, assess};
use crate::pump::config::PumpConfig;
use crate::pump::flow::def::FlowKind;
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::mail::{MailReader, MailWriter, NeedStatus, TaskState};
use crate::pump::flow::tick::OPERATOR_HOLD;
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::store::PumpStore;
use crate::pump::store::build_waits::{BuildPrerequisite, BuildWait, PrerequisiteState};
use vivi_error::VivariumError;
use vivi_mail::letter::fixture::record_body;

type R = Result<(), VivariumError>;

const NEED: &str = "n1";

struct Rig {
    _dir: TempDir,
    ctx: Ctx,
    store: PumpStore,
    mail: FakeMail,
}

fn rig() -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let store = PumpStore::open(&ctx.home.join(crate::pump::store::DB_FILENAME))?;
    Ok(Rig {
        _dir: dir,
        ctx,
        store,
        mail: FakeMail::new("mind"),
    })
}

impl Rig {
    fn record(&mut self, kind: &str) -> R {
        MailWriter::open_record(&mut self.mail, NEED, &record_body(NEED, kind)).map(drop)
    }

    fn assess(&self) -> Result<Assessment, VivariumError> {
        assess(&self.ctx, &self.store, &self.mail, NEED)
    }

    fn kinds(&self) -> Result<Vec<String>, VivariumError> {
        Ok(self
            .store
            .exceptions()?
            .into_iter()
            .map(|(_, kind, _, _)| kind)
            .collect())
    }

    fn count(&self, kind: &str) -> Result<usize, VivariumError> {
        Ok(self.kinds()?.into_iter().filter(|k| k == kind).count())
    }

    /// A live run of `handle` with one waiting step at `node`.
    fn waiting_at(&self, handle: &str, node: &str, verdict: Option<&str>) -> R {
        self.store.insert_run(&RunRow {
            need: handle.into(),
            run: 1,
            flow: "build".into(),
            flow_version: 2,
            kind: "build".into(),
            entry: "plan".into(),
            state: "live".into(),
        })?;
        self.store.insert_step(
            handle,
            &StepRow {
                run: 1,
                node: node.into(),
                task: format!("t-{handle}-{node}"),
                state: StepState::Waiting,
                verdict: verdict.map(str::to_owned),
            },
        )
    }

    fn closed_with(&mut self, handle: &str, outcome: &str) -> R {
        MailWriter::open_record(&mut self.mail, handle, &record_body(handle, "build"))?;
        MailWriter::close_record(&mut self.mail, handle, outcome)?;
        MailWriter::close_need(&mut self.mail, handle, "done")
    }
}

#[test]
fn all_landed_prerequisites_admit_and_remove_the_wait() -> R {
    let mut rig = rig()?;
    rig.record("build")?;
    rig.mail.set_prerequisites(NEED, &["a", "b"]);
    MailWriter::close_need(&mut rig.mail, "a", "done")?;
    MailWriter::close_need(&mut rig.mail, "b", "done")?;
    let got = rig.assess()?;
    assert_eq!(got.kind, FlowKind::Build);
    assert!(got.eligible);
    assert!(got.blockers.is_empty());
    assert!(rig.store.build_waits()?.is_empty());
    Ok(())
}

#[test]
fn a_need_with_no_open_record_is_eligible() -> R {
    let rig = rig()?;
    let got = rig.assess()?;
    assert_eq!(got.kind, FlowKind::Build);
    assert!(got.eligible);
    Ok(())
}

#[test]
fn multiple_open_blockers_wait_silently_and_are_all_retained() -> R {
    let mut rig = rig()?;
    rig.record("build")?;
    rig.mail.set_prerequisites(NEED, &["a", "b"]);
    let got = rig.assess()?;
    assert!(!got.eligible);
    let handles: Vec<&str> = got.blockers.iter().map(|b| b.handle.as_str()).collect();
    assert_eq!(handles, ["a", "b"]);
    assert!(
        got.blockers
            .iter()
            .all(|b| b.state == PrerequisiteState::Open)
    );
    let waits = rig.store.build_waits()?;
    assert_eq!(waits.len(), 1);
    assert_eq!(waits[0].need, NEED);
    assert_eq!(waits[0].prerequisites, got.blockers);
    assert_eq!(rig.count(PREREQ_HELD)?, 0);
    assert_eq!(rig.count(PREREQ_UNLANDED)?, 0);
    Ok(())
}

#[test]
fn mixed_blockers_keep_every_one_and_raise_both_diagnostics() -> R {
    let mut rig = rig()?;
    rig.record("build")?;
    rig.mail.set_prerequisites(NEED, &["open", "held", "gone"]);
    rig.waiting_at("held", "locked", None)?;
    rig.closed_with("gone", "withdrawn")?;
    let got = rig.assess()?;
    assert!(!got.eligible);
    let states: Vec<PrerequisiteState> = got.blockers.iter().map(|b| b.state).collect();
    assert_eq!(
        states,
        [
            PrerequisiteState::Open,
            PrerequisiteState::Held,
            PrerequisiteState::ClosedUnlanded,
        ]
    );
    assert_eq!(rig.count(PREREQ_HELD)?, 1);
    assert_eq!(rig.count(PREREQ_UNLANDED)?, 1);
    Ok(())
}

#[test]
fn lock_park_and_operator_hold_are_all_held() -> R {
    for (node, verdict) in [
        ("locked", None),
        ("parked", None),
        ("gate", Some(OPERATOR_HOLD)),
    ] {
        let mut rig = rig()?;
        rig.record("build")?;
        rig.mail.set_prerequisites(NEED, &["a"]);
        rig.waiting_at("a", node, verdict)?;
        let got = rig.assess()?;
        assert_eq!(got.blockers.len(), 1, "{node}");
        assert_eq!(got.blockers[0].state, PrerequisiteState::Held, "{node}");
        assert_eq!(rig.count(PREREQ_HELD)?, 1, "{node}");
    }
    Ok(())
}

#[test]
fn withdrawn_superseded_and_empty_outcomes_are_closed_unlanded() -> R {
    for (outcome, detail) in [
        ("withdrawn", "withdrawn"),
        ("superseded", "superseded"),
        ("", "closed"),
    ] {
        let mut rig = rig()?;
        rig.record("build")?;
        rig.mail.set_prerequisites(NEED, &["a"]);
        rig.closed_with("a", outcome)?;
        let got = rig.assess()?;
        assert_eq!(got.blockers[0].state, PrerequisiteState::ClosedUnlanded);
        assert_eq!(
            got.blockers[0].outcome.as_deref(),
            Some(detail),
            "outcome {outcome:?}"
        );
        assert_eq!(rig.count(PREREQ_UNLANDED)?, 1, "outcome {outcome:?}");
    }
    Ok(())
}

/// A reader whose dependency read fails.
struct Failing<'a>(&'a FakeMail);

impl MailReader for Failing<'_> {
    fn task_state(&self, handle: &str) -> Result<TaskState, VivariumError> {
        self.0.task_state(handle)
    }
    fn need_state(&self, need: &str) -> Result<NeedStatus, VivariumError> {
        self.0.need_state(need)
    }
    fn need_marks(&self, need: &str) -> Result<Vec<String>, VivariumError> {
        self.0.need_marks(need)
    }
    fn open_record(&self, need: &str) -> Result<Option<(String, String)>, VivariumError> {
        self.0.open_record(need)
    }
    fn bound_units(&self, need: &str) -> Result<Vec<String>, VivariumError> {
        self.0.bound_units(need)
    }
    fn sentinel_count(&self, need: &str) -> Result<usize, VivariumError> {
        self.0.sentinel_count(need)
    }
    fn need_prerequisites(&self, _need: &str) -> Result<Vec<String>, VivariumError> {
        Err(VivariumError::Message("graph unreadable".into()))
    }
    fn last_closed_record(&self, need: &str) -> Result<Option<String>, VivariumError> {
        self.0.last_closed_record(need)
    }
    fn sent_step(
        &self,
        subject: &str,
        step: &vivi_mail::letter::work::StepFront,
    ) -> Result<Option<String>, VivariumError> {
        self.0.sent_step(subject, step)
    }
}

#[test]
fn a_read_error_fails_closed_and_keeps_the_previous_wait() -> R {
    let mut rig = rig()?;
    rig.record("build")?;
    let previous = BuildWait {
        need: NEED.into(),
        record: "rec".into(),
        prerequisites: vec![BuildPrerequisite {
            handle: "a".into(),
            state: PrerequisiteState::Open,
            detail: None,
            outcome: None,
        }],
        observed_at: 7,
    };
    rig.store.upsert_build_wait(&previous)?;
    let got = assess(&rig.ctx, &rig.store, &Failing(&rig.mail), NEED);
    assert!(got.is_err());
    assert_eq!(rig.store.build_waits()?, vec![previous]);
    assert_eq!(rig.count(PREREQ_UNREADABLE)?, 1);
    Ok(())
}

#[test]
fn non_build_kinds_skip_a_failing_prerequisite_read() -> R {
    for (record_kind, marks, expected) in [
        ("research", &[][..], FlowKind::Research),
        ("release", &[][..], FlowKind::Release),
        ("", &["repair"][..], FlowKind::Repair),
    ] {
        let mut rig = rig()?;
        rig.record(record_kind)?;
        rig.mail.set_marks(NEED, marks);
        rig.store.upsert_build_wait(&BuildWait {
            need: NEED.into(),
            record: "stale-record".into(),
            prerequisites: Vec::new(),
            observed_at: 1,
        })?;

        let got = assess(&rig.ctx, &rig.store, &Failing(&rig.mail), NEED)?;
        assert_eq!(got.kind, expected);
        assert!(got.eligible);
        assert!(got.blockers.is_empty());
        assert!(rig.store.build_waits()?.is_empty());
        assert_eq!(rig.count(PREREQ_HELD)?, 0);
        assert_eq!(rig.count(PREREQ_UNLANDED)?, 0);
        assert_eq!(rig.count(PREREQ_UNREADABLE)?, 0);
    }
    Ok(())
}

#[test]
fn the_record_kind_wins_over_the_marks() -> R {
    let mut rig = rig()?;
    rig.record("build")?;
    rig.mail.set_marks(NEED, &["release"]);
    rig.mail.set_prerequisites(NEED, &["a"]);
    let got = rig.assess()?;
    assert_eq!(got.kind, FlowKind::Build);
    assert!(!got.eligible);
    Ok(())
}

#[test]
fn a_non_build_need_is_never_gated_and_loses_its_wait() -> R {
    let mut rig = rig()?;
    rig.record("release")?;
    rig.mail.set_prerequisites(NEED, &["a"]);
    rig.store.upsert_build_wait(&BuildWait {
        need: NEED.into(),
        record: "rec".into(),
        prerequisites: Vec::new(),
        observed_at: 1,
    })?;
    let got = rig.assess()?;
    assert_eq!(got.kind, FlowKind::Release);
    assert!(got.eligible);
    assert!(rig.store.build_waits()?.is_empty());
    assert_eq!(rig.count(PREREQ_HELD)?, 0);
    Ok(())
}

#[test]
fn one_alarm_across_repeated_assessments_and_an_actual_reopen() -> R {
    let mut rig = rig()?;
    rig.record("build")?;
    rig.mail.set_prerequisites(NEED, &["a"]);
    rig.waiting_at("a", "locked", None)?;
    let path = rig.ctx.home.join(crate::pump::store::DB_FILENAME);
    assert!(!rig.assess()?.eligible);
    assert!(!rig.assess()?.eligible);
    assert_eq!(rig.count(PREREQ_HELD)?, 1);
    let reopened = PumpStore::open(&path)?;
    let again = assess(&rig.ctx, &reopened, &rig.mail, NEED)?;
    assert!(!again.eligible);
    assert_eq!(
        reopened
            .exceptions()?
            .into_iter()
            .filter(|(_, kind, _, _)| kind == PREREQ_HELD)
            .count(),
        1
    );
    Ok(())
}
