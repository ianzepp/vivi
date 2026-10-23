//! The unrepaired node: answers `green` when main is green at a tip that
//! contains the tested tip; otherwise one alarm naming target, tips and both
//! attempts, no answer, the step Waiting and the need open.

use std::fs;
use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use super::{MAIN_UNREPAIRED, Unrepaired};
use crate::error::VivariumError;
use crate::mailspace::marked_line;
use crate::pump::config::PumpConfig;
use crate::pump::flow::def::FlowKind;
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::mail::{MailReader, MailWriter};
use crate::pump::flow::main_rung;
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::flow::repair::{self, AttemptFacts};
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::store::step_payload::StepPayloadRow;
use crate::pump::store::{DB_FILENAME, PumpStore};

type R = Result<(), VivariumError>;

const NEED: &str = "abcd1234";
const TESTED: &str = "feedface00000000000000000000000000000000";

struct Rig {
    _dir: TempDir,
    ctx: Ctx,
    host: Arc<FakeHost>,
    store: PumpStore,
    mail: FakeMail,
}

fn rig(record: Option<&str>) -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let mut mail = FakeMail::new("mind");
    if let Some(body) = record {
        MailWriter::open_record(&mut mail, NEED, body)?;
    }
    Ok(Rig {
        _dir: dir,
        ctx,
        host,
        store,
        mail,
    })
}

fn run() -> RunRow {
    RunRow {
        need: NEED.into(),
        run: 1,
        flow: "repair".into(),
        flow_version: 2,
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
    assert!(
        log.contains("attempt 1: no result published nothing"),
        "{log}"
    );
    assert!(
        log.contains("attempt 2: no result published nothing"),
        "{log}"
    );
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
    assert!(log.contains("no open record"), "{log}");
    assert!(log.contains("attempt 2:"), "{log}");
    Ok(())
}

const MAIN_NEW: &str = "aaaa000000000000000000000000000000000001";
const MAIN_OLD: &str = "bbbb000000000000000000000000000000000002";

fn main_record() -> String {
    format!("kind: repair\ntarget: default\ntested: {TESTED}\n\nFAILED a::b\n")
}

fn main_is(rig: &Rig, tip: &str, green: &str) -> R {
    rig.host
        .reply("ls-remote", 0, &format!("{tip}\trefs/heads/main\n"));
    rig.store
        .set_main_state(&main_rung::key(main_rung::GREEN, "default"), green)
}

fn main_contains(rig: &Rig, code: i32) {
    rig.host.reply("cat-file -e", 0, "");
    rig.host.reply("merge-base --is-ancestor", code, "");
}

#[test]
fn green_main_that_contains_the_tested_tip_answers_green() -> R {
    let rig = rig(Some(&main_record()))?;
    main_is(&rig, MAIN_NEW, MAIN_NEW)?;
    main_contains(&rig, 0);
    assert_eq!(poll(&rig)?, Some("green".into()));
    assert!(!exceptions(&rig).contains(MAIN_UNREPAIRED));
    Ok(())
}

#[test]
fn a_last_green_older_than_the_main_tip_raises_and_holds() -> R {
    let rig = rig(Some(&main_record()))?;
    main_is(&rig, MAIN_NEW, MAIN_OLD)?;
    main_contains(&rig, 0);
    assert_eq!(poll(&rig)?, None);
    let log = exceptions(&rig);
    assert_eq!(log.matches(MAIN_UNREPAIRED).count(), 1, "{log}");
    assert!(log.contains(&format!("main tip {MAIN_NEW}")), "{log}");
    assert!(log.contains(&format!("last green {MAIN_OLD}")), "{log}");
    assert!(log.contains(TESTED), "{log}");
    Ok(())
}

#[test]
fn a_green_tip_without_the_tested_tip_holds() -> R {
    let rig = rig(Some(&main_record()))?;
    main_is(&rig, MAIN_NEW, MAIN_NEW)?;
    main_contains(&rig, 1);
    assert_eq!(poll(&rig)?, None);
    assert_eq!(exceptions(&rig).matches(MAIN_UNREPAIRED).count(), 1);
    Ok(())
}

#[test]
fn a_record_without_a_tested_tip_holds() -> R {
    let rig = rig(Some("kind: repair\ntarget: default\n\nFAILED a::b\n"))?;
    main_is(&rig, MAIN_NEW, MAIN_NEW)?;
    main_contains(&rig, 0);
    assert_eq!(poll(&rig)?, None);
    assert!(exceptions(&rig).contains("tested unknown"));
    Ok(())
}

#[test]
fn a_git_error_is_named_in_the_alarm_and_holds() -> R {
    let rig = rig(Some(&main_record()))?;
    rig.host.reply("ls-remote", 128, "no route to remote");
    assert_eq!(poll(&rig)?, None);
    let log = exceptions(&rig);
    assert!(log.contains("no route to remote"), "{log}");
    Ok(())
}

fn done(rig: &Rig, node: &str, task: &str, verdict: &str) -> R {
    rig.store.insert_step(
        NEED,
        &StepRow {
            run: 1,
            node: node.into(),
            task: task.into(),
            state: StepState::Done,
            verdict: Some(verdict.into()),
        },
    )
}

#[test]
fn the_alarm_names_each_attempt_word_and_what_it_published() -> R {
    let mut rig = rig(Some(&main_record()))?;
    main_is(&rig, MAIN_NEW, MAIN_OLD)?;
    let fix1 = rig.mail.send_step("mind", "mind", "fix1", "b", &[])?;
    let fix2 = rig.mail.send_step("mind", "mind", "fix2", "b", &[])?;
    done(&rig, "fix1", &fix1, "failed")?;
    done(&rig, "fix2", &fix2, "built")?;
    done(&rig, "rung2", "t-rung2", "fail")?;
    rig.store.record_step_payload(&StepPayloadRow {
        need: NEED.into(),
        run: 1,
        node: "fix1".into(),
        class: "hand".into(),
        task: fix1,
        payload: serde_json::json!({"reason": "built but nothing published"}),
    })?;
    let facts = AttemptFacts {
        branch: repair::branch(NEED, 1, 2),
        commit: "c".repeat(40),
        base: MAIN_OLD.into(),
    };
    rig.mail.reply(
        &fix2,
        &marked_line(repair::ATTEMPT_MARKER, &repair::facts_text(&facts)),
    )?;
    assert_eq!(poll(&rig)?, None);
    let log = exceptions(&rig);
    assert!(
        log.contains("attempt 1: failed published nothing (built but nothing published)"),
        "{log}"
    );
    let published = format!(
        "attempt 2: fail published {} {}",
        facts.branch, facts.commit
    );
    assert!(log.contains(&published), "{log}");
    Ok(())
}
