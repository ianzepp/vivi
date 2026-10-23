//! The unrepaired node: one alarm naming target, tested tip and both attempt
//! branches; never answers; leaves the step Waiting and the need open.

use std::fs;

use tempfile::{TempDir, tempdir};

use super::{MAIN_UNREPAIRED, Unrepaired};
use crate::error::VivariumError;
use crate::pump::config::PumpConfig;
use crate::pump::flow::def::FlowKind;
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::mail::{MailReader, MailWriter};
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::store::{DB_FILENAME, PumpStore};

type R = Result<(), VivariumError>;

const NEED: &str = "abcd1234";
const TESTED: &str = "feedface00000000000000000000000000000000";

struct Rig {
    _dir: TempDir,
    ctx: Ctx,
    store: PumpStore,
    mail: FakeMail,
}

fn rig(record: Option<&str>) -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let mut mail = FakeMail::new("mind");
    if let Some(body) = record {
        MailWriter::open_record(&mut mail, NEED, body)?;
    }
    Ok(Rig {
        _dir: dir,
        ctx,
        store,
        mail,
    })
}

fn run() -> RunRow {
    RunRow {
        need: NEED.into(),
        run: 1,
        flow: "repair".into(),
        flow_version: 1,
        kind: "repair".into(),
        entry: "fix1".into(),
        state: "live".into(),
    }
}

fn step() -> StepRow {
    StepRow {
        run: 1,
        node: "unrepaired".into(),
        task: "t9".into(),
        state: StepState::Waiting,
        verdict: None,
    }
}

fn poll(rig: &Rig) -> Result<Option<String>, VivariumError> {
    let def = load(FlowKind::Repair)?;
    let run = run();
    let cx = StartCx {
        ctx: &rig.ctx,
        store: &rig.store,
        mail: &rig.mail,
        run: &run,
        def: &def,
    };
    Unrepaired.poll(&cx, &step())
}

fn exceptions(rig: &Rig) -> String {
    fs::read_to_string(rig.ctx.home.join("exceptions.txt")).unwrap_or_default()
}

#[test]
fn start_opens_and_waits() -> R {
    let rig = rig(None)?;
    let def = load(FlowKind::Repair)?;
    let run = run();
    let cx = StartCx {
        ctx: &rig.ctx,
        store: &rig.store,
        mail: &rig.mail,
        run: &run,
        def: &def,
    };
    assert_eq!(Unrepaired.start(&cx, &step())?, StartAction::OpenAndWait);
    Ok(())
}

#[test]
fn polling_twice_raises_one_alarm_and_never_answers() -> R {
    let record = format!("kind: repair\ntarget: vivi\ntested: {TESTED}\n\nFAILED a::b\n");
    let rig = rig(Some(&record))?;
    assert_eq!(poll(&rig)?, None);
    assert_eq!(poll(&rig)?, None);
    let log = exceptions(&rig);
    assert_eq!(log.matches(MAIN_UNREPAIRED).count(), 1, "{log}");
    assert!(log.contains("vivi"), "{log}");
    assert!(log.contains(TESTED), "{log}");
    assert!(log.contains("vivi/repair/abcd1234-r1-a1"), "{log}");
    assert!(log.contains("vivi/repair/abcd1234-r1-a2"), "{log}");
    assert!(log.contains("vivi pump unlock abcd1234"), "{log}");
    assert!(
        MailReader::open_record(&rig.mail, NEED)?.is_some(),
        "need stays open"
    );
    Ok(())
}

#[test]
fn an_unreadable_record_still_raises() -> R {
    let rig = rig(None)?;
    assert_eq!(poll(&rig)?, None);
    let log = exceptions(&rig);
    assert_eq!(log.matches(MAIN_UNREPAIRED).count(), 1, "{log}");
    assert!(log.contains("vivi/repair/abcd1234-r1-a2"), "{log}");
    Ok(())
}
