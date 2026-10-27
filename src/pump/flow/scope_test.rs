//! Tests for blocked-scope resolution.

use std::collections::BTreeMap;

use tempfile::{TempDir, tempdir};

use super::{
    BlockedScope, Resolution, ScopeState, finish_grant, grant_held, resolve, wait_holders,
};
use crate::error::VivariumError;
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::index::{StepRow, StepState};
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::need_scope::RulingDecision;
use crate::pump::store::{DB_FILENAME, PumpStore, ScopeRequest};

struct Fixture {
    _dir: TempDir,
    store: PumpStore,
    mail: FakeMail,
    /// The need each slice task belongs to, for journal identities.
    needs: BTreeMap<String, String>,
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

fn fixture() -> Fixture {
    let dir = tempdir().expect("tempdir");
    let store = PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open");
    Fixture {
        _dir: dir,
        store,
        mail: FakeMail::new("mind"),
        needs: BTreeMap::new(),
    }
}

impl Fixture {
    /// Add a slice of (need, run) with a write scope; returns its task handle.
    fn slice(&mut self, need: &str, run: u32, scope: &[&str], state: StepState) -> String {
        let task = self
            .mail
            .send_step("mind", "hand", "slice", "body", &[])
            .expect("send");
        self.needs.insert(task.clone(), need.to_string());
        self.store
            .insert_step(
                need,
                &StepRow {
                    run,
                    node: "slice".into(),
                    task: task.clone(),
                    state,
                    verdict: None,
                },
            )
            .expect("step");
        self.store
            .insert_slice_scope(&task, run, &strings(scope))
            .expect("scope");
        task
    }

    /// Settle a blocked-scope request; panics on a store or mail failure.
    /// Each genuinely new request names its own `source` key; a replay
    /// passes the source of the request it replays.
    fn resolve(
        &mut self,
        run: u32,
        task: &str,
        source: &str,
        paths: &[&str],
        design: bool,
    ) -> Resolution {
        self.try_resolve(run, task, source, paths, design)
            .expect("resolve")
    }

    /// Settle a blocked-scope request, keeping store or mail failures.
    fn try_resolve(
        &mut self,
        run: u32,
        task: &str,
        source: &str,
        paths: &[&str],
        design: bool,
    ) -> Result<Resolution, VivariumError> {
        let req = BlockedScope {
            paths: strings(paths),
            reason: "needs it".into(),
            design,
        };
        let request = self.journal_request(run, task, source, &req);
        resolve(&self.store, &mut self.mail, &request, &req)
    }

