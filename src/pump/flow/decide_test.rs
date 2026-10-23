//! Tests for the join table and the raise check.

use serde_json::json;
use tempfile::{TempDir, tempdir};

use super::decide;
use crate::error::VivariumError;
use crate::pump::config::PumpConfig;
use crate::pump::flow::advance::route::JoinInput;
use crate::pump::flow::def::{FlowNode, NodeClass};
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::handlers::{self, Registry};
use crate::pump::flow::index::{StepRow, StepState};
use crate::pump::host::test_ctx;
use crate::pump::state::State;
use crate::pump::store::step_payload::StepPayloadRow;
use crate::pump::store::{DB_FILENAME, PumpStore};

fn open() -> Result<(TempDir, PumpStore), VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    Ok((dir, store))
}

fn node(id: &str, class: NodeClass) -> FlowNode {
    FlowNode {
        id: id.into(),
        class,
        label: String::new(),
    }
}

fn input(node: &str, task: &str, word: &str) -> JoinInput {
    JoinInput {
        node: node.into(),
        task: task.into(),
        word: word.into(),
    }
}

/// Store an auditor payload for `task`, with a raise rung when given.
fn audit_payload(
    store: &PumpStore,
    task: &str,
    run: u32,
    rung: Option<&str>,
) -> Result<(), VivariumError> {
    let front = match rung {
        Some(rung) => json!({"answer": "residual", "raise_rung": rung}),
        None => json!({"answer": "residual"}),
    };
    store.record_step_payload(&StepPayloadRow {
        need: "n1".into(),
        run,
        node: "audit1".into(),
        class: "auditor".into(),
        task: task.into(),
        payload: json!({"front": front}),
    })
}

fn join_word(
    store: &PumpStore,
    audit: &str,
    rung: &str,
    raise: Option<&str>,
) -> Result<String, VivariumError> {
    audit_payload(store, "t-audit", 1, raise)?;
    decide(
        store,
        &node("join", NodeClass::Join),
        &[
            input("audit1", "t-audit", audit),
            input("rung", "t-rung", rung),
        ],
    )
}

#[test]
fn join_table_covers_every_row() -> Result<(), VivariumError> {
    let (_dir, store) = open()?;
    let rows = [
        ("clean_pass", "pass", "verified"),
        ("residual", "pass", "verified"),
        ("clean_pass", "fail", "judge"),
        ("residual", "fail", "judge"),
        ("block_range", "pass", "block"),
        ("block_range", "fail", "block"),
        ("block_ship", "pass", "block"),
        ("block_ship", "fail", "block"),
    ];
    for (audit, rung, want) in rows {
        assert_eq!(
            join_word(&store, audit, rung, None)?,
            want,
            "{audit}+{rung}"
        );
    }
    Ok(())
}

#[test]
fn a_stored_raise_rung_raises_a_passing_join() -> Result<(), VivariumError> {
    let (_dir, store) = open()?;
    assert_eq!(
        join_word(&store, "clean_pass", "pass", Some("main"))?,
        "raised"
    );
    assert_eq!(
        join_word(&store, "residual", "pass", Some("need"))?,
        "raised"
    );
    // An empty rung is no rung.
    assert_eq!(join_word(&store, "residual", "pass", Some(""))?, "verified");
    // A failing rung still goes to the judge, and a block still blocks.
    assert_eq!(
        join_word(&store, "residual", "fail", Some("main"))?,
        "judge"
    );
    assert_eq!(
        join_word(&store, "block_ship", "pass", Some("main"))?,
        "block"
    );
    Ok(())
}

#[test]
fn input_order_does_not_matter() -> Result<(), VivariumError> {
    let (_dir, store) = open()?;
    audit_payload(&store, "t-audit", 1, Some("release"))?;
    let word = decide(
        &store,
        &node("join", NodeClass::Join),
        &[
            input("rung", "t-rung", "pass"),
            input("audit1", "t-audit", "clean_pass"),
        ],
    )?;
    assert_eq!(word, "raised");
    Ok(())
}

