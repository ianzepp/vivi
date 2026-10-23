//! A green main rung answers the unrepaired step of the matching repair run.

use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use super::{UNREPAIRED_NODE, answer_green};
use crate::error::VivariumError;
use crate::letter::fixture::{sentinel_body, step_body};
use crate::pump::config::PumpConfig;
use crate::pump::flow::def::{FlowDef, FlowEdge, FlowKind, FlowNode, NodeClass};
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::mail::{MailReader, MailWriter, NeedStatus};
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::store::{DB_FILENAME, PumpStore};

type R = Result<(), VivariumError>;

const TIP: &str = "0123456789abcdef0123456789abcdef01234567";
const TESTED: &str = "3333333333333333333333333333333333333333";

struct Rig {
    _dir: TempDir,
    ctx: Ctx,
    host: Arc<FakeHost>,
    store: PumpStore,
    mail: FakeMail,
    def: FlowDef,
}

fn node(id: &str, class: NodeClass) -> FlowNode {
    FlowNode {
        id: id.into(),
        class,
        label: id.into(),
    }
}

fn repair_def() -> FlowDef {
    FlowDef {
        kind: FlowKind::Repair,
        nodes: vec![
            node(UNREPAIRED_NODE, NodeClass::Lock),
            node("done", NodeClass::Done),
        ],
        edges: vec![FlowEdge {
            from: UNREPAIRED_NODE.into(),
            to: "done".into(),
            words: vec![String::from("green")],
        }],
        entries: vec![UNREPAIRED_NODE.into()],
    }
}

fn rig() -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    Ok(Rig {
        _dir: dir,
        ctx,
        host,
        store,
        mail: FakeMail::new("pump"),
        def: repair_def(),
    })
}

impl Rig {
    /// A live run of `kind` on `need` with an unrepaired step Waiting and an
    /// open record naming `target`. Returns the unrepaired step task.
    fn run(&mut self, need: &str, kind: &str, target: &str) -> Result<String, VivariumError> {
        self.run_at(need, kind, target, UNREPAIRED_NODE, None)
    }

    /// Like `run`, with the step at `node` and a `tested:` line when given.
    fn run_at(
        &mut self,
        need: &str,
        kind: &str,
        target: &str,
        node: &str,
        tested: Option<&str>,
    ) -> Result<String, VivariumError> {
        self.store.insert_run(&RunRow {
            need: need.into(),
            run: 1,
            flow: kind.into(),
            flow_version: 2,
            kind: kind.into(),
            entry: node.into(),
            state: "live".into(),
        })?;
        let sentinel =
            self.mail
                .send_step("pump", "pump", "run", &sentinel_body(need, 1, ""), &[])?;
        self.mail.bind(need, std::slice::from_ref(&sentinel))?;
        self.store.insert_step(
            need,
            &StepRow {
                run: 1,
                node: "sentinel".into(),
                task: sentinel,
                state: StepState::Running,
                verdict: None,
            },
        )?;
        let task = self
            .mail
            .send_step("pump", "pump", node, &step_body(need, 1, node, ""), &[])?;
        self.mail.bind(need, std::slice::from_ref(&task))?;
        self.store.insert_step(
            need,
            &StepRow {
                run: 1,
                node: node.into(),
                task: task.clone(),
                state: StepState::Waiting,
                verdict: None,
            },
        )?;
        let tested = tested.map_or(String::new(), |sha| format!("\ntested: {sha}"));
        MailWriter::open_record(
            &mut self.mail,
            need,
            &format!("kind: {kind}\ntarget: {target}{tested}"),
        )?;
        Ok(task)
    }

    fn green(&mut self, target: &str) -> Result<Vec<String>, VivariumError> {
        answer_green(
            &self.ctx,
            &self.store,
            &mut self.mail,
            &self.def,
            target,
            TIP,
        )
    }
}

#[test]
fn green_answers_the_unrepaired_step_and_lands_the_need() -> R {
    let mut rig = rig()?;
    let task = rig.run("n1", "repair", "default")?;

    assert_eq!(rig.green("default")?, vec![task]);

    assert_eq!(rig.mail.need_state("n1")?, NeedStatus::Done);
    assert_eq!(rig.mail.record_outcome("n1"), Some("landed"));
    assert!(rig.store.live_runs()?.is_empty());
    Ok(())
}

#[test]
fn another_target_is_untouched() -> R {
    let mut rig = rig()?;
    let task = rig.run("n1", "repair", "other")?;

    assert!(rig.green("default")?.is_empty());

    assert_ne!(rig.mail.need_state("n1")?, NeedStatus::Done);
    assert_eq!(rig.mail.record_outcome("n1"), None);
    assert_eq!(rig.store.live_runs()?.len(), 1);
    assert!(
        rig.store
            .steps_of("n1", 1)?
            .iter()
            .any(|s| s.task == task && s.state == StepState::Waiting)
    );
    Ok(())
}

