//! Routing tests of the shipped repair flow: attempts, unrepaired, green and a
//! restart at fix1, driven through `advance` over the fake mail port.

use serde_json::json;
use tempfile::{TempDir, tempdir};

use crate::error::VivariumError;
use crate::pump::config::PumpConfig;
use crate::pump::flow::advance::restart::restart;
use crate::pump::flow::advance::route::JoinInput;
use crate::pump::flow::advance::start::{SENTINEL_NODE, start};
use crate::pump::flow::advance::terminal::Finished;
use crate::pump::flow::advance::{Advanced, advance};
use crate::pump::flow::def::{FlowDef, FlowKind, FlowNode, NodeClass};
use crate::pump::flow::fake_mail::{Call, FakeMail};
use crate::pump::flow::index::{RunRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::mail::{MailReader, MailWriter, TaskStatus};
use crate::pump::flow::outcome::Outcome;
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::store::{DB_FILENAME, PumpStore};

type R = Result<(), VivariumError>;

const NEED: &str = "n1";
const TESTED: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

struct Rig {
    _dir: TempDir,
    ctx: Ctx,
    store: PumpStore,
    mail: FakeMail,
    def: FlowDef,
}

/// A repair need with a live run 1 at fix1.
fn rig() -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let mut mail = FakeMail::new("pump");
    let body = format!("kind: repair\ntarget: vivi\ntested: {TESTED}\nmain is red");
    MailWriter::open_record(&mut mail, NEED, &body)?;
    start(&ctx, &store, &mut mail, NEED, Some("fix1"))?;
    Ok(Rig {
        _dir: dir,
        ctx,
        store,
        mail,
        def: load(FlowKind::Repair)?,
    })
}

impl Rig {
    fn run(&self, run: u32) -> Result<RunRow, VivariumError> {
        self.store
            .runs_of(NEED)?
            .into_iter()
            .find(|row| row.run == run)
            .ok_or_else(|| VivariumError::Other(format!("no run {run}")))
    }

    /// The newest open row of `node` in run 1, as a finished step with `word`.
    fn outcome(&self, node: &str, word: &str) -> Result<Outcome, VivariumError> {
        let rows = self.store.steps_of(NEED, 1)?;
        let row = rows
            .iter()
            .rev()
            .find(|row| row.node == node && row.state != StepState::Done)
            .ok_or_else(|| VivariumError::Other(format!("no open step {node}")))?;
        Ok(Outcome {
            need: NEED.into(),
            run: 1,
            node: node.into(),
            task: row.task.clone(),
            word: word.into(),
            payload: json!({}),
        })
    }

    fn feed(&mut self, node: &str, word: &str) -> Result<Advanced, VivariumError> {
        let outcome = self.outcome(node, word)?;
        let decide = |_: &FlowNode, _: &[JoinInput]| {
            Err(VivariumError::Other("the repair flow has no join".into()))
        };
        advance(
            &self.ctx,
            &self.store,
            &mut self.mail,
            &self.def,
            &outcome,
            &decide,
        )
    }

    /// Feed a path of (node, word) pairs, each from the row the last one sent.
    fn walk(&mut self, path: &[(&str, &str)]) -> R {
        for (node, word) in path {
            self.feed(node, word)?;
        }
        Ok(())
    }

    fn sent(advanced: &Advanced) -> Vec<&str> {
        advanced
            .routed
            .sent
            .iter()
            .map(|s| s.node.as_str())
            .collect()
    }

    fn need_closed(&self) -> bool {
        self.mail
            .position(|c| matches!(c, Call::CloseNeed { .. }))
            .is_some()
    }

    fn record_landed(&self) -> bool {
        self.mail
            .position(|c| matches!(c, Call::CloseRecord { outcome, .. } if outcome == "landed"))
            .is_some()
    }

    fn sentinel_open(&self) -> Result<bool, VivariumError> {
        let rows = self.store.steps_of(NEED, 1)?;
        let row = rows
            .iter()
            .find(|row| row.node == SENTINEL_NODE)
            .ok_or_else(|| VivariumError::Other("no sentinel".into()))?;
        Ok(self.mail.task_state(&row.task)?.status == TaskStatus::Open)
    }

    fn state_of(&self, node: &str) -> Result<Option<StepState>, VivariumError> {
        let rows = self.store.steps_of(NEED, 1)?;
        Ok(rows.iter().rev().find(|r| r.node == node).map(|r| r.state))
    }
}

/// The path of words that leaves `node` open on attempt 1.
fn to_attempt1(node: &str) -> Vec<(&'static str, &'static str)> {
    match node {
        "fix1" => vec![],
        "rung1" => vec![("fix1", "built")],
        _ => vec![("fix1", "built"), ("rung1", "pass")],
    }
}

