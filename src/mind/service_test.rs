//! Tests for [`crate::mind::service`]: one wake tick end to end over temp
//! dirs, a fake probe, and the fake session.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tempfile::tempdir;

use super::*;
use crate::mind::actions::ActionRequest;
use crate::mind::config::{AuthorityStage, MindPaths, parse};
use crate::mind::pi::{FakePi, PiSession, TurnSummary};

const NOW: u64 = 5_000_000;

struct TestClock(Mutex<u64>);

impl TestClock {
    fn fixed(at: u64) -> Self {
        Self(Mutex::new(at))
    }

    fn advance(&self, secs: u64) {
        *self.0.lock().unwrap() += secs;
    }
}

impl Clock for TestClock {
    fn now(&self) -> u64 {
        *self.0.lock().unwrap()
    }
}

struct SlowPi {
    clock: Arc<TestClock>,
    secs: u64,
    reply: String,
    fail: bool,
}

impl PiSession for SlowPi {
    async fn prompt(
        &mut self,
        _message: String,
        _events: Option<tokio::sync::mpsc::UnboundedSender<crate::mind::pi::TurnEvent>>,
    ) -> Result<TurnSummary, crate::VivariumError> {
        self.clock.advance(self.secs);
        if self.fail {
            Err(crate::VivariumError::Other("turn failed".into()))
        } else {
            Ok(TurnSummary {
                text: self.reply.clone(),
            })
        }
    }

    async fn compact(&mut self) -> Result<String, crate::VivariumError> {
        Ok(String::new())
    }

    async fn history(&mut self, _limit: usize) -> Result<Vec<String>, crate::VivariumError> {
        Ok(Vec::new())
    }
}

struct FakeProbe(Vec<ProjectSignals>);