#[test]
fn a_build_run_is_untouched() -> R {
    let mut rig = rig()?;
    rig.run("n1", "build", "default")?;

    assert!(rig.green("default")?.is_empty());

    assert_ne!(rig.mail.need_state("n1")?, NeedStatus::Done);
    assert_eq!(rig.store.live_runs()?.len(), 1);
    Ok(())
}

/// Git replies: both commits are local and `merge-base --is-ancestor` exits
/// with `code` (0 yes, 1 no).
fn ancestry(rig: &Rig, code: i32) {
    rig.host.reply("cat-file -e", 0, "");
    rig.host.reply("merge-base --is-ancestor", code, "");
}

fn exceptions(rig: &Rig) -> String {
    std::fs::read_to_string(rig.ctx.home.join("exceptions.txt")).unwrap_or_default()
}

#[test]
fn a_run_at_fix1_whose_green_tip_holds_the_tested_tip_is_landed_outside_the_flow() -> R {
    let mut rig = rig()?;
    let task = rig.run_at("n1", "repair", "default", "fix1", Some(TESTED))?;
    ancestry(&rig, 0);

    assert_eq!(rig.green("default")?, vec![String::from("n1")]);

    assert_eq!(rig.mail.need_state("n1")?, NeedStatus::Done);
    let note = rig.mail.need_close_note("n1").unwrap_or_default();
    assert!(note.contains("main green at"), "{note}");
    assert!(note.contains(TIP), "{note}");
    assert!(rig.store.live_runs()?.is_empty());
    assert!(
        rig.store
            .steps_of("n1", 1)?
            .iter()
            .any(|s| s.task == task && s.state == StepState::Done)
    );
    assert!(
        rig.store
            .steps_of("n1", 1)?
            .iter()
            .all(|s| s.verdict.as_deref() != Some("green"))
    );
    Ok(())
}

#[test]
fn a_green_tip_that_is_an_ancestor_of_the_tested_tip_does_not_close_the_run() -> R {
    let mut rig = rig()?;
    rig.run_at("n1", "repair", "default", "fix1", Some(TESTED))?;
    // An older green applied late: the tested tip is not behind the green tip.
    ancestry(&rig, 1);

    assert!(rig.green("default")?.is_empty());

    assert_ne!(rig.mail.need_state("n1")?, NeedStatus::Done);
    assert_eq!(rig.store.live_runs()?.len(), 1);
    let asked = format!("merge-base --is-ancestor {TESTED} {TIP}");
    assert!(
        rig.host.commands().iter().any(|c| c.contains(&asked)),
        "{:?}",
        rig.host.commands()
    );
    assert_eq!(exceptions(&rig), "");
    Ok(())
}

#[test]
fn an_equal_green_tip_closes_the_run() -> R {
    let mut rig = rig()?;
    rig.run_at("n1", "repair", "default", "fix1", Some(TIP))?;

    assert_eq!(rig.green("default")?, vec![String::from("n1")]);

    assert_eq!(rig.mail.need_state("n1")?, NeedStatus::Done);
    assert!(rig.store.live_runs()?.is_empty());
    Ok(())
}

#[test]
fn a_git_error_raises_one_green_check_failed_and_leaves_the_run() -> R {
    let mut rig = rig()?;
    rig.run_at("n1", "repair", "default", "fix1", Some(TESTED))?;
    rig.host.reply("cat-file -e", 0, "");
    rig.host
        .reply("merge-base --is-ancestor", 128, "fatal: bad object");

    assert!(rig.green("default")?.is_empty());
    assert!(rig.green("default")?.is_empty());

    assert_ne!(rig.mail.need_state("n1")?, NeedStatus::Done);
    assert_eq!(rig.store.live_runs()?.len(), 1);
    let log = exceptions(&rig);
    assert_eq!(log.matches("flow-green-check-failed").count(), 1, "{log}");
    assert!(
        log.contains("repair need n1: expected git to tell if green tip"),
        "{log}"
    );
    assert!(log.contains("actual: merge-base exit 128"), "{log}");
    Ok(())
}

#[test]
fn a_record_without_a_tested_line_changes_nothing() -> R {
    let mut rig = rig()?;
    rig.run_at("n1", "repair", "default", "fix1", None)?;

    assert!(rig.green("default")?.is_empty());

    assert_ne!(rig.mail.need_state("n1")?, NeedStatus::Done);
    assert_eq!(rig.store.live_runs()?.len(), 1);
    assert!(rig.host.commands().is_empty());
    Ok(())
}