/// The path of words that leaves `node` open on attempt 2, after a failed attempt 1.
fn to_attempt2(node: &str) -> Vec<(&'static str, &'static str)> {
    let mut path = vec![("fix1", "failed")];
    match node {
        "fix2" => {}
        "rung2" => path.push(("fix2", "built")),
        _ => path.extend([("fix2", "built"), ("rung2", "pass")]),
    }
    path
}

const FAILURES: [(&str, &str); 6] = [
    ("fix1", "failed"),
    ("fix1", "overtime"),
    ("rung1", "fail"),
    ("rung1", "failed"),
    ("merge1", "conflict"),
    ("merge1", "failed"),
];

#[test]
fn the_happy_path_builds_runs_the_rung_merges_and_lands() -> R {
    let mut rig = rig()?;
    assert_eq!(Rig::sent(&rig.feed("fix1", "built")?), vec!["rung1"]);
    assert_eq!(Rig::sent(&rig.feed("rung1", "pass")?), vec!["merge1"]);
    let landed = rig.feed("merge1", "merged")?;
    assert_eq!(landed.finished, vec![Finished::Landed]);
    assert!(rig.need_closed());
    assert!(rig.record_landed());
    Ok(())
}

#[test]
fn every_failure_on_attempt_one_sends_fix2() -> R {
    for (node, word) in FAILURES {
        let mut rig = rig()?;
        rig.walk(&to_attempt1(node))?;
        let advanced = rig.feed(node, word)?;
        assert_eq!(Rig::sent(&advanced), vec!["fix2"], "{node} {word}");
        assert!(advanced.finished.is_empty(), "{node} {word}");
        assert!(!rig.need_closed(), "{node} {word}");
    }
    Ok(())
}

#[test]
fn every_failure_on_attempt_two_reaches_unrepaired_and_holds() -> R {
    for (node, word) in FAILURES {
        let node = node.replace('1', "2");
        let mut rig = rig()?;
        rig.walk(&to_attempt2(&node))?;
        let advanced = rig.feed(&node, word)?;
        assert_eq!(Rig::sent(&advanced), vec!["unrepaired"], "{node} {word}");
        assert!(advanced.finished.is_empty(), "{node} {word}");
        assert_eq!(rig.state_of("unrepaired")?, Some(StepState::Waiting));
        assert!(rig.sentinel_open()?, "{node} {word}");
        assert!(!rig.need_closed(), "{node} {word}");
    }
    Ok(())
}

#[test]
fn green_on_the_unrepaired_step_lands_the_need() -> R {
    let mut rig = rig()?;
    rig.walk(&to_attempt2("fix2"))?;
    rig.feed("fix2", "failed")?;
    let landed = rig.feed("unrepaired", "green")?;
    assert_eq!(landed.finished, vec![Finished::Landed]);
    assert!(rig.need_closed());
    assert!(rig.record_landed());
    Ok(())
}

#[test]
fn a_restart_at_fix1_from_unrepaired_supersedes_the_old_run() -> R {
    let mut rig = rig()?;
    rig.walk(&to_attempt2("fix2"))?;
    rig.feed("fix2", "overtime")?;
    let old = rig.run(1)?;

    let next = restart(&rig.ctx, &rig.store, &mut rig.mail, &old, "fix1")?;

    assert_eq!(
        (next.run, next.entry.as_str(), next.kind.as_str()),
        (2, "fix1", "repair")
    );
    assert_eq!(rig.run(1)?.state, "superseded");
    assert_eq!(rig.run(2)?.state, "live");
    assert_eq!(rig.state_of("unrepaired")?, Some(StepState::Done));
    let rows = rig.store.steps_of(NEED, 2)?;
    assert!(rows.iter().any(|row| row.node == "fix1"));
    assert!(!rig.need_closed());
    Ok(())
}

#[test]
fn the_repair_flow_has_no_escalation_and_failures_lead_only_to_fix2_or_unrepaired() -> R {
    let def = load(FlowKind::Repair)?;
    assert!(
        def.nodes
            .iter()
            .all(|n| !matches!(n.class, NodeClass::Cto | NodeClass::Restart(_)))
    );
    let failing = ["failed", "fail", "overtime", "conflict"];
    for edge in def
        .edges
        .iter()
        .filter(|e| e.words.iter().any(|w| failing.contains(&w.as_str())))
    {
        assert!(
            ["fix2", "unrepaired"].contains(&edge.to.as_str()),
            "{} -> {}",
            edge.from,
            edge.to
        );
    }
    let mut done_from: Vec<&str> = def
        .edges
        .iter()
        .filter(|e| e.to == "done")
        .map(|e| e.from.as_str())
        .collect();
    done_from.sort_unstable();
    assert_eq!(done_from, vec!["m1", "m2", "unrepaired"]);
    Ok(())
}
