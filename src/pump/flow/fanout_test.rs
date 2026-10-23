//! Tests for fanout expansion and settle.

use tempfile::{TempDir, tempdir};

use super::{
    BUILT, expand, is_slice_row, mark_exhausted, parse_reply, settle, slice_node, target_of,
};
use crate::error::VivariumError;
use crate::pump::config::PumpConfig;
use crate::pump::envelope::EnvelopeError;
use crate::pump::envelope::roles::{Slice, SlicerAnswer, SlicerMode};
use crate::pump::flow::def::{SENTINEL_MARKER, STEP_MARKER};
use crate::pump::flow::fake_mail::{Call, FakeMail};
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::scope::ScopeState;
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::store::{DB_FILENAME, PumpStore};

type R = Result<(), VivariumError>;

const NEED: &str = "n1";

struct Rig {
    _dir: TempDir,
    ctx: Ctx,
    store: PumpStore,
    mail: FakeMail,
    run: RunRow,
    sentinel: String,
    slicer: StepRow,
}

fn rig() -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let run = RunRow {
        need: NEED.into(),
        run: 1,
        flow: "build".into(),
        flow_version: 1,
        kind: "build".into(),
        entry: "plan".into(),
        state: "live".into(),
    };
    store.insert_run(&run)?;
    let mut mail = FakeMail::new("pump");
    let sentinel = mail.send_step(
        "pump",
        "pump",
        "run",
        &format!("{SENTINEL_MARKER} {NEED} r1"),
        &[],
    )?;
    let slicer_task = mail.send_step(
        "pump",
        "pump",
        "slice",
        &format!("{STEP_MARKER} {NEED} r1 slice"),
        &[],
    )?;
    mail.bind(NEED, &[sentinel.clone(), slicer_task.clone()])?;
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
    let slicer = StepRow {
        run: 1,
        node: "slice".into(),
        task: slicer_task,
        state: StepState::Done,
        verdict: Some("sliced".into()),
    };
    store.insert_step(NEED, &slicer)?;
    Ok(Rig {
        _dir: dir,
        ctx,
        store,
        mail,
        run,
        sentinel,
        slicer,
    })
}

fn slice(id: &str, deps: &[&str]) -> Slice {
    Slice {
        id: id.into(),
        title: format!("title {id}"),
        brief: format!("brief {id}"),
        target: "vivi".into(),
        write_scope: vec![format!("src/{id}.rs")],
        deps: deps.iter().map(|d| (*d).to_string()).collect(),
    }
}

impl Rig {
    fn expand(&mut self, slices: &[Slice]) -> Result<Vec<String>, VivariumError> {
        expand(
            &self.ctx,
            &self.store,
            &mut self.mail,
            &self.run,
            &self.slicer,
            slices,
        )
    }

    fn task_of(&self, id: &str) -> Result<String, VivariumError> {
        let node = slice_node(id);
        self.store
            .steps_of(NEED, 1)?
            .into_iter()
            .find(|row| row.node == node)
            .map(|row| row.task)
            .ok_or_else(|| VivariumError::Message(format!("no slice row {id}")))
    }

    fn settle(&self) -> Result<Option<&'static str>, VivariumError> {
        settle(&self.store, &self.mail, &self.run)
    }

    fn close(&mut self, id: &str, verdict: &str) -> R {
        let task = self.task_of(id)?;
        self.mail.close_task(&task, verdict, None, None)
    }
}

fn two_slice_reply() -> &'static str {
    r#"+++
answer = "sliced"

[[slice]]
id = "A"
title = "First"
brief = 'Do the first thing.'
target = "vivi"
write_scope = ["src/a.rs"]
deps = []

[[slice]]
id = "B"
title = "Second"
brief = 'Do the second thing.'
target = "vivi"
write_scope = ["src/b.rs"]
deps = ["A"]
+++
Notes.
"#
}

#[test]
fn slicer_envelope_reads_two_slice_tables() {
    let reply = parse_reply(two_slice_reply()).expect("parse");
    assert_eq!(reply.answer, SlicerAnswer::Sliced);
    assert_eq!(reply.mode, SlicerMode::Slice);
    assert_eq!(reply.slice.len(), 2);
    assert_eq!(reply.slice[1].id, "B");
    assert_eq!(reply.slice[1].deps, vec!["A".to_string()]);
    assert_eq!(reply.slice[0].write_scope, vec!["src/a.rs".to_string()]);
}

