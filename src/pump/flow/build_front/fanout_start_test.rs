//! Tests for the fanout start handler.

use std::fs;
use std::sync::Arc;

use tempfile::{TempDir, tempdir};

use super::FanoutStart;
use crate::error::VivariumError;
use crate::pump::config::PumpConfig;
use crate::pump::flow::def::{FlowDef, FlowKind};
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::flow::step_inputs::POOL_RESULT_MARKER;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::store::{DB_FILENAME, NeedBranchRow, PumpStore};

type R = Result<(), VivariumError>;

const NEED: &str = "n1";

struct Rig {
    dir: TempDir,
    ctx: Ctx,
    host: Arc<FakeHost>,
    store: PumpStore,
    mail: FakeMail,
    run: RunRow,
    def: FlowDef,
}

fn rig() -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let run = RunRow {
        need: NEED.into(),
        run: 1,
        flow: "build".into(),
        flow_version: 2,
        kind: "build".into(),
        entry: "plan".into(),
        state: "live".into(),
    };
    store.insert_run(&run)?;
    Ok(Rig {
        dir,
        ctx,
        host,
        store,
        mail: FakeMail::new("pump"),
        run,
        def: load(FlowKind::Build)?,
    })
}

fn row(node: &str, task: &str, state: StepState) -> StepRow {
    StepRow {
        run: 1,
        node: node.into(),
        task: task.into(),
        state,
        verdict: None,
    }
}

fn slice(id: &str, target: &str, scope: &str) -> String {
    format!(
        "[[slice]]\nid = \"{id}\"\ntitle = \"t\"\nbrief = 'b'\ntarget = \"{target}\"\n\
         write_scope = [{scope}]\ndeps = []\n"
    )
}

fn envelope(slices: &str) -> String {
    format!("+++\nanswer = \"sliced\"\n{slices}+++\nreport\n")
}

fn result(job: &str, message: &str) -> String {
    format!(
        "{POOL_RESULT_MARKER}{job}: finished\npool p  slot 1  model m\n\n--- receipt\n{message}"
    )
}

/// A done slicer step `slicer_node` with `replies` (oldest first) and a
/// running fanout step on `fanout_node`.
fn seat(rig: &mut Rig, slicer_node: &str, fanout_node: &str, replies: &[String]) -> R {
    let task = rig.mail.send_step("pump", "pump", "slicer", "body", &[])?;
    for (n, reply) in replies.iter().enumerate() {
        rig.mail.reply(&task, &result(&format!("j{n}"), reply))?;
    }
    rig.store
        .insert_step(NEED, &row(slicer_node, &task, StepState::Done))?;
    rig.store
        .insert_step(NEED, &row(fanout_node, "t-fan", StepState::Running))?;
    Ok(())
}

fn start(rig: &Rig, node: &str) -> Result<StartAction, VivariumError> {
    let cx = StartCx {
        ctx: &rig.ctx,
        store: &rig.store,
        mail: &rig.mail,
        run: &rig.run,
        def: &rig.def,
    };
    FanoutStart.start(&cx, &row(node, "t-fan", StepState::Running))
}

fn exceptions(rig: &Rig) -> String {
    fs::read_to_string(rig.dir.path().join("home").join("exceptions.txt")).unwrap_or_default()
}

fn failed() -> StartAction {
    StartAction::Decide {
        word: "failed".into(),
    }
}

#[test]
fn build1_fans_out_both_slices_and_records_the_open_fanout() -> R {
    let mut rig = rig()?;
    let both = format!(
        "{}{}",
        slice("S1", "default", "\"src/a.rs\""),
        slice("S2", "default", "\"src/b.rs\"")
    );
    seat(&mut rig, "slice", "build1", &[envelope(&both)])?;

    let action = start(&rig, "build1")?;

    let StartAction::Fanout {
        slicer_step,
        slices,
    } = action
    else {
        panic!("expected a fanout, got {action:?}");
    };
    assert_eq!(slicer_step.node, "slice");
    let ids: Vec<&str> = slices.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, vec!["S1", "S2"]);
    assert_eq!(rig.store.open_fanout(NEED, 1)?, Some("t-fan".into()));
    Ok(())
}

#[test]
fn build2_reads_the_rework_slicer() -> R {
    let mut rig = rig()?;
    let one = slice("R1", "default", "\"src/a.rs\"");
    seat(&mut rig, "rework1", "build2", &[envelope(&one)])?;

    let action = start(&rig, "build2")?;

    let StartAction::Fanout {
        slicer_step,
        slices,
    } = action
    else {
        panic!("expected a fanout, got {action:?}");
    };
    assert_eq!(slicer_step.node, "rework1");
    assert_eq!(slices.len(), 1);
    Ok(())
}

#[test]
fn an_unknown_target_answers_failed_and_raises_the_exception() -> R {
    let mut rig = rig()?;
    let bad = slice("S1", "nowhere", "\"src/a.rs\"");
    seat(&mut rig, "slice", "build1", &[envelope(&bad)])?;

    let action = start(&rig, "build1")?;

    assert_eq!(action, failed());
    let text = exceptions(&rig);
    assert!(text.contains("flow-slices-invalid"), "{text}");
    assert!(text.contains("nowhere"), "{text}");
    assert_eq!(rig.store.open_fanout(NEED, 1)?, None);
    Ok(())
}

