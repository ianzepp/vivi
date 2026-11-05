//! Tests that a restart recovers from its recorded entry when the start failed
//! before it wrote any newer run: intake starts the parked build at that entry,
//! an entryless restart stays an error, and a blocked one waits its turn.

use std::collections::BTreeMap;

use tempfile::{TempDir, tempdir};

use super::{RestartResult, restart, resume};
use crate::pump::config::PumpConfig;
use crate::pump::flow::advance::start::start;
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::handlers::Registry;
use crate::pump::flow::index::run_state;
use crate::pump::flow::intake::register as register_intake;
use crate::pump::flow::mail::{MailReader, MailWriter};
use crate::pump::flow::tick::tick_with;
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::jobs::{JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, PumpStore};
use vivi_error::VivariumError;

const NEED: &str = "n1";

#[derive(Default)]
struct Jobs {
    jobs: BTreeMap<String, PoolJob>,
}

impl JobStore for Jobs {
    fn jobs(&self) -> &BTreeMap<String, PoolJob> {
        &self.jobs
    }

    fn jobs_mut(&mut self) -> &mut BTreeMap<String, PoolJob> {
        &mut self.jobs
    }
}

struct Rig {
    _dir: TempDir,
    ctx: Ctx,
    store: PumpStore,
    mail: FakeMail,
    jobs: Jobs,
    registry: Registry<Jobs, FakeMail>,
}

/// A need with a live run 1 and both tick phases that finish a restart.
fn rig() -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let mut mail = FakeMail::new("pump");
    MailWriter::open_record(&mut mail, NEED, "record")?;
    start(&ctx, &store, &mut mail, NEED, None)?;
    let mut registry = Registry::new();
    super::register(&mut registry);
    register_intake(&mut registry);
    Ok(Rig {
        _dir: dir,
        ctx,
        store,
        mail,
        jobs: Jobs::default(),
        registry,
    })
}

impl Rig {
    fn old(&self) -> Result<crate::pump::flow::index::RunRow, VivariumError> {
        self.store
            .runs_of(NEED)?
            .into_iter()
            .find(|r| r.run == 1)
            .ok_or_else(|| VivariumError::Other("no run 1".into()))
    }

    fn tick(&mut self) -> Result<(), VivariumError> {
        tick_with(
            &self.ctx,
            &mut self.jobs,
            &self.store,
            &mut self.mail,
            &self.registry,
        )
    }

    fn runs(&self) -> Result<Vec<(u32, String, String)>, VivariumError> {
        Ok(self
            .store
            .runs_of(NEED)?
            .into_iter()
            .map(|r| (r.run, r.entry, r.state))
            .collect())
    }

    /// The state a failed read leaves: entry recorded, run restarting, no
    /// newer run.
    fn park(&self, entry: &str) -> Result<(), VivariumError> {
        let old = self.old()?;
        self.store.set_restart_entry(NEED, old.run, entry)?;
        self.store
            .set_run_state(NEED, old.run, run_state::RESTARTING)
    }
}

#[test]
fn a_restart_with_no_newer_run_resumes_at_the_recorded_entry() -> Result<(), VivariumError> {
    let mut rig = rig()?;
    rig.park("plan")?;
    assert_eq!(rig.store.runs_of(NEED)?.len(), 1);

    rig.tick()?;
    assert_eq!(
        rig.runs()?,
        [
            (1, "plan".into(), "superseded".into()),
            (2, "plan".into(), "live".into())
        ]
    );

    rig.tick()?;
    assert_eq!(
        rig.runs()?,
        [
            (1, "plan".into(), "superseded".into()),
            (2, "plan".into(), "live".into())
        ],
        "a repeated tick starts no second successor"
    );
    Ok(())
}

#[test]
fn restart_records_its_entry_before_it_marks_the_run_restarting() -> Result<(), VivariumError> {
    let mut rig = rig()?;
    let old = rig.old()?;
    let started = restart(&rig.ctx, &rig.store, &mut rig.mail, &old, "plan")?;
    assert!(matches!(started, RestartResult::Started(_)));
    assert_eq!(
        rig.store.restart_entry(NEED, old.run)?.as_deref(),
        Some("plan")
    );
    Ok(())
}

#[test]
fn a_restart_with_no_entry_and_no_newer_run_still_errors() -> Result<(), VivariumError> {
    let mut rig = rig()?;
    let old = rig.old()?;
    rig.store
        .set_run_state(NEED, old.run, run_state::RESTARTING)?;
    assert!(resume(&rig.ctx, &rig.store, &mut rig.mail, &old).is_err());
    Ok(())
}

#[test]
fn a_blocked_recovery_waits_at_its_recorded_entry_then_starts_it() -> Result<(), VivariumError> {
    let mut rig = rig()?;
    rig.park("rework1")?;
    rig.mail.set_prerequisites(NEED, &["dep"]);

    rig.tick()?;
    assert_eq!(
        rig.runs()?,
        [(1, "plan".into(), "restarting".into())],
        "no successor artifacts"
    );
    assert_eq!(rig.mail.sentinel_count(NEED)?, 1, "no successor sentinel");
    assert_eq!(
        rig.store.restart_entry(NEED, 1)?.as_deref(),
        Some("rework1")
    );

    rig.tick()?;
    assert_eq!(
        rig.runs()?,
        [(1, "plan".into(), "restarting".into())],
        "a repeated tick still waits"
    );

    rig.mail.set_prerequisites(NEED, &[]);
    rig.tick()?;
    assert_eq!(
        rig.runs()?,
        [
            (1, "plan".into(), "superseded".into()),
            (2, "rework1".into(), "live".into())
        ]
    );
    Ok(())
}