    /// The journal identity of `task`'s request under `source`.
    fn journal_request(
        &self,
        run: u32,
        task: &str,
        source: &str,
        req: &BlockedScope,
    ) -> ScopeRequest {
        ScopeRequest {
            source: source.to_string(),
            need: self.needs[task].clone(),
            target: "default".to_string(),
            run,
            node: "slice".to_string(),
            task: task.to_string(),
            attempt: 1,
            paths: req.paths.clone(),
            reason: req.reason.clone(),
            design: req.design,
            decision: RulingDecision::Grant,
            issue: None,
        }
    }
}

/// The blocked-scope requests counted on `task` in run 1.
fn counted(store: &PumpStore, task: &str) -> u32 {
    store
        .scope_coordination_snapshot(task, 1)
        .expect("snapshot")
        .requests
}

#[test]
fn no_holder_widens_and_stores_the_scope() {
    let mut f = fixture();
    let a = f.slice("n1", 1, &["src/a.rs"], StepState::Running);
    let got = f.resolve(1, &a, "s1", &["src/b.rs"], false);
    assert_eq!(got, Resolution::Widen(strings(&["src/b.rs"])));
    assert_eq!(
        f.store.slice_scope(&a).expect("scope"),
        Some(strings(&["src/a.rs", "src/b.rs"]))
    );
    assert_eq!(
        f.store.scope_state(&a).expect("state"),
        Some(ScopeState::Clear)
    );
}

#[test]
fn a_pending_holder_makes_the_slice_wait_with_a_dependency_edge() {
    let mut f = fixture();
    let a = f.slice("n1", 1, &["src/a.rs"], StepState::Running);
    let b = f.slice("n1", 1, &["src/b.rs"], StepState::Pending);
    let got = f.resolve(1, &a, "s1", &["src/b.rs", "src/c.rs"], false);
    assert_eq!(got, Resolution::WaitOn(vec![b.clone()]));
    assert_eq!(f.mail.tasks[&a].depends_on, vec![b.clone()]);
    assert_eq!(wait_holders(&f.store, &a).expect("holders"), vec![b]);
    assert_eq!(
        f.store.scope_state(&a).expect("state"),
        Some(ScopeState::Wait)
    );
    // The free path was widened right away; the held one was not.
    assert_eq!(
        f.store.slice_scope(&a).expect("scope"),
        Some(strings(&["src/a.rs", "src/c.rs"]))
    );
}

#[test]
fn a_directory_entry_holds_paths_under_it() {
    let mut f = fixture();
    let a = f.slice("n1", 1, &["src/a.rs"], StepState::Running);
    let b = f.slice("n1", 1, &["src/pump/"], StepState::Running);
    let got = f.resolve(1, &a, "s1", &["src/pump/run.rs"], false);
    assert_eq!(got, Resolution::WaitOn(vec![b]));
}

#[test]
fn a_directory_entry_needs_the_slash_to_hold() {
    let mut f = fixture();
    let a = f.slice("n1", 1, &["src/a.rs"], StepState::Running);
    f.slice("n1", 1, &["src/pump"], StepState::Running);
    let got = f.resolve(1, &a, "s1", &["src/pump/run.rs"], false);
    assert_eq!(got, Resolution::Widen(strings(&["src/pump/run.rs"])));
}

#[test]
fn a_closed_slice_does_not_hold() {
    let mut f = fixture();
    let a = f.slice("n1", 1, &["src/a.rs"], StepState::Running);
    f.slice("n1", 1, &["src/b.rs"], StepState::Done);
    f.slice("n1", 1, &["src/c.rs"], StepState::Failed);
    let got = f.resolve(1, &a, "s1", &["src/b.rs", "src/c.rs"], false);
    assert_eq!(got, Resolution::Widen(strings(&["src/b.rs", "src/c.rs"])));
}

#[test]
fn a_second_request_goes_to_rework() {
    let mut f = fixture();
    let a = f.slice("n1", 1, &["src/a.rs"], StepState::Running);
    assert!(matches!(
        f.resolve(1, &a, "s1", &["src/b.rs"], false),
        Resolution::Widen(_)
    ));
    assert!(matches!(
        f.resolve(1, &a, "s2", &["src/c.rs"], false),
        Resolution::Rework(_)
    ));
    assert_eq!(
        f.store.slice_scope(&a).expect("scope"),
        Some(strings(&["src/a.rs", "src/b.rs"]))
    );
}

#[test]
fn six_paths_go_to_rework_and_five_do_not() {
    let mut f = fixture();
    let a = f.slice("n1", 1, &["src/a.rs"], StepState::Running);
    let six = ["x/1", "x/2", "x/3", "x/4", "x/5", "x/6"];
    assert!(matches!(
        f.resolve(1, &a, "s1", &six, false),
        Resolution::Rework(_)
    ));
    let b = f.slice("n1", 1, &["src/b.rs"], StepState::Running);
    assert!(matches!(
        f.resolve(1, &b, "s2", &six[..5], false),
        Resolution::Widen(_)
    ));
}

#[test]
fn the_design_flag_goes_to_rework() {
    let mut f = fixture();
    let a = f.slice("n1", 1, &["src/a.rs"], StepState::Running);
    assert!(matches!(
        f.resolve(1, &a, "s1", &["src/b.rs"], true),
        Resolution::Rework(_)
    ));
}

#[test]
fn a_slice_in_another_run_or_need_never_holds() {
    let mut f = fixture();
    let a = f.slice("n1", 2, &["src/a.rs"], StepState::Running);
    f.slice("n1", 1, &["src/b.rs"], StepState::Running);
    f.slice("n2", 2, &["src/b.rs"], StepState::Running);
    let got = f.resolve(2, &a, "s1", &["src/b.rs"], false);
    assert_eq!(got, Resolution::Widen(strings(&["src/b.rs"])));
}

#[test]
fn scope_rows_round_trip() {
    let mut f = fixture();
    let a = f.slice("n1", 1, &["src/a.rs"], StepState::Running);
    assert_eq!(f.store.bump_scope_requests(&a).expect("bump"), 1);
    assert_eq!(f.store.bump_scope_requests(&a).expect("bump"), 2);
    f.store
        .set_scope_state(&a, ScopeState::Rework)
        .expect("set");
    assert_eq!(
        f.store.scope_state(&a).expect("get"),
        Some(ScopeState::Rework)
    );
    assert_eq!(f.store.scope_state("nope").expect("get"), None);
    assert!(f.store.bump_scope_requests("nope").is_err());
}

#[test]
fn granting_the_held_paths_widens_the_scope_and_the_next_request_is_the_second() {
    let mut f = fixture();
    let a = f.slice("n1", 1, &["src/a.rs"], StepState::Running);
    f.slice("n1", 1, &["src/b.rs"], StepState::Running);
    f.resolve(1, &a, "s1", &["src/b.rs", "src/c.rs"], false);

    grant_held(&f.store, &a).expect("grant");
    assert_eq!(
        f.store.slice_scope(&a).expect("scope"),
        Some(strings(&["src/a.rs", "src/c.rs", "src/b.rs"]))
    );
    finish_grant(&f.store, &a).expect("finish");
    assert!(wait_holders(&f.store, &a).expect("holders").is_empty());

    // The request that ended in the wait still counts: the next one is the
    // second and goes to the slicer.
    assert!(matches!(
        f.resolve(1, &a, "s2", &["src/d.rs"], false),
        Resolution::Rework(_)
    ));
}

#[test]
fn requested_directory_waits_for_descendant_holder() {
    let mut f = fixture();
    let a = f.slice("n1", 1, &["src/a.rs"], StepState::Running);
    let b = f.slice("n1", 1, &["src/pump/run.rs"], StepState::Pending);
    let got = f.resolve(1, &a, "s1", &["src/pump/"], false);
    assert_eq!(got, Resolution::WaitOn(vec![b.clone()]));
    assert_eq!(f.mail.tasks[&a].depends_on, vec![b.clone()]);
    assert_eq!(
        f.store.scope_holders(&a).expect("holders"),
        strings(&[b.as_str(), "path:src/pump/"])
    );
    assert_eq!(
        f.store.scope_state(&a).expect("state"),
        Some(ScopeState::Wait)
    );

    // A sibling directory prefix is component-aware: no conflict, so the
    // request is free and widens the asking slice.
    let c = f.slice("n1", 1, &["src/keep.rs"], StepState::Running);
    let got = f.resolve(1, &c, "s2", &["src/pump-other/"], false);
    assert_eq!(got, Resolution::Widen(strings(&["src/pump-other/"])));
    assert_eq!(
        f.store.scope_state(&c).expect("state"),
        Some(ScopeState::Clear)
    );
}

#[test]
fn scope_resolution_replay_does_not_increment_request_count() {
    let mut f = fixture();
    let a = f.slice("n1", 1, &["src/a.rs"], StepState::Running);
    let b = f.slice("n1", 1, &["src/b.rs"], StepState::Pending);

    // The settlement commits, then its dependency write fails: the slice
    // task vanished from the mail after it was sent.
    let task = f.mail.tasks.remove(&a).expect("slice task");
    assert!(f.try_resolve(1, &a, "s1", &["src/b.rs"], false).is_err());
    assert_eq!(
        f.store.scope_state(&a).expect("state"),
        Some(ScopeState::Wait)
    );
    assert_eq!(counted(&f.store, &a), 1);
    assert_eq!(
        f.store.scope_holders(&a).expect("holders"),
        strings(&[b.as_str(), "path:src/b.rs"])
    );

    // The mail heals, and the same source is collected again.
    f.mail.tasks.insert(a.clone(), task);

    // The replay adopts the recorded resolution: no second request is
    // counted, nothing is widened, and the dependency write is redone from
    // the recorded holder set.
    let got = f.resolve(1, &a, "s1", &["src/b.rs"], false);
    assert_eq!(got, Resolution::WaitOn(vec![b.clone()]));
    assert_eq!(counted(&f.store, &a), 1, "replay counts no request");
    assert_eq!(f.mail.tasks[&a].depends_on, vec![b]);
    assert_eq!(
        f.store.slice_scope(&a).expect("scope"),
        Some(strings(&["src/a.rs"])),
        "the held path is not granted by the wait"
    );
}
