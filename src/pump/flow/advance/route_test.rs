//! Tests for [`super::route`]: every matching edge is taken, successors are
//! sent and bound before the predecessor closes, joins wait, wrong words are
//! rejected.

use serde_json::json;
use tempfile::{TempDir, tempdir};

use super::{JoinInput, Routed, route};
use crate::error::VivariumError;
use crate::pump::config::PumpConfig;
use crate::pump::flow::def::{FlowDef, FlowKind, FlowNode, SENTINEL_MARKER, STEP_MARKER};
use crate::pump::flow::fake_mail::{Call, FakeMail};
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::mail::{MailReader, MailWriter, TaskStatus};
use crate::pump::flow::outcome::Outcome;
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::store::{DB_FILENAME, PumpStore};

type R = Result<(), VivariumError>;

const NEED: &str = "n1";

struct Rig {
    _dir: TempDir,
    ctx: Ctx,
    store: PumpStore,
    mail: FakeMail,
    def: FlowDef,
    sentinel: String,
}

fn rig() -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    store.insert_run(&RunRow {
        need: NEED.into(),
        run: 1,
        flow: "build".into(),
        flow_version: 1,
        kind: "build".into(),
        entry: "plan".into(),
        state: "live".into(),
    })?;
    let mut mail = FakeMail::new("pump");
    let sentinel = mail.send_step(
        "pump",
        "pump",
        "run",
        &format!("{SENTINEL_MARKER} {NEED} r1"),
        &[],
    )?;
    mail.bind(NEED, std::slice::from_ref(&sentinel))?;
    store.insert_step(
        NEED,
        &StepRow {
            run: 1,
            node: "sentinel".into(),
            task: sentinel.clone(),
            state: StepState::Running,
            verdict: None,
        },
    )?;
    Ok(Rig {
        _dir: dir,
        ctx,
        store,
        mail,
        def: load(FlowKind::Build)?,
        sentinel,
    })
}

impl Rig {
    /// An open step of `node` in the run, already bound.
    fn step(&mut self, node: &str) -> Result<Outcome, VivariumError> {
        let task = self.mail.send_step(
            "pump",
            "pump",
            node,
            &format!("{STEP_MARKER} {NEED} r1 {node}"),
            &[],
        )?;
        self.mail.bind(NEED, std::slice::from_ref(&task))?;
        self.store.insert_step(
            NEED,
            &StepRow {
                run: 1,
                node: node.into(),
                task: task.clone(),
                state: StepState::Running,
                verdict: None,
            },
        )?;
        Ok(outcome(node, &task, ""))
    }

    fn route(&mut self, out: &Outcome, decide: &str) -> Result<Routed, VivariumError> {
        let word = decide.to_string();
        let decide = move |_: &FlowNode, _: &[JoinInput]| Ok(word.clone());
        route(
            &self.ctx,
            &self.store,
            &mut self.mail,
            &self.def,
            out,
            &decide,
        )
    }
}

fn status(mail: &FakeMail, task: &str) -> Result<TaskStatus, VivariumError> {
    Ok(mail.task_state(task)?.status)
}

fn outcome(node: &str, task: &str, word: &str) -> Outcome {
    Outcome {
        need: NEED.into(),
        run: 1,
        node: node.into(),
        task: task.into(),
        word: word.into(),
        payload: json!({}),
    }
}

fn with_word(mut out: Outcome, word: &str) -> Outcome {
    out.word = word.into();
    out
}

fn nodes(routed: &Routed) -> Vec<&str> {
    routed.sent.iter().map(|s| s.node.as_str()).collect()
}

#[test]
fn one_word_fans_out_to_every_matching_edge() -> R {
    let mut rig = rig()?;
    let build = rig.step("build1")?;
    rig.mail.calls.clear();

    let routed = rig.route(&with_word(build.clone(), "built"), "")?;
    assert_eq!(nodes(&routed), vec!["audit1", "test1"]);
    assert!(routed.rejected.is_none());

    // Both successors wait on the predecessor and have step rows.
    for sent in &routed.sent {
        assert_eq!(
            rig.mail.tasks[&sent.task].depends_on,
            vec![build.task.clone()]
        );
        assert!(rig.mail.tasks[&sent.task].body.starts_with(STEP_MARKER));
        let row = rig.store.step_by_task(&sent.task)?.expect("step row");
        assert_eq!(
            (row.node, row.state),
            (sent.node.clone(), StepState::Pending)
        );
    }
    let row = rig
        .store
        .step_by_task(&build.task)?
        .expect("predecessor row");
    assert_eq!(row.state, StepState::Done);
    assert_eq!(row.verdict.as_deref(), Some("built"));
    Ok(())
}