impl Probe for FakeProbe {
    fn probe(&self, root: &Path) -> ProjectSignals {
        self.0
            .iter()
            .find(|signal| signal.root == root)
            .cloned()
            .unwrap_or_else(|| ProjectSignals {
                root: root.to_path_buf(),
                verdict: "ok".into(),
                ..ProjectSignals::default()
            })
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    config: MindConfig,
    registry: PathBuf,
}

fn fixture(stage: &str) -> Fixture {
    let dir = tempdir().unwrap();
    let registry = dir.path().join("vivi").join("pump-service.toml");
    std::fs::create_dir_all(registry.parent().unwrap()).unwrap();
    std::fs::write(&registry, "[[project]]\nroot = \"/w/app\"\n").unwrap();
    let home = dir.path().join("mind");
    std::fs::create_dir_all(&home).unwrap();
    let paths = MindPaths::under(dir.path().join("mind.toml"), &home);
    let config = parse(&format!("authority = \"{stage}\""), paths).unwrap();
    Fixture {
        _dir: dir,
        config,
        registry,
    }
}

fn stalled() -> FakeProbe {
    FakeProbe(vec![ProjectSignals {
        root: "/w/app".into(),
        verdict: "stalled".into(),
        reasons: vec!["writer running but last tick 3h ago".into()],
        ..ProjectSignals::default()
    }])
}

const UNLOCK: &str = "Run is locked after a flaky gate.\n+++\n[[action]]\nkind = \"unlock\"\nproject = \"/w/app\"\nneed = \"3f9a1c2e\"\n+++";

async fn one_tick(
    fx: &Fixture,
    pi: FakePi,
    probe: &FakeProbe,
    now: u64,
) -> (TickOutcome, FakePi, DryRun) {
    let clock = TestClock::fixed(now);
    one_tick_with_clock(fx, pi, probe, &clock).await
}

async fn one_tick_with_clock<P: PiSession>(
    fx: &Fixture,
    pi: P,
    probe: &FakeProbe,
    clock: &TestClock,
) -> (TickOutcome, P, DryRun) {
    let shared = Shared::new(
        pi,
        fx.config.authority.as_str(),
        fx.config.paths.state.clone(),
    );
    let cx = TickContext {
        config: &fx.config,
        registry: &fx.registry,
        probe,
        clock,
    };
    let mut actuator = DryRun::default();
    let outcome = tick(&shared, &cx, &mut actuator).await.unwrap();
    (outcome, shared.pi.into_inner(), actuator)
}

#[tokio::test]
async fn a_successful_wake_ends_when_its_turn_returns() {
    let fx = fixture("unstick");
    let clock = Arc::new(TestClock::fixed(NOW));
    let pi = SlowPi {
        clock: Arc::clone(&clock),
        secs: 137,
        reply: UNLOCK.into(),
        fail: false,
    };
    let (outcome, _, _) = one_tick_with_clock(&fx, pi, &stalled(), &clock).await;
    assert!(matches!(outcome, TickOutcome::Woke { .. }));
    let state = store::load_at(&fx.config.paths.state).unwrap();
    let wake = state.last_wake.unwrap();
    assert_eq!(wake.started_at, NOW);
    assert_eq!(wake.ended_at, Some(NOW + 137));
    assert_eq!(wake.outcome.as_deref(), Some("success"));
    assert_eq!(state.recent_wakes, vec![NOW]);
    assert_eq!(state.ledger["3f9a1c2e"], vec![NOW + 137]);
}

#[tokio::test]
async fn a_failed_wake_ends_when_its_turn_returned_and_backs_off_from_there() {
    let fx = fixture("unstick");
    let clock = Arc::new(TestClock::fixed(NOW));
    let pi = SlowPi {
        clock: Arc::clone(&clock),
        secs: 137,
        reply: String::new(),
        fail: true,
    };
    let (outcome, _, _) = one_tick_with_clock(&fx, pi, &stalled(), &clock).await;
    assert!(matches!(outcome, TickOutcome::Failed(_)));
    let wake = store::load_at(&fx.config.paths.state)
        .unwrap()
        .last_wake
        .unwrap();
    assert_eq!(wake.started_at, NOW);
    assert_eq!(wake.ended_at, Some(NOW + 137));
    assert_eq!(wake.outcome.as_deref(), Some("failed"));
    assert_eq!(wake.retry_at, Some(NOW + 137 + 60));
}

#[tokio::test]
async fn a_stalled_pump_wakes_the_mind_and_unstick_dry_runs_the_unlock() {
    let fx = fixture("unstick");
    let (outcome, pi, actuator) =
        one_tick(&fx, FakePi::with_replies(&[UNLOCK]), &stalled(), NOW).await;
    let TickOutcome::Woke { planned } = outcome else {
        panic!("expected a wake, got {outcome:?}");
    };
    assert_eq!(planned.len(), 1);
    assert_eq!(planned[0].verdict, Ok(()));
    assert_eq!(
        actuator.applied,
        vec!["would run vivi pump unlock 3f9a1c2e --project /w/app"]
    );
    assert_eq!(pi.prompts.len(), 1);
    assert!(pi.prompts[0].contains("<<<untrusted source=\"pump-health\">"));
    let state = store::load_at(&fx.config.paths.state).unwrap();
    let wake = state.last_wake.unwrap();
    assert_eq!(wake.outcome.as_deref(), Some("success"));
    assert_eq!(state.ledger["3f9a1c2e"], vec![NOW]);
    assert!(state.pending_report.unwrap().contains("permitted"));
}

#[test]
fn an_attached_prompt_carries_a_fresh_probe_of_every_project() {
    let fx = fixture("observe");
    let text = attach_text(
        &fx.registry,
        &fx.config.paths.state,
        &stalled(),
        "how is app doing?",
    );
    assert!(
        text.contains("Project /w/app verdict stalled (STUCK)"),
        "{text}"
    );
    assert!(
        text.contains("<<<untrusted source=\"pump-health\">"),
        "{text}"
    );
    assert!(text.ends_with("Operator:\nhow is app doing?"), "{text}");
}

#[test]
fn an_attached_prompt_still_goes_when_the_projects_cannot_be_read() {
    let fx = fixture("observe");
    std::fs::write(&fx.registry, "[[project\nroot = ").unwrap();
    let text = attach_text(&fx.registry, &fx.config.paths.state, &stalled(), "hello");
    assert!(
        text.starts_with("(a fresh pump status could not be read"),
        "{text}"
    );
    assert!(text.ends_with("\nhello"), "{text}");
}

fn log_text(fx: &Fixture) -> String {
    std::fs::read_to_string(&fx.config.paths.log).unwrap_or_default()
}

#[tokio::test]
async fn the_mind_log_says_why_a_wake_began() {
    let fx = fixture("unstick");
    one_tick(
        &fx,
        FakePi::with_replies(&["Nothing to do."]),
        &FakeProbe(vec![]),
        NOW,
    )
    .await;
    let log = log_text(&fx);
    assert!(log.contains("wake begins: heartbeat ("), "{log}");
    assert!(!log.contains("signal"), "{log}");

    let fx = fixture("unstick");
    one_tick(&fx, FakePi::with_replies(&[UNLOCK]), &stalled(), NOW).await;
    let log = log_text(&fx);
    assert!(
        log.contains("wake begins: heartbeat (") && log.contains("; signal /w/app verdict=stalled"),
        "{log}"
    );
}

#[tokio::test]
async fn observe_refuses_the_same_unlock_and_reports_it_next_wake() {
    let fx = fixture("observe");
    let (outcome, _, actuator) =
        one_tick(&fx, FakePi::with_replies(&[UNLOCK]), &stalled(), NOW).await;
    let TickOutcome::Woke { planned } = outcome else {
        panic!("expected a wake");
    };
    assert!(planned[0].verdict.is_err());
    assert!(actuator.applied.is_empty());
    assert_eq!(fx.config.authority, AuthorityStage::Observe);
    let later = NOW + 4000;
    let (_, pi, _) = one_tick(&fx, FakePi::default(), &stalled(), later).await;
    assert!(
        pi.prompts[0].starts_with("Action results ("),
        "{}",
        pi.prompts[0]
    );
    assert!(
        pi.prompts[0].contains("\n- Unlock: refused"),
        "{}",
        pi.prompts[0]
    );
}

#[tokio::test]
async fn the_same_stall_does_not_wake_again_before_the_heartbeat() {
    let fx = fixture("observe");
    one_tick(&fx, FakePi::default(), &stalled(), NOW).await;
    let (outcome, pi, _) = one_tick(&fx, FakePi::default(), &stalled(), NOW + 60).await;
    assert!(
        matches!(outcome, TickOutcome::Slept("nothing due")),
        "{outcome:?}"
    );
    assert!(pi.prompts.is_empty());
}

#[tokio::test]
async fn a_paused_mind_does_not_wake() {
    let fx = fixture("unstick");
    store::update_at(&fx.config.paths.state, |state| {
        state.paused = true;
        Ok(())
    })
    .unwrap();
    let (outcome, pi, _) = one_tick(&fx, FakePi::default(), &stalled(), NOW).await;
    assert!(matches!(outcome, TickOutcome::Slept("paused")));
    assert!(pi.prompts.is_empty());
}

#[tokio::test]
async fn ignored_projects_are_not_probed_into_the_prompt() {
    let fx = fixture("observe");
    store::update_at(&fx.config.paths.state, |state| {
        state.ignored.insert("/w/app".into());
        Ok(())
    })
    .unwrap();
    let (outcome, pi, _) = one_tick(&fx, FakePi::default(), &stalled(), NOW).await;
    assert!(matches!(outcome, TickOutcome::Woke { .. }));
    assert!(!pi.prompts[0].contains("/w/app"));
}

#[test]
fn the_example_unit_is_loopback_only_and_names_the_run_verb() {
    let unit = crate::mind::commands::UNIT_EXAMPLE;
    assert!(unit.contains("ExecStart=/usr/local/bin/vivi mind service run"));
    assert!(unit.contains("IPAddressDeny=any"));
    assert!(unit.contains("IPAddressAllow=localhost"));
    assert!(unit.contains("NoNewPrivileges=yes"));
    assert!(unit.contains("User=vivi"));
}

#[test]
fn a_wake_left_open_by_a_dead_mind_is_closed_as_failed_on_start() {
    let dir = tempdir().unwrap();
    let state = dir.path().join("state.json");
    assert!(!close_interrupted_wake(&state, NOW).unwrap());
    store::update_at(&state, |state| {
        state.last_wake = Some(WakeRecord {
            started_at: NOW - 60,
            ..WakeRecord::default()
        });
        Ok(())
    })
    .unwrap();
    assert!(close_interrupted_wake(&state, NOW).unwrap());
    let closed = store::load_at(&state).unwrap().last_wake.unwrap();
    assert_eq!(closed.ended_at, Some(NOW));
    assert_eq!(closed.outcome.as_deref(), Some("failed"));
    assert!(closed.retry_at.is_some_and(|at| at > NOW));
    assert!(!close_interrupted_wake(&state, NOW).unwrap());
}

fn fixture_with_mcp(follow_ups: u32) -> Fixture {
    let dir = tempdir().unwrap();
    let registry = dir.path().join("vivi").join("pump-service.toml");
    std::fs::create_dir_all(registry.parent().unwrap()).unwrap();
    std::fs::write(&registry, "[[project]]\nroot = \"/w/app\"\n").unwrap();
    let home = dir.path().join("mind");
    std::fs::create_dir_all(&home).unwrap();
    let paths = MindPaths::under(dir.path().join("mind.toml"), &home);
    let config = parse(
        &format!(
            "authority = \"unstick\"\n[mcp]\nserver = \"http://127.0.0.1:1\"\nfollow_ups = {follow_ups}\n"
        ),
        paths,
    )
    .unwrap();
    Fixture {
        _dir: dir,
        config,
        registry,
    }
}

/// An actuator that answers like a live server: it runs the action and says
/// what it did.
#[derive(Default)]
struct Answering {
    ran: Vec<String>,
}

impl Actuator for Answering {
    fn apply(&mut self, request: &ActionRequest) -> Result<String, String> {
        let line = format!(
            "{:?} ok for {}",
            request.kind,
            request.need.as_deref().unwrap_or("-")
        );
        self.ran.push(line.clone());
        Ok(line)
    }

