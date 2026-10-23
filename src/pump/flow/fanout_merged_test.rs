//! Tests for [`super::close_slice_merged`] over a recording mail writer.

use tempfile::{TempDir, tempdir};

use super::{VERDICT_MERGED, close_slice_merged, slice_node};
use crate::error::VivariumError;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::mail::MailWriter;
use crate::pump::store::{DB_FILENAME, PumpStore};

type R = Result<(), VivariumError>;

const NEED: &str = "n1";
const TASK: &str = "t-slice";

/// A close as the writer saw it: handle, verdict, branch, commit.
type Closed = (String, String, Option<String>, Option<String>);

/// A mail writer that records only task closes; every other write is unused.
#[derive(Default)]
struct Recorder {
    closes: Vec<Closed>,
    fail_close: bool,
}

impl MailWriter for Recorder {
    fn send_step(
        &mut self,
        _from: &str,
        _to: &str,
        _subject: &str,
        _body: &str,
        _depends_on: &[String],
    ) -> Result<String, VivariumError> {
        Ok(String::new())
    }

    fn bind(&mut self, _need: &str, _units: &[String]) -> R {
        Ok(())
    }

    fn reply(&mut self, _handle: &str, _body: &str) -> R {
        Ok(())
    }

    fn post_to_need_audience(&mut self, _need: &str, _body: &str) -> R {
        Ok(())
    }

    fn close_task(
        &mut self,
        handle: &str,
        verdict: &str,
        branch: Option<&str>,
        commit: Option<&str>,
    ) -> R {
        if self.fail_close {
            return Err(VivariumError::Other("close failed".into()));
        }
        self.closes.push((
            handle.into(),
            verdict.into(),
            branch.map(str::to_string),
            commit.map(str::to_string),
        ));
        Ok(())
    }

    fn reopen_task(&mut self, _handle: &str) -> R {
        Ok(())
    }

    fn close_need(&mut self, _need: &str, _note: &str) -> R {
        Ok(())
    }

    fn open_record(&mut self, _need: &str, _body: &str) -> Result<String, VivariumError> {
        Ok(String::new())
    }

    fn close_record(&mut self, _need: &str, _outcome: &str) -> R {
        Ok(())
    }
}

fn store() -> Result<(TempDir, PumpStore), VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    store.insert_run(&RunRow {
        need: NEED.into(),
        run: 1,
        flow: "build".into(),
        flow_version: 2,
        kind: "build".into(),
        entry: "plan".into(),
        state: "live".into(),
    })?;
    store.insert_step(
        NEED,
        &StepRow {
            run: 1,
            node: slice_node("a"),
            task: TASK.into(),
            state: StepState::Running,
            verdict: None,
        },
    )?;
    Ok((dir, store))
}

#[test]
fn merged_close_carries_verdict_branch_and_commit() -> R {
    let (_dir, store) = store()?;
    let mut mail = Recorder::default();
    close_slice_merged(&store, &mut mail, TASK, "vivi/need/n1", "abc123")?;
    assert_eq!(
        mail.closes,
        vec![(
            TASK.to_string(),
            VERDICT_MERGED.to_string(),
            Some("vivi/need/n1".to_string()),
            Some("abc123".to_string()),
        )]
    );
    let row = store.step_by_task(TASK)?.expect("row");
    assert_eq!(row.state, StepState::Done);
    assert_eq!(row.verdict.as_deref(), Some(VERDICT_MERGED));
    Ok(())
}

#[test]
fn failed_close_leaves_the_step_row_running() -> R {
    let (_dir, store) = store()?;
    let mut mail = Recorder {
        fail_close: true,
        ..Recorder::default()
    };
    assert!(close_slice_merged(&store, &mut mail, TASK, "b", "c").is_err());
    let row = store.step_by_task(TASK)?.expect("row");
    assert_eq!(row.state, StepState::Running);
    assert_eq!(row.verdict, None);
    Ok(())
}