#[test]
fn order_is_send_then_bind_then_close() -> R {
    let mut rig = rig()?;
    let build = rig.step("build1")?;
    rig.mail.calls.clear();

    let routed = rig.route(&with_word(build.clone(), "built"), "")?;
    let sends: Vec<usize> = rig
        .mail
        .calls
        .iter()
        .enumerate()
        .filter(|(_, c)| matches!(c, Call::SendStep { .. }))
        .map(|(i, _)| i)
        .collect();
    let bind = rig.mail.position(|c| matches!(c, Call::Bind { .. }));
    let close = rig
        .mail
        .position(|c| matches!(c, Call::CloseTask { handle, .. } if *handle == build.task));
    assert_eq!(sends.len(), 2);
    assert!(sends.iter().all(|s| Some(*s) < bind));
    assert!(bind < close);
    assert_eq!(rig.mail.calls.len(), 4, "two sends, one bind, one close");

    // One bind names the sentinel and both new tasks.
    let mut want = vec![rig.sentinel.clone()];
    want.extend(routed.sent.iter().map(|s| s.task.clone()));
    assert_eq!(
        rig.mail.calls[bind.expect("bind")],
        Call::Bind {
            need: NEED.into(),
            units: want
        }
    );
    assert!(matches!(
        &rig.mail.calls[close.expect("close")],
        Call::CloseTask { verdict, .. } if verdict == "built"
    ));
    Ok(())
}

#[test]
fn join_fires_only_after_both_inputs() -> R {
    let mut rig = rig()?;
    let audit = rig.step("audit1")?;
    let test = rig.step("test1")?;
    rig.mail.calls.clear();

    // The first input finds the join waiting; the decision is not asked.
    let waits = |_: &FlowNode, _: &[JoinInput]| Err(VivariumError::Other("asked too early".into()));
    let routed = route(
        &rig.ctx,
        &rig.store,
        &mut rig.mail,
        &rig.def,
        &with_word(audit.clone(), "clean_pass"),
        &waits,
    )?;
    assert_eq!(routed.waiting, vec!["j1"]);
    assert!(routed.sent.is_empty());
    assert_eq!(
        status(&rig.mail, &audit.task)?,
        TaskStatus::Done,
        "the finished input still closes"
    );

    // The second input fires it; the decision sees both inputs.
    let seen = std::cell::RefCell::new(Vec::new());
    let verified = |_: &FlowNode, inputs: &[JoinInput]| {
        *seen.borrow_mut() = inputs.to_vec();
        Ok("verified".to_string())
    };
    let routed = route(
        &rig.ctx,
        &rig.store,
        &mut rig.mail,
        &rig.def,
        &with_word(test.clone(), "pass"),
        &verified,
    )?;
    assert_eq!(nodes(&routed), vec!["merge"]);
    let mut got: Vec<(String, String)> = seen
        .borrow()
        .iter()
        .map(|i| (i.node.clone(), i.word.clone()))
        .collect();
    got.sort();
    assert_eq!(
        got,
        vec![
            ("audit1".to_string(), "clean_pass".to_string()),
            ("test1".to_string(), "pass".to_string())
        ]
    );
    Ok(())
}

#[test]
fn a_word_outside_the_set_is_rejected_and_changes_nothing() -> R {
    let mut rig = rig()?;
    let build = rig.step("build1")?;
    rig.mail.calls.clear();

    let routed = rig.route(&with_word(build.clone(), "merged"), "")?;
    assert!(routed.rejected.is_some());
    assert!(routed.sent.is_empty());
    assert!(rig.mail.calls.is_empty());
    assert_eq!(status(&rig.mail, &build.task)?, TaskStatus::Open);
    assert_eq!(
        rig.store.step_by_task(&build.task)?.expect("row").state,
        StepState::Running
    );
    Ok(())
}

#[test]
fn decisions_are_not_tasks_and_failed_routes_to_cto() -> R {
    let mut rig = rig()?;
    let plan = rig.step("plan")?;
    let routed = rig.route(&with_word(plan, "planned"), "")?;
    assert_eq!(nodes(&routed), vec!["admit1"], "pq is routing only");

    let slice = rig.step("slice")?;
    let routed = rig.route(&with_word(slice, "failed"), "")?;
    assert_eq!(nodes(&routed), vec!["cto"]);
    Ok(())
}

#[test]
fn raisecheck_is_decided_by_code_and_terminals_are_returned() -> R {
    let mut rig = rig()?;
    let judge = rig.step("judge1")?;
    let routed = rig.route(&with_word(judge, "residual"), "none")?;
    assert_eq!(nodes(&routed), vec!["merge"]);

    let plan = rig.step("plan")?;
    let routed = rig.route(&with_word(plan.clone(), "delivered"), "")?;
    assert!(routed.sent.is_empty());
    assert_eq!(routed.terminal.len(), 1);
    assert_eq!(routed.terminal[0].node, "done");
    assert_eq!(
        rig.store
            .step_by_task(&plan.task)?
            .expect("row")
            .verdict
            .as_deref(),
        Some("delivered"),
        "the predecessor closes even when the target is terminal"
    );
    Ok(())
}

#[test]
fn lock_becomes_an_open_task_and_restart_is_a_terminal() -> R {
    let mut rig = rig()?;
    let cto = rig.step("cto")?;
    let routed = rig.route(&with_word(cto, "lock"), "")?;
    assert_eq!(nodes(&routed), vec!["locked"]);
    let row = rig.store.step_by_task(&routed.sent[0].task)?.expect("row");
    assert_eq!(row.state, StepState::Waiting);

    let cto = rig.step("cto")?;
    let routed = rig.route(&with_word(cto, "slice"), "")?;
    assert!(routed.sent.is_empty());
    assert_eq!(routed.terminal[0].node, "r_slice");
    Ok(())
}
