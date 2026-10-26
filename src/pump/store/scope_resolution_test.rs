//! Tests for replay-safe scope resolutions.

use tempfile::{TempDir, tempdir};

use super::{HELD_PATH_PREFIX, PumpStore};
use crate::error::VivariumError;
use crate::pump::flow::index::{StepRow, StepState};
use crate::pump::flow::scope::{BlockedScope, Resolution, ScopeState};
use crate::pump::store::DB_FILENAME;

type R = Result<(), VivariumError>;

struct Fixture {
    _dir: TempDir,
    store: PumpStore,
    slices: u32,
    requests: u32,
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

fn fixture() -> Result<Fixture, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    Ok(Fixture {
        _dir: dir,
        store,
        slices: 0,
        requests: 0,
    })
}

impl Fixture {
    /// Add a slice of (need, run) with a write scope; returns its task handle.
    fn slice(
        &mut self,
        need: &str,
        run: u32,
        scope: &[&str],
        state: StepState,
    ) -> Result<String, VivariumError> {
        self.slices += 1;
        let task = format!("{need}-{run}-slice-{}", self.slices);
        self.store.insert_step(
            need,
            &StepRow {
                run,
                node: format!("slice:s{}", self.slices),
                task: task.clone(),
                state,
                verdict: None,
            },
        )?;
        self.store.insert_slice_scope(&task, run, &strings(scope))?;
        Ok(task)
    }

    /// Apply a fresh blocked-scope request under a fresh source job.
    fn request(
        &mut self,
        task: &str,
        run: u32,
        paths: &[&str],
        design: bool,
    ) -> Result<super::ScopeResolutionRow, VivariumError> {
        self.requests += 1;
        let source = format!("{task}-scope-{}", self.requests);
        let request = self.blocked(paths, design);
        self.store
            .apply_scope_resolution(&source, task, run, &request)
    }

    fn blocked(&self, paths: &[&str], design: bool) -> BlockedScope {
        BlockedScope {
            paths: strings(paths),
            reason: "needs it".into(),
            design,
        }
    }
}

#[test]
fn first_apply_stores_the_whole_settlement() -> R {
    let mut f = fixture()?;
    let a = f.slice("n1", 1, &["src/a.rs"], StepState::Running)?;
    let b = f.slice("n1", 1, &["src/b.rs"], StepState::Running)?;
    let row = f.request(&a, 1, &["src/b.rs", "src/c.rs"], false)?;
    assert_eq!(row.source_job, format!("{a}-scope-1"));
    assert_eq!(row.task, a);
    assert_eq!(row.run, 1);
    assert_eq!(row.requests, 1);
    assert_eq!(row.request, f.blocked(&["src/b.rs", "src/c.rs"], false));
    assert_eq!(row.resolution, Resolution::WaitOn(vec![b.clone()]));
    assert_eq!(row.holders, vec![b.clone()]);
    assert_eq!(row.state, ScopeState::Wait);
    assert_eq!(f.store.scope_requests(&a)?, Some(1));
    // The free path widened; the held one is pending grant evidence.
    assert_eq!(
        f.store.slice_scope(&a)?,
        Some(strings(&["src/a.rs", "src/c.rs"]))
    );
    assert_eq!(
        f.store.scope_holders(&a)?,
        vec![b, format!("{HELD_PATH_PREFIX}src/b.rs")]
    );
    assert_eq!(f.store.scope_state(&a)?, Some(ScopeState::Wait));
    Ok(())
}