    fn feeds_back(&self) -> bool {
        true
    }
}

async fn tick_answering(fx: &Fixture, pi: FakePi, now: u64) -> (TickOutcome, FakePi, Answering) {
    let shared = Shared::new(pi, "unstick", fx.config.paths.state.clone());
    let clock = TestClock::fixed(now);
    let stalled = stalled();
    let cx = TickContext {
        config: &fx.config,
        registry: &fx.registry,
        probe: &stalled,
        clock: &clock,
    };
    let mut actuator = Answering::default();
    let outcome = tick(&shared, &cx, &mut actuator).await.unwrap();
    (outcome, shared.pi.into_inner(), actuator)
}

const REQUEUE: &str =
    "Checking.\n+++\n[[action]]\nkind = \"unlock\"\nproject = \"/w/app\"\nneed = \"4b8c0d1e\"\n+++";

#[tokio::test]
async fn the_session_sees_what_its_actions_answered_and_may_act_again() {
    let fx = fixture_with_mcp(2);
    let replies = [UNLOCK, REQUEUE, "Both done."];
    let (outcome, pi, actuator) = tick_answering(&fx, FakePi::with_replies(&replies), NOW).await;
    let TickOutcome::Woke { planned } = outcome else {
        panic!("expected a wake, got {outcome:?}");
    };
    assert_eq!(planned.len(), 2, "both rounds' actions are reported");
    assert_eq!(
        actuator.ran,
        ["Unlock ok for 3f9a1c2e", "Unlock ok for 4b8c0d1e"]
    );
    assert_eq!(pi.prompts.len(), 3, "the wake prompt and two follow-ups");
    assert!(
        pi.prompts[1].contains("Action results ("),
        "{}",
        pi.prompts[1]
    );
    assert!(
        pi.prompts[1].contains("<<<untrusted source=\"action-result\">"),
        "{}",
        pi.prompts[1]
    );
    assert!(pi.prompts[1].contains("Unlock ok for 3f9a1c2e"));
    let state = store::load_at(&fx.config.paths.state).unwrap();
    assert_eq!(state.last_wake.unwrap().outcome.as_deref(), Some("success"));
}

#[tokio::test]
async fn follow_ups_stop_at_the_configured_count() {
    let fx = fixture_with_mcp(1);
    let replies = [UNLOCK, REQUEUE, "never asked"];
    let (outcome, pi, actuator) = tick_answering(&fx, FakePi::with_replies(&replies), NOW).await;
    let TickOutcome::Woke { planned } = outcome else {
        panic!("expected a wake, got {outcome:?}");
    };
    assert_eq!(pi.prompts.len(), 2, "one follow-up, then the wake ends");
    assert_eq!(planned.len(), 2);
    assert_eq!(actuator.ran.len(), 2);
    let state = store::load_at(&fx.config.paths.state).unwrap();
    assert!(
        state.pending_report.unwrap().contains("Unlock: permitted"),
        "the last round's results wait for the next wake"
    );
}

#[tokio::test]
async fn a_reply_without_actions_ends_the_wake_without_a_follow_up() {
    let fx = fixture_with_mcp(3);
    let (_, pi, actuator) = tick_answering(&fx, FakePi::with_replies(&["All healthy."]), NOW).await;
    assert_eq!(pi.prompts.len(), 1);
    assert!(actuator.ran.is_empty());
}

#[tokio::test]
async fn a_failed_follow_up_turn_does_not_fail_a_wake_whose_actions_ran() {
    let fx = fixture_with_mcp(2);
    // The wake's one reply is used; the follow-up finds the fake exhausted.
    let (outcome, _, actuator) = tick_answering(&fx, FakePi::with_replies(&[UNLOCK]), NOW).await;
    assert!(matches!(outcome, TickOutcome::Woke { .. }), "{outcome:?}");
    assert_eq!(actuator.ran.len(), 1);
    let state = store::load_at(&fx.config.paths.state).unwrap();
    assert_eq!(state.last_wake.unwrap().outcome.as_deref(), Some("success"));
}
