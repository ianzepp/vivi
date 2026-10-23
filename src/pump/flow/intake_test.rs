//! Tests for the intake phase: which needs get a run, in what order, under
//! the cap, and how a half-started run is finished.

use std::collections::BTreeMap;

use tempfile::{TempDir, tempdir};

use super::{Candidate, register, select};
use crate::error::VivariumError;
use crate::pump::config::PumpConfig;
use crate::pump::flow::advance::start::start;
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::handlers::Registry;
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::tick::tick_with;
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::jobs::{JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, PumpStore};

type R = Result<(), VivariumError>;
type Reg = Registry<Jobs, FakeMail>;

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
    registry: Reg,
}

fn rig(cap: Option<u32>) -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let mut cfg = PumpConfig::default();
    cfg.flow.max_live_runs = cap;
    let (ctx, _host) = test_ctx(dir.path(), cfg);
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    let mut registry = Registry::new();
    register(&mut registry);
    Ok(Rig {
        _dir: dir,
        ctx,
        store,
        mail: FakeMail::new("pump"),
        jobs: Jobs::default(),
        registry,
    })
}

impl Rig {
    fn open(&mut self, need: &str, body: &str) -> R {
        MailWriter::open_record(&mut self.mail, need, body).map(|_| ())
    }

    fn tick(&mut self) -> R {
        tick_with(
            &self.ctx,
            &mut self.jobs,
            &self.store,
            &mut self.mail,
            &self.registry,
        )
    }

    fn runs(&self, need: &str) -> Result<Vec<(u32, String, String)>, VivariumError> {
        Ok(self
            .store
            .runs_of(need)?
            .into_iter()
            .map(|r| (r.run, r.entry, r.state))
            .collect())
    }

    fn started(&self, needs: &[&str]) -> Result<Vec<bool>, VivariumError> {
        needs
            .iter()
            .map(|need| Ok(!self.store.runs_of(need)?.is_empty()))
            .collect()
    }
}

fn cand(need: &str, repair: bool, target: &str, priority: i64, age: u64) -> Candidate {
    Candidate {
        need: need.into(),
        repair,
        target: target.into(),
        priority,
        age,
    }
}

#[test]
fn a_need_with_an_open_record_and_no_run_gets_run_one_at_plan() -> R {
    let mut rig = rig(None)?;
    rig.open("n1", "kind: build")?;
    rig.tick()?;
    assert_eq!(rig.runs("n1")?, [(1, "plan".into(), "live".into())]);
    Ok(())
}

#[test]
fn a_need_with_a_live_run_gets_nothing_more() -> R {
    let mut rig = rig(None)?;
    rig.open("n1", "kind: build")?;
    start(&rig.ctx, &rig.store, &mut rig.mail, "n1", None)?;
    let calls = rig.mail.calls.len();
    rig.tick()?;
    assert_eq!(rig.runs("n1")?.len(), 1);
    assert_eq!(rig.mail.calls.len(), calls);
    Ok(())
}

#[test]
fn a_need_marked_investigate_starts_a_build_run_like_any_other() -> R {
    let mut rig = rig(None)?;
    rig.open("n1", "kind: build")?;
    rig.mail.set_marks("n1", &["investigate"]);
    rig.tick()?;
    assert_eq!(rig.runs("n1")?, [(1, "plan".into(), "live".into())]);
    Ok(())
}

#[test]
fn a_repair_need_starts_a_run_at_fix1() -> R {
    let mut rig = rig(None)?;
    let tested = "a".repeat(40);
    rig.open(
        "n1",
        &format!("kind: repair\ntarget: vivi\ntested: {tested}"),
    )?;
    rig.tick()?;
    let runs = rig.runs("n1")?;
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].1, "fix1");
    assert!(
        rig.mail
            .calls
            .iter()
            .any(|c| format!("{c:?}").contains("SendStep"))
    );
    Ok(())
}

#[test]
fn the_cap_leaves_the_lowest_ranked_need_waiting() -> R {
    let mut rig = rig(Some(3))?;
    for (need, level) in [("n1", 4), ("n2", 3), ("n3", 2), ("n4", 1)] {
        rig.open(need, "kind: build")?;
        rig.store.set_need_priority(need, level, 1)?;
    }
    rig.tick()?;
    assert_eq!(
        rig.started(&["n1", "n2", "n3", "n4"])?,
        [true, true, true, false]
    );
    // The cap holds on the next tick too.
    rig.tick()?;
    assert_eq!(rig.started(&["n4"])?, [false]);
    Ok(())
}

#[test]
fn a_run_left_starting_by_a_failed_start_becomes_live() -> R {
    let mut rig = rig(None)?;
    rig.open("n1", "kind: build")?;
    rig.mail.fail_at = Some(rig.mail.calls.len());
    start(&rig.ctx, &rig.store, &mut rig.mail, "n1", None).unwrap_err();
    assert_eq!(rig.runs("n1")?[0].2, "starting");
    rig.tick()?;
    assert_eq!(rig.runs("n1")?, [(1, "plan".into(), "live".into())]);
    Ok(())
}

#[test]
fn select_orders_by_priority_then_age_and_stops_at_the_cap() {
    let candidates = vec![
        cand("old-low", false, "", 0, 10),
        cand("new-high", false, "", 5, 30),
        cand("old-high", false, "", 5, 20),
        cand("unparsed", false, "", 5, u64::MAX),
    ];
    let all = select(candidates.clone(), 0, None, &[]);
    assert_eq!(all, ["old-high", "new-high", "unparsed", "old-low"]);
    // Two slots left: the two best-ranked start, not the oldest.
    assert_eq!(
        select(candidates, 1, Some(3), &[]),
        ["old-high", "new-high"]
    );
}

#[test]
fn select_puts_repair_first_and_skips_a_second_repair_for_one_target() {
    let candidates = vec![
        cand("build", false, "", 9, 1),
        cand("fix-a2", true, "vivi", 0, 2),
        cand("fix-a1", true, "vivi", 0, 1),
        cand("fix-b", true, "other", 0, 3),
    ];
    assert_eq!(
        select(candidates.clone(), 0, None, &[]),
        ["fix-a1", "fix-b", "build"]
    );
    // A repair run already in flight for the target blocks that target.
    assert_eq!(
        select(candidates, 0, None, &["vivi".to_owned()]),
        ["fix-b", "build"]
    );
}

#[test]
fn select_with_a_full_cap_picks_nothing() {
    let candidates = vec![cand("n1", false, "", 0, 1)];
    assert!(select(candidates, 3, Some(3), &[]).is_empty());
}

#[test]
fn select_never_holds_a_repair_candidate_at_the_cap() {
    let candidates = vec![
        cand("build", false, "", 9, 1),
        cand("fix-a", true, "vivi", 0, 2),
        cand("fix-busy", true, "other", 0, 3),
    ];
    // Cap 1 is full of ordinary runs: the repair for a free target starts,
    // the ordinary candidate and the repair for a busy target do not.
    assert_eq!(
        select(candidates, 1, Some(1), &["other".to_owned()]),
        ["fix-a"]
    );
}

#[test]
fn select_repair_picks_do_not_count_toward_the_cap() {
    let candidates = vec![
        cand("fix-a", true, "vivi", 0, 1),
        cand("fix-b", true, "other", 0, 2),
        cand("n1", false, "", 0, 3),
        cand("n2", false, "", 0, 4),
    ];
    assert_eq!(
        select(candidates, 0, Some(1), &[]),
        ["fix-a", "fix-b", "n1"]
    );
}