#[test]
fn a_replayed_source_returns_the_stored_row_unchanged() -> R {
    let mut f = fixture()?;
    let a = f.slice("n1", 1, &["src/a.rs"], StepState::Running)?;
    let b = f.slice("n1", 1, &["src/b.rs"], StepState::Running)?;
    let source = format!("{a}-scope-1");
    let first = f
        .store
        .apply_scope_resolution(&source, &a, 1, &f.blocked(&["src/b.rs"], false))?;
    // The world moves on: the holder closes and the scope widens.
    f.store.set_step_state(&b, StepState::Done, None)?;
    f.store.widen_slice_scope(&a, &["src/z.rs".to_string()])?;
    let again = f
        .store
        .apply_scope_resolution(&source, &a, 1, &f.blocked(&["src/b.rs"], false))?;
    assert_eq!(again, first);
    assert_eq!(f.store.scope_requests(&a)?, Some(1));
    // The settlement was not recomputed against the changed scopes.
    assert_eq!(again.resolution, Resolution::WaitOn(vec![b]));
    assert_eq!(
        f.store.slice_scope(&a)?,
        Some(strings(&["src/a.rs", "src/z.rs"]))
    );
    Ok(())
}

#[test]
fn a_source_job_settled_for_another_task_is_refused() -> R {
    let mut f = fixture()?;
    let a = f.slice("n1", 1, &["src/a.rs"], StepState::Running)?;
    let b = f.slice("n1", 1, &["src/b.rs"], StepState::Running)?;
    f.store
        .apply_scope_resolution("job-1", &a, 1, &f.blocked(&["src/c.rs"], false))?;
    let error = f
        .store
        .apply_scope_resolution("job-1", &b, 1, &f.blocked(&["src/d.rs"], false))
        .expect_err("refused");
    assert!(error.to_string().contains("already settled"), "{error}");
    assert_eq!(f.store.scope_requests(&b)?, Some(0));
    Ok(())
}

#[test]
fn an_enclosing_directory_request_waits_on_the_held_file() -> R {
    let mut f = fixture()?;
    let a = f.slice("n1", 1, &["src/a.rs"], StepState::Running)?;
    let b = f.slice("n1", 1, &["src/b.rs"], StepState::Running)?;
    let row = f.request(&a, 1, &["src/", "docs/x.md"], false)?;
    assert_eq!(row.resolution, Resolution::WaitOn(vec![b.clone()]));
    assert_eq!(row.holders, vec![b]);
    // The free path widened right away; the enclosed one is granted later.
    assert_eq!(
        f.store.slice_scope(&a)?,
        Some(strings(&["src/a.rs", "docs/x.md"]))
    );
    Ok(())
}

#[test]
fn a_genuine_second_request_still_goes_to_rework() -> R {
    let mut f = fixture()?;
    let a = f.slice("n1", 1, &["src/a.rs"], StepState::Running)?;
    f.request(&a, 1, &["src/b.rs"], false)?;
    let second = f.request(&a, 1, &["src/c.rs"], false)?;
    assert!(matches!(second.resolution, Resolution::Rework(_)));
    assert_eq!(second.requests, 2);
    assert_eq!(second.state, ScopeState::Rework);
    assert!(second.holders.is_empty());
    assert_eq!(f.store.scope_state(&a)?, Some(ScopeState::Rework));
    assert_eq!(
        f.store.slice_scope(&a)?,
        Some(strings(&["src/a.rs", "src/b.rs"]))
    );
    Ok(())
}

#[test]
fn read_accessors_expose_the_count_and_the_outstanding_wait() -> R {
    let mut f = fixture()?;
    let a = f.slice("n1", 1, &["src/a.rs"], StepState::Running)?;
    let b = f.slice("n1", 1, &["src/b.rs"], StepState::Running)?;
    assert_eq!(f.store.scope_requests(&a)?, Some(0));
    assert_eq!(f.store.scope_requests("nope")?, None);
    assert_eq!(f.store.outstanding_scope_resolution(&a)?, None);

    let row = f.request(&a, 1, &["src/b.rs"], false)?;
    assert_eq!(
        f.store.scope_resolution(&row.source_job)?,
        Some(row.clone())
    );
    assert_eq!(f.store.scope_resolution("nope")?, None);
    // The outstanding wait is the newest scope-wait settlement of the task.
    assert_eq!(f.store.outstanding_scope_resolution(&a)?, Some(row));
    assert_eq!(f.store.outstanding_scope_resolution(&b)?, None);
    Ok(())
}
