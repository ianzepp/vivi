//! Repairer start: a job from the current main tip, the need_branches row,
//! the prior-attempt brief.

use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use super::RepairerStart;
use crate::error::VivariumError;
use crate::mailspace::marked_line;
use crate::pump::config::PumpConfig;
use crate::pump::flow::def::FlowKind;
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::flow::repair::{self, AttemptFacts};
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::store::{DB_FILENAME, PumpStore};
use crate::routing::RoutedRole;

type R = Result<(), VivariumError>;

const NEED: &str = "abcd1234";
const MAIN_A: &str = "1111111111111111111111111111111111111111";
const MAIN_B: &str = "2222222222222222222222222222222222222222";
const RECORD: &str = "kind: repair\ntarget: default\ntested: 0000\n\nFAILED a::b\n";

struct Rig {
    _dir: TempDir,
    ctx: Ctx,
    host: Arc<FakeHost>,
    store: PumpStore,
    mail: FakeMail,
}

fn rig(role: bool, record: &str) -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (mut ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    if role {
        let mut routing = (*ctx.routing).clone();
        routing.roles.push(RoutedRole {
            name: String::from("repairer"),
            kind: Some(String::from("repairer")),
            status: String::from("active"),
            pool: Some(String::from("test-planner-pool")),
            read_only: None,
            model: None,
            models: Vec::new(),
        });
        ctx = ctx.with_routing(routing);
    }
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let mut mail = FakeMail::new("mind");
    MailWriter::open_record(&mut mail, NEED, record)?;
    Ok(Rig {
        _dir: dir,
        ctx,
        host,
        store,
        mail,
    })
}

fn main_tip(host: &FakeHost, sha: &str) {
    host.reply(
        "ls-remote origin main",
        0,
        &format!("{sha}\trefs/heads/main\n"),
    );
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

fn step(node: &str, task: &str, state: StepState, verdict: Option<&str>) -> StepRow {
    StepRow {
        run: 1,
        node: node.into(),
        task: task.into(),
        state,
        verdict: verdict.map(str::to_owned),
    }
}

fn start(rig: &Rig, step: &StepRow) -> Result<StartAction, VivariumError> {
    let def = load(FlowKind::Repair)?;
    let run = run();
    let cx = StartCx {
        ctx: &rig.ctx,
        store: &rig.store,
        mail: &rig.mail,
        run: &run,
        def: &def,
    };
    RepairerStart.start(&cx, step)
}

fn job(action: StartAction) -> Box<crate::pump::jobs::JobSpec> {
    match action {
        StartAction::Job(spec) => spec,
        other => panic!("expected a job, got {other:?}"),
    }
}

#[test]
fn attempt_one_cuts_a_repair_branch_from_the_main_tip() -> R {
    let rig = rig(true, RECORD)?;
    main_tip(&rig.host, MAIN_A);
    let spec = job(start(&rig, &step("fix1", "t1", StepState::Pending, None))?);
    assert_eq!(spec.kind, "repairer");
    assert_eq!(spec.role, "repairer");
    assert_eq!(spec.task.as_deref(), Some("t1"));
    assert_eq!(spec.target.as_deref(), Some("default"));
    assert_eq!(spec.from_ref.as_deref(), Some(MAIN_A));
    assert_eq!(spec.branch.as_deref(), Some("vivi/repair/abcd1234-r1-a1"));
    assert_eq!(
        spec.publish_ref.as_deref(),
        Some("refs/heads/vivi/repair/abcd1234-r1-a1")
    );
    assert!(!spec.read_only);
    assert_eq!(spec.budget, 1800);
    assert!(spec.body.contains("Failing output:\nFAILED a::b"));
    assert!(spec.body.contains(MAIN_A));
    assert!(!spec.body.contains("Prior attempt"));
    let row = rig.store.need_branch(NEED)?.expect("need_branches row");
    assert_eq!(row.branch, "vivi/need/abcd1234");
    assert_eq!(row.base.as_deref(), Some(MAIN_A));
    assert_eq!(row.status, "live");
    assert_eq!(row.run, 1);
    assert_eq!(row.target, "default");
    Ok(())
}

#[test]
fn attempt_two_reads_a_new_tip_and_names_the_rung_verdict() -> R {
    let mut rig = rig(true, RECORD)?;
    let fix = rig.mail.send_step("mind", "mind", "fix1", "b", &[])?;
    let rung = rig.mail.send_step("mind", "mind", "rung1", "b", &[])?;
    rig.store
        .insert_step(NEED, &step("fix1", &fix, StepState::Done, Some("built")))?;
    rig.store
        .insert_step(NEED, &step("rung1", &rung, StepState::Done, Some("fail")))?;
    let facts = AttemptFacts {
        branch: repair::branch(NEED, 1, 1),
        commit: "c".repeat(40),
        base: MAIN_A.into(),
    };
    rig.mail.reply(
        &fix,
        &marked_line(repair::ATTEMPT_MARKER, &repair::facts_text(&facts)),
    )?;
    rig.mail.reply(
        &rung,
        &marked_line(repair::RUNG_MARKER, "failed on a::b::c_test"),
    )?;
    main_tip(&rig.host, MAIN_B);
    let spec = job(start(&rig, &step("fix2", "t2", StepState::Pending, None))?);
    assert_eq!(spec.from_ref.as_deref(), Some(MAIN_B));
    assert_eq!(spec.branch.as_deref(), Some("vivi/repair/abcd1234-r1-a2"));
    assert!(spec.body.contains("Prior attempt"));
    assert!(
        spec.body.contains("- rung1: Done, verdict fail"),
        "{}",
        spec.body
    );
    assert!(spec.body.contains(&"c".repeat(40)));
    assert!(
        spec.body.contains("failed on a::b::c_test"),
        "{}",
        spec.body
    );
    let row = rig.store.need_branch(NEED)?.expect("row");
    assert_eq!(row.base.as_deref(), Some(MAIN_B));
    Ok(())
}

#[test]
fn no_repairer_role_is_an_error() -> R {
    let rig = rig(false, RECORD)?;
    main_tip(&rig.host, MAIN_A);
    let err = start(&rig, &step("fix1", "t1", StepState::Pending, None)).unwrap_err();
    assert!(err.to_string().contains("no repairer role"), "{err}");
    Ok(())
}

#[test]
fn a_record_without_a_target_is_an_error() -> R {
    let rig = rig(true, "kind: repair\nFAILED a::b\n")?;
    main_tip(&rig.host, MAIN_A);
    let err = start(&rig, &step("fix1", "t1", StepState::Pending, None)).unwrap_err();
    assert!(err.to_string().contains("target"), "{err}");
    assert!(rig.store.need_branch(NEED)?.is_none());
    Ok(())
}

#[test]
fn a_node_without_an_attempt_is_an_error() -> R {
    let rig = rig(true, RECORD)?;
    let err = start(&rig, &step("unrepaired", "t1", StepState::Pending, None)).unwrap_err();
    assert!(matches!(err, VivariumError::Parse(_)), "{err}");
    Ok(())
}