#[test]
fn slicer_envelope_refuses_a_proof_key() {
    let text = two_slice_reply().replace("deps = []", "deps = []\nproof = 'cargo test'");
    assert!(matches!(
        parse_reply(&text),
        Err(EnvelopeError::Toml { .. })
    ));
}

#[test]
fn slicer_envelope_refuses_sliced_without_a_slice() {
    let text = "+++\nanswer = \"sliced\"\n+++\nnothing\n";
    assert!(matches!(parse_reply(text), Err(EnvelopeError::Invalid(_))));
}

#[test]
fn slicer_envelope_escalate_takes_no_slices() {
    let ok = "+++\nanswer = \"escalate\"\nreason = 'too big'\n+++\n";
    let reply = parse_reply(ok).expect("parse");
    assert_eq!(reply.answer, SlicerAnswer::Escalate);
    assert!(reply.slice.is_empty());
    let with_slice = format!(
        "+++\nanswer = \"escalate\"\nreason = 'x'\n\n[[slice]]\nid = \"A\"\ntitle = \"t\"\n\
         brief = 'b'\ntarget = \"vivi\"\n+++\n"
    );
    assert!(matches!(
        parse_reply(&with_slice),
        Err(EnvelopeError::Invalid(_))
    ));
}

#[test]
fn slicer_envelope_accepts_split_ids() {
    let text = "+++\nanswer = \"sliced\"\nmode = \"split\"\n\n\
        [[slice]]\nid = \"S3.1\"\ntitle = \"a\"\nbrief = 'b'\ntarget = \"vivi\"\n\n\
        [[slice]]\nid = \"S3.2\"\ntitle = \"a\"\nbrief = 'b'\ntarget = \"vivi\"\n+++\n";
    let reply = parse_reply(text).expect("parse");
    assert_eq!(reply.mode, SlicerMode::Split);
    assert_eq!(reply.slice[0].id, "S3.1");
}

#[test]
fn expand_sends_in_topological_order_with_the_json_edges() -> R {
    let mut r = rig()?;
    // The slicer lists C first; C needs A and B, B needs A.
    let tasks = r.expand(&[slice("C", &["A", "B"]), slice("B", &["A"]), slice("A", &[])])?;
    assert_eq!(tasks.len(), 3);
    let (a, b, c) = (r.task_of("A")?, r.task_of("B")?, r.task_of("C")?);
    assert_eq!(tasks, vec![a.clone(), b.clone(), c.clone()]);
    let slicer = r.slicer.task.clone();
    assert_eq!(r.mail.tasks[&a].depends_on, vec![slicer.clone()]);
    assert_eq!(r.mail.tasks[&b].depends_on, vec![slicer.clone(), a.clone()]);
    assert_eq!(r.mail.tasks[&c].depends_on, vec![slicer, a, b]);
    Ok(())
}

#[test]
fn expand_binds_with_the_sentinel_and_indexes_rows_and_scope() -> R {
    let mut r = rig()?;
    r.expand(&[slice("A", &[]), slice("B", &["A"])])?;
    let (a, b) = (r.task_of("A")?, r.task_of("B")?);
    let bound = r.mail.calls.iter().rev().find_map(|call| match call {
        Call::Bind { units, .. } => Some(units.clone()),
        _ => None,
    });
    assert_eq!(bound, Some(vec![r.sentinel.clone(), a.clone(), b.clone()]));
    let rows = r.store.steps_of(NEED, 1)?;
    assert_eq!(rows[0].node, "sentinel");
    let a_row = rows.iter().find(|row| row.task == a).expect("row a");
    assert_eq!(a_row.state, StepState::Pending);
    assert!(is_slice_row(a_row));
    assert_eq!(r.store.slice_scope(&a)?, Some(vec!["src/A.rs".to_string()]));
    assert_eq!(r.store.slice_scope(&b)?, Some(vec!["src/B.rs".to_string()]));
    let body = &r.mail.tasks[&a].body;
    assert!(body.starts_with(&format!("{STEP_MARKER} {NEED} r1 slice:A")));
    assert_eq!(target_of(body), Some("vivi"));
    assert!(body.contains("brief A"));
    Ok(())
}

