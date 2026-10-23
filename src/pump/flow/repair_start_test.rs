//! Repairer start: a job from the current main tip, the need_branches row,
//! the prior-attempt brief.

use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use super::RepairerStart;
use crate::error::VivariumError;
use crate::mailspace::marked_line;
use crate::pump::config::PumpConfig;
use crate::pump::flow::advance::restart::restart;
use crate::pump::flow::advance::start::start as start_run;
use crate::pump::flow::def::FlowKind;
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::flow::repair::{self, AttemptFacts};
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::store::step_payload::StepPayloadRow;
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
        flow_version: 2,
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
    start_in(rig, &run(), step)
}

fn start_in(rig: &Rig, run: &RunRow, step: &StepRow) -> Result<StartAction, VivariumError> {
    let def = load(FlowKind::Repair)?;
    let cx = StartCx {
        ctx: &rig.ctx,
        store: &rig.store,
        mail: &rig.mail,
        run,
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
    assert_eq!((row.need.as_str(), row.run), (NEED, 1));
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

#[test]
fn a_restarted_repair_run_publishes_to_fresh_refs() -> R {
    let mut rig = rig(true, RECORD)?;
    let old = start_run(&rig.ctx, &rig.store, &mut rig.mail, NEED, Some("fix1"))?;
    let rows = rig.store.steps_of(NEED, old.run)?;
    let fix1 = rows
        .iter()
        .find(|row| row.node == "fix1")
        .expect("fix1 row")
        .clone();
    let fix2 = rig.mail.send_step("mind", "mind", "fix2", "b", &[])?;
    rig.store.insert_step(
        NEED,
        &StepRow {
            run: old.run,
            node: "fix2".into(),
            task: fix2.clone(),
            state: StepState::Running,
            verdict: None,
        },
    )?;
    let mut old_refs = Vec::new();
    for (attempt, task) in [(1, &fix1.task), (2, &fix2)] {
        let branch = repair::branch(NEED, old.run, attempt);
        let facts = AttemptFacts {
            branch: branch.clone(),
            commit: "c".repeat(40),
            base: MAIN_A.into(),
        };
        rig.mail.reply(
            task,
            &marked_line(repair::ATTEMPT_MARKER, &repair::facts_text(&facts)),
        )?;
        old_refs.push(branch);
    }
    main_tip(&rig.host, MAIN_A);
    let first = job(start_in(&rig, &old, &fix1)?);
    assert_eq!(first.branch.as_deref(), Some(old_refs[0].as_str()));
    assert_eq!(rig.store.need_branch(NEED)?.expect("row").run, 1);

    let next = restart(&rig.ctx, &rig.store, &mut rig.mail, &old, "fix1")?;

    assert_eq!(next.run, 2);
    let new_fix1 = rig
        .store
        .steps_of(NEED, next.run)?
        .into_iter()
        .find(|row| row.node == "fix1")
        .expect("new fix1 row");
    main_tip(&rig.host, MAIN_B);
    let spec = job(start_in(&rig, &next, &new_fix1)?);
    let want = format!("vivi/repair/{NEED}-r2-a1");
    assert_eq!(spec.branch.as_deref(), Some(want.as_str()));
    assert_eq!(
        spec.publish_ref.as_deref(),
        Some(format!("refs/heads/{want}").as_str())
    );
    assert!(old_refs.iter().all(|old| *old != want));
    assert!(!spec.body.contains("Prior attempt"));
    let row = rig.store.need_branch(NEED)?.expect("row");
    assert_eq!(row.run, 2);
    assert_eq!(row.base.as_deref(), Some(MAIN_B));
    Ok(())
}

#[test]
fn attempt_two_brief_says_why_attempt_one_changed_nothing() -> R {
    let mut rig = rig(true, RECORD)?;
    let fix = rig.mail.send_step("mind", "mind", "fix1", "b", &[])?;
    rig.store.insert_step(
        NEED,
        &step("fix1", &fix, StepState::Done, Some("unchanged")),
    )?;
    rig.store.record_step_payload(&StepPayloadRow {
        need: NEED.into(),
        run: 1,
        node: "fix1".into(),
        class: "repairer".into(),
        task: fix.clone(),
        payload: serde_json::json!({
            "job": "j1",
            "reason": "expected a commit on X from base B; actual: no change",
        }),
    })?;
    main_tip(&rig.host, MAIN_B);
    let spec = job(start(&rig, &step("fix2", "t2", StepState::Pending, None))?);
    assert!(spec.body.contains("verdict unchanged"), "{}", spec.body);
    assert!(
        spec.body
            .contains("expected a commit on X from base B; actual: no change"),
        "{}",
        spec.body
    );
    assert!(spec.body.contains("(job j1)"), "{}", spec.body);
    assert!(
        spec.body.contains("Attempt 1 published nothing."),
        "{}",
        spec.body
    );
    Ok(())
}