#[test]
fn a_cyclic_reply_answers_failed_and_raises_the_exception() -> R {
    let mut rig = rig()?;
    let cyclic = format!(
        "{}{}",
        slice("A", "default", "\"src/a.rs\"").replace("deps = []", "deps = [\"B\"]"),
        slice("B", "default", "\"src/b.rs\"").replace("deps = []", "deps = [\"A\"]")
    );
    seat(&mut rig, "slice", "build1", &[envelope(&cyclic)])?;

    let action = start(&rig, "build1")?;

    assert_eq!(action, failed());
    let text = exceptions(&rig);
    assert!(text.contains("flow-slices-invalid"), "{text}");
    assert!(text.contains("cycle"), "{text}");
    assert_eq!(rig.store.open_fanout(NEED, 1)?, None);
    Ok(())
}

#[test]
fn an_empty_write_scope_answers_failed() -> R {
    let mut rig = rig()?;
    let bad = slice("S1", "default", "");
    seat(&mut rig, "slice", "build1", &[envelope(&bad)])?;

    let action = start(&rig, "build1")?;

    assert_eq!(action, failed());
    assert!(exceptions(&rig).contains("empty write_scope"));
    Ok(())
}

#[test]
fn a_slicer_with_no_reply_answers_failed() -> R {
    let mut rig = rig()?;
    seat(&mut rig, "slice", "build1", &[])?;

    let action = start(&rig, "build1")?;

    assert_eq!(action, failed());
    assert!(exceptions(&rig).contains("no pool result reply"));
    Ok(())
}

#[test]
fn a_correction_newer_than_a_broken_reply_is_used() -> R {
    let mut rig = rig()?;
    let good = slice("S1", "default", "\"src/a.rs\"");
    seat(
        &mut rig,
        "slice",
        "build1",
        &["not an envelope".to_string(), envelope(&good)],
    )?;

    let action = start(&rig, "build1")?;

    assert!(matches!(action, StartAction::Fanout { .. }), "{action:?}");
    Ok(())
}

#[test]
fn a_store_read_error_is_an_error_not_a_slices_invalid_failure() -> R {
    let mut rig = rig()?;
    let one = slice("S1", "default", "\"src/a.rs\"");
    seat(&mut rig, "slice", "build1", &[envelope(&one)])?;
    let conn = rusqlite::Connection::open(rig.dir.path().join(DB_FILENAME))
        .map_err(|e| VivariumError::Other(e.to_string()))?;
    conn.execute_batch("DROP TABLE flow_steps")
        .map_err(|e| VivariumError::Other(e.to_string()))?;

    let action = start(&rig, "build1");
    assert!(action.is_err(), "{action:?}");

    // The batch check itself keeps the read error: it is not a problem.
    let cx = StartCx {
        ctx: &rig.ctx,
        store: &rig.store,
        mail: &rig.mail,
        run: &rig.run,
        def: &rig.def,
    };
    let slices = vec![crate::pump::envelope::roles::Slice {
        id: "S1".into(),
        title: "t".into(),
        brief: "b".into(),
        target: "default".into(),
        write_scope: vec!["src/a.rs".into()],
        deps: Vec::new(),
    }];
    let found = super::problems(&cx, &slices);
    assert!(found.is_err(), "{found:?}");
    assert!(!exceptions(&rig).contains("flow-slices-invalid"));
    Ok(())
}

#[test]
fn a_missing_slicer_step_is_an_error_naming_the_node() -> R {
    let rig = rig()?;
    let err = start(&rig, "build1").unwrap_err();
    assert!(err.to_string().contains("build1"), "{err}");
    Ok(())
}

fn need_tip(rig: &Rig, sha: &str) {
    rig.host.reply(
        "ls-remote origin refs/heads/vivi/need/",
        0,
        &format!("{sha}\trefs/heads/vivi/need/{NEED}\n"),
    );
}

#[test]
fn the_need_tip_at_the_start_is_recorded_once() -> R {
    let mut rig = rig()?;
    let one = slice("S1", "default", "\"src/a.rs\"");
    seat(&mut rig, "slice", "build1", &[envelope(&one)])?;
    rig.store.upsert_need_branch(&NeedBranchRow {
        need: NEED.into(),
        branch: format!("vivi/need/{NEED}"),
        base: None,
        status: "live".into(),
        run: 1,
        target: "default".into(),
        priors: Vec::new(),
    })?;
    assert_eq!(rig.store.fanout_start("t-fan")?, None);

    let first = "1".repeat(40);
    need_tip(&rig, &first);
    assert!(matches!(start(&rig, "build1")?, StartAction::Fanout { .. }));
    assert_eq!(rig.store.fanout_start("t-fan")?, Some(Some(first.clone())));

    need_tip(&rig, &"2".repeat(40));
    assert!(matches!(start(&rig, "build1")?, StartAction::Fanout { .. }));
    assert_eq!(rig.store.fanout_start("t-fan")?, Some(Some(first)));
    Ok(())
}

#[test]
fn a_start_with_no_need_branch_records_none() -> R {
    let mut rig = rig()?;
    let one = slice("S1", "default", "\"src/a.rs\"");
    seat(&mut rig, "slice", "build1", &[envelope(&one)])?;
    start(&rig, "build1")?;
    assert_eq!(rig.store.fanout_start("t-fan")?, Some(None));
    Ok(())
}
