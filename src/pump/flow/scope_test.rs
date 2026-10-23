//! Tests for blocked-scope resolution.

use tempfile::{TempDir, tempdir};

use super::{BlockedScope, Resolution, ScopeState, resolve};
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::index::{StepRow, StepState};
use crate::pump::flow::mail::MailWriter;
use crate::pump::store::{DB_FILENAME, PumpStore};

struct Fixture {
    _dir: TempDir,
    store: PumpStore,
    mail: FakeMail,
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
    }
}

impl Fixture {
    /// Add a slice of (need, run) with a write scope; returns its task handle.
    fn slice(&mut self, need: &str, run: u32, scope: &[&str], state: StepState) -> String {
        let task = self
            .mail
            .send_step("mind", "hand", "slice", "body", &[])
            .expect("send");
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

    fn resolve(&mut self, run: u32, task: &str, paths: &[&str], design: bool) -> Resolution {
        let req = BlockedScope {
            paths: strings(paths),
            reason: "needs it".into(),
            design,
        };
        resolve(&self.store, &mut self.mail, run, task, &req).expect("resolve")
    }
}

#[test]
fn no_holder_widens_and_stores_the_scope() {
    let mut f = fixture();
    let a = f.slice("n1", 1, &["src/a.rs"], StepState::Running);
    let got = f.resolve(1, &a, &["src/b.rs"], false);
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
    let got = f.resolve(1, &a, &["src/b.rs", "src/c.rs"], false);
    assert_eq!(got, Resolution::WaitOn(vec![b.clone()]));
    assert_eq!(f.mail.tasks[&a].depends_on, vec![b.clone()]);
    assert_eq!(f.store.scope_holders(&a).expect("holders"), vec![b]);
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
    let got = f.resolve(1, &a, &["src/pump/run.rs"], false);
    assert_eq!(got, Resolution::WaitOn(vec![b]));
}

#[test]
fn a_directory_entry_needs_the_slash_to_hold() {
    let mut f = fixture();
    let a = f.slice("n1", 1, &["src/a.rs"], StepState::Running);
    f.slice("n1", 1, &["src/pump"], StepState::Running);
    let got = f.resolve(1, &a, &["src/pump/run.rs"], false);
    assert_eq!(got, Resolution::Widen(strings(&["src/pump/run.rs"])));
}

#[test]
fn a_closed_slice_does_not_hold() {
    let mut f = fixture();
    let a = f.slice("n1", 1, &["src/a.rs"], StepState::Running);
    f.slice("n1", 1, &["src/b.rs"], StepState::Done);
    f.slice("n1", 1, &["src/c.rs"], StepState::Failed);
    let got = f.resolve(1, &a, &["src/b.rs", "src/c.rs"], false);
    assert_eq!(got, Resolution::Widen(strings(&["src/b.rs", "src/c.rs"])));
}

#[test]
fn a_second_request_goes_to_repair() {
    let mut f = fixture();
    let a = f.slice("n1", 1, &["src/a.rs"], StepState::Running);
    assert!(matches!(
        f.resolve(1, &a, &["src/b.rs"], false),
        Resolution::Widen(_)
    ));
    assert!(matches!(
        f.resolve(1, &a, &["src/c.rs"], false),
        Resolution::Repair(_)
    ));
    assert_eq!(
        f.store.slice_scope(&a).expect("scope"),
        Some(strings(&["src/a.rs", "src/b.rs"]))
    );
}

#[test]
fn six_paths_go_to_repair_and_five_do_not() {
    let mut f = fixture();
    let a = f.slice("n1", 1, &["src/a.rs"], StepState::Running);
    let six = ["x/1", "x/2", "x/3", "x/4", "x/5", "x/6"];
    assert!(matches!(
        f.resolve(1, &a, &six, false),
        Resolution::Repair(_)
    ));
    let b = f.slice("n1", 1, &["src/b.rs"], StepState::Running);
    assert!(matches!(
        f.resolve(1, &b, &six[..5], false),
        Resolution::Widen(_)
    ));
}

#[test]
fn the_design_flag_goes_to_repair() {
    let mut f = fixture();
    let a = f.slice("n1", 1, &["src/a.rs"], StepState::Running);
    assert!(matches!(
        f.resolve(1, &a, &["src/b.rs"], true),
        Resolution::Repair(_)
    ));
}

#[test]
fn a_slice_in_another_run_or_need_never_holds() {
    let mut f = fixture();
    let a = f.slice("n1", 2, &["src/a.rs"], StepState::Running);
    f.slice("n1", 1, &["src/b.rs"], StepState::Running);
    f.slice("n2", 2, &["src/b.rs"], StepState::Running);
    let got = f.resolve(2, &a, &["src/b.rs"], false);
    assert_eq!(got, Resolution::Widen(strings(&["src/b.rs"])));
}

#[test]
fn scope_rows_round_trip() {
    let mut f = fixture();
    let a = f.slice("n1", 1, &["src/a.rs"], StepState::Running);
    assert_eq!(f.store.bump_scope_requests(&a).expect("bump"), 1);
    assert_eq!(f.store.bump_scope_requests(&a).expect("bump"), 2);
    f.store
        .set_scope_state(&a, ScopeState::Repair)
        .expect("set");
    assert_eq!(
        f.store.scope_state(&a).expect("get"),
        Some(ScopeState::Repair)
    );
    assert_eq!(f.store.scope_state("nope").expect("get"), None);
    assert!(f.store.bump_scope_requests("nope").is_err());
}