#[test]
fn a_failed_input_fails_the_join() -> Result<(), VivariumError> {
    let (_dir, store) = open()?;
    let join = node("join", NodeClass::Join);
    for pair in [
        [input("audit1", "a", "failed"), input("rung", "r", "pass")],
        [
            input("audit1", "a", "block_ship"),
            input("rung", "r", "failed"),
        ],
        [input("audit1", "a", "failed"), input("rung", "r", "failed")],
    ] {
        assert_eq!(decide(&store, &join, &pair)?, "failed");
    }
    Ok(())
}

#[test]
fn a_bad_join_names_the_node() -> Result<(), VivariumError> {
    let (_dir, store) = open()?;
    let join = node("join", NodeClass::Join);
    let cases: [Vec<JoinInput>; 4] = [
        vec![input("audit1", "a", "clean_pass")],
        vec![],
        vec![input("audit1", "a", "mystery"), input("rung", "r", "pass")],
        vec![
            input("audit1", "a", "clean_pass"),
            input("audit2", "b", "residual"),
        ],
    ];
    for inputs in cases {
        let Err(error) = decide(&store, &join, &inputs) else {
            return Err(VivariumError::Other("a bad join answered".into()));
        };
        assert!(error.to_string().contains("`join`"), "unnamed: {error}");
    }
    Ok(())
}

fn judge_step(store: &PumpStore, task: &str, run: u32) -> Result<(), VivariumError> {
    store.insert_step(
        "n1",
        &StepRow {
            run,
            node: "judge".into(),
            task: task.into(),
            state: StepState::Done,
            verdict: None,
        },
    )
}

fn raise_word(store: &PumpStore, task: &str) -> Result<String, VivariumError> {
    decide(
        store,
        &node("raisecheck", NodeClass::Raisecheck),
        &[input("judge", task, "residual")],
    )
}

#[test]
fn raisecheck_reads_the_newest_audit_of_the_run() -> Result<(), VivariumError> {
    let (_dir, store) = open()?;
    judge_step(&store, "t-judge", 1)?;
    audit_payload(&store, "t-audit", 1, Some("main"))?;
    assert_eq!(raise_word(&store, "t-judge")?, "raised");

    // A later audit of the same run without a raise rung wins.
    audit_payload(&store, "t-audit2", 1, None)?;
    assert_eq!(raise_word(&store, "t-judge")?, "none");
    Ok(())
}

#[test]
fn raisecheck_is_none_without_a_raise_or_an_audit() -> Result<(), VivariumError> {
    let (_dir, store) = open()?;
    judge_step(&store, "t-judge", 1)?;
    assert_eq!(raise_word(&store, "t-judge")?, "none");
    audit_payload(&store, "t-audit", 1, None)?;
    assert_eq!(raise_word(&store, "t-judge")?, "none");
    // An audit of another run does not count.
    audit_payload(&store, "t-other", 2, Some("main"))?;
    assert_eq!(raise_word(&store, "t-judge")?, "none");
    Ok(())
}

#[test]
fn raisecheck_errors_on_a_bad_shape() -> Result<(), VivariumError> {
    let (_dir, store) = open()?;
    let check = node("raisecheck", NodeClass::Raisecheck);
    assert!(decide(&store, &check, &[]).is_err());
    assert!(decide(&store, &check, &[input("judge", "unknown", "residual")]).is_err());
    let two = [
        input("judge", "a", "residual"),
        input("judge", "b", "residual"),
    ];
    assert!(decide(&store, &check, &two).is_err());
    Ok(())
}

#[test]
fn another_class_has_no_rule() -> Result<(), VivariumError> {
    let (_dir, store) = open()?;
    let Err(error) = decide(&store, &node("audit1", NodeClass::Auditor), &[]) else {
        return Err(VivariumError::Other("an auditor node was decided".into()));
    };
    assert!(error.to_string().contains("`audit1`"), "unnamed: {error}");
    Ok(())
}

#[test]
fn shipped_registry_decides_a_join() -> Result<(), VivariumError> {
    let (dir, store) = open()?;
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let registry: Registry<State, FakeMail> = handlers::shipped(&ctx);
    audit_payload(&store, "t-audit", 1, None)?;
    let word = registry.decide(
        &store,
        &node("join", NodeClass::Join),
        &[
            input("audit1", "t-audit", "clean_pass"),
            input("rung", "t-rung", "pass"),
        ],
    )?;
    assert_eq!(word, "verified");
    Ok(())
}
