//! Tests for the retained work history of a recorded need branch.

use tempfile::{TempDir, tempdir};

use super::{RetainedSlice, render, retained};
use crate::error::VivariumError;
use crate::pump::flow::index::{RunRow, StepRow, StepState, run_state};
use crate::pump::flow::need_branch::{branch, prior};
use crate::pump::store::{DB_FILENAME, NeedBranchRow, PumpStore};

const NEED: &str = "abcd1234";
const BASE: &str = "1111111111111111111111111111111111111111";
const CHECKOUT: &str = "2222222222222222222222222222222222222222";

type R = Result<(), VivariumError>;

struct Rig {
    _dir: TempDir,
    store: PumpStore,
}

fn rig() -> Rig {
    let dir = tempdir().expect("tempdir");
    let store = PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open");
    Rig { _dir: dir, store }
}

fn run(n: u32, state: &str) -> RunRow {
    RunRow {
        need: NEED.into(),
        run: n,
        flow: "build".into(),
        flow_version: 2,
        kind: "build".into(),
        entry: "admit".into(),
        state: state.into(),
    }
}

fn slice(n: u32, node: &str, task: &str, verdict: Option<&str>) -> StepRow {
    StepRow {
        run: n,
        node: node.into(),
        task: task.into(),
        state: StepState::Done,
        verdict: verdict.map(str::to_owned),
    }
}

fn step(n: u32, node: &str, task: &str, state: StepState, verdict: Option<&str>) -> StepRow {
    StepRow {
        state,
        ..slice(n, node, task, verdict)
    }
}

fn row(priors: Vec<String>, owning: u32) -> NeedBranchRow {
    NeedBranchRow {
        need: NEED.into(),
        branch: branch(NEED),
        base: Some(BASE.into()),
        status: "live".into(),
        run: owning,
        target: "default".into(),
        priors,
    }
}

fn add(store: &PumpStore, row: &StepRow) {
    store.insert_step(NEED, row).expect("insert step");
}

fn identities(slices: &[RetainedSlice]) -> Vec<(u32, &str, &str, &str)> {
    slices
        .iter()
        .map(|s| (s.run, s.node.as_str(), s.task.as_str(), s.verdict.as_str()))
        .collect()
}

#[test]
fn merged_slices_span_carried_runs_through_the_owning_run() -> R {
    let f = rig();
    for (n, state) in [
        (1, run_state::DONE),
        (2, run_state::FINISHING),
        (3, run_state::LIVE),
    ] {
        f.store.insert_run(&run(n, state))?;
    }
    add(&f.store, &slice(1, "slice:a", "t-a", Some("merged")));
    add(&f.store, &slice(2, "slice:b", "t-b", Some("merged")));
    add(&f.store, &slice(3, "slice:c", "t-c", Some("merged")));
    let history = retained(&f.store, &row(vec![], 3))?;
    assert_eq!(
        identities(&history.merged),
        [
            (1, "slice:a", "t-a", "merged"),
            (2, "slice:b", "t-b", "merged"),
            (3, "slice:c", "t-c", "merged"),
        ]
    );
    assert!(history.no_defect.is_empty());
    assert_eq!(history.archived_through, None);
    assert_eq!(history.active_merged().count(), 3);
    Ok(())
}

#[test]
fn a_merged_slice_of_a_superseded_run_is_kept() -> R {
    let f = rig();
    f.store.insert_run(&run(1, run_state::SUPERSEDED))?;
    f.store.insert_run(&run(2, run_state::LIVE))?;
    add(&f.store, &slice(1, "slice:a", "t-a", Some("merged")));
    add(&f.store, &slice(2, "slice:b", "t-b", Some("no-defect")));
    let history = retained(&f.store, &row(vec![], 2))?;
    assert_eq!(
        f.store.run_row(NEED, 1)?.state,
        run_state::SUPERSEDED,
        "the source run really was superseded"
    );
    assert_eq!(
        identities(&history.merged),
        [(1, "slice:a", "t-a", "merged")]
    );
    assert_eq!(
        identities(&history.no_defect),
        [(2, "slice:b", "t-b", "no-defect")]
    );
    Ok(())
}

#[test]
fn the_archival_boundary_excludes_earlier_runs_from_active_contents() -> R {
    let f = rig();
    for n in 1..=3 {
        f.store.insert_run(&run(n, run_state::DONE))?;
    }
    add(&f.store, &slice(1, "slice:a", "t-a", Some("merged")));
    add(&f.store, &slice(2, "slice:b", "t-b", Some("merged")));
    add(&f.store, &slice(3, "slice:c", "t-c", Some("merged")));
    add(&f.store, &slice(3, "slice:d", "t-d", Some("no-defect")));

    let open = retained(&f.store, &row(vec![], 3))?;
    assert_eq!(open.archived_through, None);
    assert_eq!(open.active_merged().count(), 3);
    assert_eq!(open.active_no_defect().count(), 1);

    let archived = retained(&f.store, &row(vec![prior(NEED, 2)], 3))?;
    assert_eq!(archived.archived_through, Some(2));
    assert_eq!(identities(&archived.merged).len(), 3);
    let active: Vec<&str> = archived.active_merged().map(|s| s.task.as_str()).collect();
    assert_eq!(active, ["t-c"]);
    let unchanged: Vec<&str> = archived
        .active_no_defect()
        .map(|s| s.task.as_str())
        .collect();
    assert_eq!(unchanged, ["t-d"]);
    Ok(())
}

