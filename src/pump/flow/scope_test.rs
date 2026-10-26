//! Tests for blocked-scope resolution.

use tempfile::{TempDir, tempdir};

use super::{
    BlockedScope, Resolution, ScopeState, finish_grant, grant_held, resolve, wait_holders,
};
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::index::{StepRow, StepState};
use crate::pump::flow::mail::MailWriter;
use crate::pump::store::{DB_FILENAME, PumpStore};

struct Fixture {
    dir: TempDir,
    store: PumpStore,
    mail: FakeMail,
    requests: u32,
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

fn fixture() -> Fixture {
    let dir = tempdir().expect("tempdir");
    let store = PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open");
    Fixture {
        dir,
        store,
        mail: FakeMail::new("mind"),
        requests: 0,
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

    /// Drop and reopen the store, as a restarted pump would.
    fn reopen(&mut self) {
        self.store = PumpStore::open(&self.dir.path().join(DB_FILENAME)).expect("reopen");
    }

    /// A genuine request: every call carries its own originating source job.
    fn resolve(&mut self, run: u32, task: &str, paths: &[&str], design: bool) -> Resolution {
        self.requests += 1;
        let source = format!("{task}-scope-{}", self.requests);
        self.resolve_as(&source, run, task, paths, design)
    }

    /// Resolve under an explicit source job, so a test can replay one.
    fn resolve_as(
        &mut self,
        source: &str,
        run: u32,
        task: &str,
        paths: &[&str],
        design: bool,
    ) -> Resolution {
        let req = BlockedScope {
            paths: strings(paths),
            reason: "needs it".into(),
            design,
        };
        resolve(&self.store, &mut self.mail, run, task, source, &req).expect("resolve")
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
fn a_second_request_goes_to_rework() {
    let mut f = fixture();
    let a = f.slice("n1", 1, &["src/a.rs"], StepState::Running);
    assert!(matches!(
        f.resolve(1, &a, &["src/b.rs"], false),
        Resolution::Widen(_)
    ));
    assert!(matches!(
        f.resolve(1, &a, &["src/c.rs"], false),
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
        f.resolve(1, &a, &six, false),
        Resolution::Rework(_)
    ));
    let b = f.slice("n1", 1, &["src/b.rs"], StepState::Running);
    assert!(matches!(
        f.resolve(1, &b, &six[..5], false),
        Resolution::Widen(_)
    ));
}

#[test]
fn the_design_flag_goes_to_rework() {
    let mut f = fixture();
    let a = f.slice("n1", 1, &["src/a.rs"], StepState::Running);
    assert!(matches!(
        f.resolve(1, &a, &["src/b.rs"], true),
        Resolution::Rework(_)
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
    f.resolve(1, &a, &["src/b.rs", "src/c.rs"], false);

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
        f.resolve(1, &a, &["src/d.rs"], false),
        Resolution::Rework(_)
    ));
}

#[test]
fn a_directory_request_enclosing_a_held_file_waits() {
    let mut f = fixture();
    let a = f.slice("n1", 1, &["src/a.rs"], StepState::Running);
    let b = f.slice("n1", 1, &["src/b.rs"], StepState::Running);
    let got = f.resolve(1, &a, &["src/", "docs/x.md"], false);
    assert_eq!(got, Resolution::WaitOn(vec![b.clone()]));
    assert_eq!(f.mail.tasks[&a].depends_on, vec![b.clone()]);
    assert_eq!(wait_holders(&f.store, &a).expect("holders"), vec![b]);
    // The free path widened right away; the enclosed one is granted later.
    assert_eq!(
        f.store.slice_scope(&a).expect("scope"),
        Some(strings(&["src/a.rs", "docs/x.md"]))
    );
}

#[test]
fn nested_directories_overlap_in_both_directions() {
    // Holder has the ancestor: it covers the requested descendant.
    let mut f = fixture();
    let a = f.slice("n1", 1, &["src/a.rs"], StepState::Running);
    let b = f.slice("n1", 1, &["src/"], StepState::Running);
    assert_eq!(
        f.resolve(1, &a, &["src/pump/"], false),
        Resolution::WaitOn(vec![b])
    );

    // Holder has the descendant: the requested ancestor encloses it.
    let mut f = fixture();
    let a = f.slice("n1", 1, &["src/a.rs"], StepState::Running);
    let b = f.slice("n1", 1, &["src/pump/"], StepState::Running);
    assert_eq!(
        f.resolve(1, &a, &["src/"], false),
        Resolution::WaitOn(vec![b])
    );

    // Equal directory declarations hold too.
    let mut f = fixture();
    let a = f.slice("n1", 1, &["src/a.rs"], StepState::Running);
    let b = f.slice("n1", 1, &["src/pump/"], StepState::Running);
    assert_eq!(
        f.resolve(1, &a, &["src/pump/"], false),
        Resolution::WaitOn(vec![b])
    );
}

#[test]
fn leading_dots_and_root_spellings_normalize_before_overlap() {
    // A holder declaration spelled `./src/` holds a plainly spelled file.
    let mut f = fixture();
    let a = f.slice("n1", 1, &["src/a.rs"], StepState::Running);
    let b = f.slice("n1", 1, &["./src/"], StepState::Running);
    assert_eq!(
        f.resolve(1, &a, &["./src/run.rs"], false),
        Resolution::WaitOn(vec![b])
    );

    // A root request encloses every held path, so nothing stays free.
    let mut f = fixture();
    let a = f.slice("n1", 1, &["src/a.rs"], StepState::Running);
    let b = f.slice("n1", 1, &["src/b.rs"], StepState::Running);
    let c = f.slice("n1", 1, &["docs/"], StepState::Pending);
    assert_eq!(
        f.resolve(1, &a, &["./"], false),
        Resolution::WaitOn(vec![b, c])
    );
    assert_eq!(
        f.store.slice_scope(&a).expect("scope"),
        Some(strings(&["src/a.rs"]))
    );
}

#[test]
fn sibling_prefixes_never_overlap() {
    // A holder of `src-old/` does not hold a `src/` request.
    let mut f = fixture();
    let a = f.slice("n1", 1, &["src-old/a.rs"], StepState::Running);
    f.slice("n1", 1, &["src-old/"], StepState::Running);
    assert_eq!(
        f.resolve(1, &a, &["src/b.rs"], false),
        Resolution::Widen(strings(&["src/b.rs"]))
    );

    // ...and a holder of `src/` does not hold a `src-old/` request.
    let mut f = fixture();
    let a = f.slice("n1", 1, &["src/a.rs"], StepState::Running);
    f.slice("n1", 1, &["src/"], StepState::Running);
    assert_eq!(
        f.resolve(1, &a, &["src-old/b.rs"], false),
        Resolution::Widen(strings(&["src-old/b.rs"]))
    );
}

#[test]
fn a_file_request_does_not_enclose_a_held_file() {
    let mut f = fixture();
    let a = f.slice("n1", 1, &["src/a.rs"], StepState::Running);
    f.slice("n1", 1, &["src/pump/run.rs"], StepState::Running);
    assert_eq!(
        f.resolve(1, &a, &["src/pump"], false),
        Resolution::Widen(strings(&["src/pump"]))
    );
}

#[test]
fn one_holder_with_several_held_paths_appears_once() {
    let mut f = fixture();
    let a = f.slice("n1", 1, &["src/a.rs"], StepState::Running);
    let b = f.slice("n1", 1, &["src/pump/"], StepState::Running);
    assert_eq!(
        f.resolve(1, &a, &["src/pump/run.rs", "src/pump/mod.rs"], false),
        Resolution::WaitOn(vec![b.clone()])
    );
    assert_eq!(wait_holders(&f.store, &a).expect("holders"), vec![b]);
}

#[test]
fn replayed_hand_request_does_not_increment_request_count() {
    let mut f = fixture();
    let a = f.slice("n1", 1, &["src/a.rs"], StepState::Running);
    let b = f.slice("n1", 1, &["src/pump/"], StepState::Running);
    // The request encloses the held directory entry: src/ covers src/pump/.
    let source = format!("{a}-scope-1");
    let first = f.resolve_as(&source, 1, &a, &["src/", "docs/x.md"], false);
    assert_eq!(first, Resolution::WaitOn(vec![b.clone()]));
    assert_eq!(f.store.scope_requests(&a).expect("count"), Some(1));

    // The world moves on: the holder closes, so a recomputation would widen.
    f.store
        .set_step_state(&b, StepState::Done, None)
        .expect("close holder");

    // The same hand request again: the stored resolution returns untouched.
    let again = f.resolve_as(&source, 1, &a, &["src/", "docs/x.md"], false);
    assert_eq!(again, Resolution::WaitOn(vec![b.clone()]));
    assert_eq!(f.store.scope_requests(&a).expect("count"), Some(1));
    assert_eq!(
        f.store.scope_state(&a).expect("state"),
        Some(ScopeState::Wait)
    );
    assert_eq!(
        f.store.slice_scope(&a).expect("scope"),
        Some(strings(&["src/a.rs", "docs/x.md"]))
    );
    assert_eq!(wait_holders(&f.store, &a).expect("holders"), vec![b]);
}

#[test]
fn a_replayed_request_survives_a_store_reopen() {
    let mut f = fixture();
    let a = f.slice("n1", 1, &["src/a.rs"], StepState::Running);
    let b = f.slice("n1", 1, &["src/b.rs"], StepState::Running);
    let source = format!("{a}-scope-1");
    let first = f.resolve_as(&source, 1, &a, &["src/b.rs"], false);
    assert_eq!(first, Resolution::WaitOn(vec![b.clone()]));
    let count = f.store.scope_requests(&a).expect("count");

    f.reopen();
    let again = f.resolve_as(&source, 1, &a, &["src/b.rs"], false);
    assert_eq!(again, Resolution::WaitOn(vec![b]));
    assert_eq!(f.store.scope_requests(&a).expect("count"), count);
    assert_eq!(
        f.store.scope_state(&a).expect("state"),
        Some(ScopeState::Wait)
    );
}

#[test]
fn a_replayed_wait_rewrites_each_holder_edge_once() {
    let mut f = fixture();
    let a = f.slice("n1", 1, &["src/a.rs"], StepState::Running);
    let b = f.slice("n1", 1, &["src/b.rs"], StepState::Running);
    let c = f.slice("n1", 1, &["docs/"], StepState::Running);
    let source = format!("{a}-scope-1");
    let first = f.resolve_as(&source, 1, &a, &["src/b.rs", "docs/x.md"], false);
    assert_eq!(first, Resolution::WaitOn(vec![b.clone(), c.clone()]));

    // The dependency-write retry of a replay leaves every edge unique.
    f.resolve_as(&source, 1, &a, &["src/b.rs", "docs/x.md"], false);
    f.resolve_as(&source, 1, &a, &["src/b.rs", "docs/x.md"], false);
    assert_eq!(f.mail.tasks[&a].depends_on, vec![b, c]);
    assert_eq!(f.store.scope_requests(&a).expect("count"), Some(1));
}
