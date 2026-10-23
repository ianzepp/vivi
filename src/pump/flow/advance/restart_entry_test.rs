//! Tests that a restart resumes from its recorded entry when the start failed
//! before it wrote any newer run.

use tempfile::tempdir;

use super::{restart, resume};
use crate::error::VivariumError;
use crate::pump::config::PumpConfig;
use crate::pump::flow::advance::start::start;
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::index::run_state;
use crate::pump::flow::mail::MailWriter;
use crate::pump::host::test_ctx;
use crate::pump::store::{DB_FILENAME, PumpStore};

const NEED: &str = "n1";

#[test]
fn a_restart_with_no_newer_run_resumes_at_the_recorded_entry() -> Result<(), VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let mut mail = FakeMail::new("pump");
    MailWriter::open_record(&mut mail, NEED, "record")?;
    let old = start(&ctx, &store, &mut mail, NEED, None)?;

    // The state a failed read leaves: entry recorded, run restarting, no newer run.
    store.set_restart_entry(NEED, old.run, "plan")?;
    store.set_run_state(NEED, old.run, run_state::RESTARTING)?;
    assert_eq!(store.runs_of(NEED)?.len(), 1);

    resume(&ctx, &store, &mut mail, &old)?;
    let runs = store.runs_of(NEED)?;
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].state, "superseded");
    assert_eq!(runs[1].state, "live");
    assert_eq!(runs[1].entry, "plan");

    resume(&ctx, &store, &mut mail, &old)?;
    let again = store.runs_of(NEED)?;
    assert_eq!(again.len(), 2);
    assert_eq!(again[0].state, "superseded");
    assert_eq!(again[1].state, "live");
    Ok(())
}

#[test]
fn restart_records_its_entry_before_it_marks_the_run_restarting() -> Result<(), VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let mut mail = FakeMail::new("pump");
    MailWriter::open_record(&mut mail, NEED, "record")?;
    let old = start(&ctx, &store, &mut mail, NEED, None)?;
    restart(&ctx, &store, &mut mail, &old, "plan")?;
    assert_eq!(store.restart_entry(NEED, old.run)?.as_deref(), Some("plan"));
    Ok(())
}

#[test]
fn a_restart_with_no_entry_and_no_newer_run_still_errors() -> Result<(), VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let mut mail = FakeMail::new("pump");
    MailWriter::open_record(&mut mail, NEED, "record")?;
    let old = start(&ctx, &store, &mut mail, NEED, None)?;
    store.set_run_state(NEED, old.run, run_state::RESTARTING)?;
    assert!(resume(&ctx, &store, &mut mail, &old).is_err());
    Ok(())
}