#[test]
fn the_newest_matching_prior_sets_the_boundary() -> R {
    let f = rig();
    for n in 1..=4 {
        f.store.insert_run(&run(n, run_state::DONE))?;
    }
    for (n, node, task) in [
        (1, "slice:a", "t-a"),
        (2, "slice:b", "t-b"),
        (3, "slice:c", "t-c"),
        (4, "slice:d", "t-d"),
    ] {
        add(&f.store, &slice(n, node, task, Some("merged")));
    }
    let priors = vec![
        String::from("vivi/branch/other-name"),
        String::from("vivi/need-prior/other-r9"),
        prior(NEED, 1),
        prior(NEED, 3),
    ];
    let history = retained(&f.store, &row(priors, 4))?;
    assert_eq!(history.archived_through, Some(3));
    let active: Vec<&str> = history.active_merged().map(|s| s.task.as_str()).collect();
    assert_eq!(active, ["t-d"]);
    Ok(())
}

#[test]
fn excluded_outcomes_stay_out_of_both_collections() -> R {
    let f = rig();
    for n in 1..=2 {
        f.store.insert_run(&run(n, run_state::DONE))?;
    }
    f.store.insert_run(&run(3, run_state::LIVE))?;
    // A run after the owning run is not part of the history at all.
    add(
        &f.store,
        &slice(3, "slice:later", "t-later", Some("merged")),
    );
    for row in [
        slice(1, "slice:replaced", "t-replaced", Some("replaced")),
        slice(1, "slice:superseded", "t-superseded", Some("superseded")),
        slice(1, "slice:failed", "t-failed", Some("failed")),
        slice(1, "slice:scope", "t-scope", Some("blocked-scope")),
        slice(1, "slice:withdrawn", "t-withdrawn", Some("withdrawn")),
        step(
            1,
            "slice:waiting",
            "t-waiting",
            StepState::Waiting,
            Some("merged"),
        ),
        step(1, "slice:pending", "t-pending", StepState::Pending, None),
        step(1, "slice:running", "t-running", StepState::Running, None),
        step(
            1,
            "slice:failed-row",
            "t-failed-row",
            StepState::Failed,
            Some("merged"),
        ),
        // Not a slice row: node carries no `slice:` prefix.
        slice(1, "build1", "t-build", Some("merged")),
    ] {
        add(&f.store, &row);
    }
    // The owning run keeps exactly one merged slice.
    add(&f.store, &slice(2, "slice:keep", "t-keep", Some("merged")));
    let history = retained(&f.store, &row(vec![], 2))?;
    assert_eq!(
        identities(&history.merged),
        [(2, "slice:keep", "t-keep", "merged")]
    );
    assert!(history.no_defect.is_empty());
    assert_eq!(history.active_merged().count(), 1);
    Ok(())
}

#[test]
fn no_defect_is_not_reported_as_a_merge() -> R {
    let f = rig();
    for n in 1..=2 {
        f.store.insert_run(&run(n, run_state::DONE))?;
    }
    add(&f.store, &slice(1, "slice:a", "t-a", Some("no-defect")));
    add(&f.store, &slice(2, "slice:b", "t-b", Some("merged")));
    let history = retained(&f.store, &row(vec![], 2))?;
    assert_eq!(
        identities(&history.no_defect),
        [(1, "slice:a", "t-a", "no-defect")]
    );
    assert_eq!(
        identities(&history.merged),
        [(2, "slice:b", "t-b", "merged")]
    );
    let text = render(&history, CHECKOUT);
    assert_eq!(text.matches("r1 slice:a t-a no-defect").count(), 1);
    assert!(!text.contains("merged:\n  r1 slice:a"));
    Ok(())
}

#[test]
fn render_names_target_branch_base_checkout_and_each_source() -> R {
    let f = rig();
    for n in 1..=2 {
        f.store.insert_run(&run(n, run_state::DONE))?;
    }
    add(&f.store, &slice(1, "slice:a", "t-a", Some("merged")));
    add(&f.store, &slice(2, "slice:b", "t-b", Some("no-defect")));
    let history = retained(&f.store, &row(vec![], 2))?;
    let text = render(&history, CHECKOUT);
    for expected in [
        "target: default",
        format!("branch: {}", branch(NEED)).as_str(),
        format!("base: {BASE}").as_str(),
        format!("checkout: {CHECKOUT}").as_str(),
        "plan or cut remaining work only",
        "r1 slice:a t-a merged",
        "r2 slice:b t-b no-defect",
    ] {
        assert!(text.contains(expected), "missing `{expected}` in:\n{text}");
    }
    assert_eq!(text.matches("r1 slice:a t-a merged").count(), 1);

    let mut fresh = row(vec![], 2);
    fresh.base = None;
    let text = render(&retained(&f.store, &fresh)?, CHECKOUT);
    assert!(!text.contains("base:"), "no base line without one:\n{text}");
    Ok(())
}