#[test]
fn expand_refuses_a_cycle_and_sends_nothing() -> R {
    let mut r = rig()?;
    let before = r.mail.calls.len();
    let got = r.expand(&[slice("A", &["B"]), slice("B", &["A"])]);
    assert!(matches!(got, Err(VivariumError::Parse(_))));
    assert_eq!(r.mail.calls.len(), before);
    assert_eq!(r.store.steps_of(NEED, 1)?.len(), 2);
    Ok(())
}

#[test]
fn expand_refuses_unknown_and_duplicate_ids() -> R {
    let mut r = rig()?;
    assert!(r.expand(&[slice("A", &["Z"])]).is_err());
    assert!(r.expand(&[slice("A", &[]), slice("A", &[])]).is_err());
    assert!(r.expand(&[slice("A", &["A"])]).is_err());
    assert_eq!(r.store.steps_of(NEED, 1)?.len(), 2);
    Ok(())
}

#[test]
fn a_later_fanout_depends_on_slices_the_run_already_sent() -> R {
    let mut r = rig()?;
    r.expand(&[slice("A", &[])])?;
    r.expand(&[slice("R1", &["A"])])?;
    let (a, r1) = (r.task_of("A")?, r.task_of("R1")?);
    assert!(r.mail.tasks[&r1].depends_on.contains(&a));
    Ok(())
}

#[test]
fn settle_is_none_without_slices_or_while_one_is_open() -> R {
    let mut r = rig()?;
    assert_eq!(r.settle()?, None);
    r.expand(&[slice("A", &[]), slice("B", &[])])?;
    assert_eq!(r.settle()?, None);
    r.close("A", "merged")?;
    assert_eq!(r.settle()?, None);
    Ok(())
}

#[test]
fn settle_built_when_every_slice_is_merged_or_no_defect() -> R {
    let mut r = rig()?;
    r.expand(&[slice("A", &[]), slice("B", &[])])?;
    r.close("A", "merged")?;
    r.close("B", "no-defect")?;
    assert_eq!(r.settle()?, Some(BUILT));
    Ok(())
}

#[test]
fn settle_is_not_built_for_another_verdict() -> R {
    let mut r = rig()?;
    r.expand(&[slice("A", &[])])?;
    r.close("A", "done")?;
    assert_eq!(r.settle()?, None);
    Ok(())
}

#[test]
fn settle_failed_when_a_slice_is_exhausted() -> R {
    let mut r = rig()?;
    r.expand(&[slice("A", &[]), slice("B", &[])])?;
    r.close("A", "merged")?;
    mark_exhausted(&r.store, &r.task_of("B")?)?;
    assert_eq!(r.settle()?, Some("failed"));
    Ok(())
}

#[test]
fn a_scope_wait_slice_keeps_the_fanout_open() -> R {
    let mut r = rig()?;
    r.expand(&[slice("A", &[]), slice("B", &[])])?;
    r.close("A", "merged")?;
    let b = r.task_of("B")?;
    r.store.set_scope_state(&b, ScopeState::Wait)?;
    assert_eq!(r.settle()?, None);
    r.store.set_scope_state(&b, ScopeState::Repair)?;
    assert_eq!(r.settle()?, None);
    Ok(())
}

#[test]
fn a_replaced_slice_does_not_block_its_replacements() -> R {
    let mut r = rig()?;
    r.expand(&[slice("A", &[])])?;
    r.close("A", "replaced")?;
    assert_eq!(r.settle()?, None);
    r.expand(&[slice("A.1", &[]), slice("A.2", &[])])?;
    r.close("A.1", "merged")?;
    r.close("A.2", "merged")?;
    assert_eq!(r.settle()?, Some(BUILT));
    Ok(())
}

#[test]
fn slice_tasks_are_sent_from_and_to_the_pump_identity() -> R {
    let mut r = rig()?;
    let tasks = r.expand(&[slice("A", &[]), slice("B", &["A"])])?;
    let pump = r.ctx.cfg.reply_from.clone();
    assert_ne!(pump, r.ctx.cfg.to, "default to must differ for this test");
    for task in &tasks {
        let t = &r.mail.tasks[task];
        assert_eq!((&t.from, &t.to), (&pump, &pump));
    }
    Ok(())
}
