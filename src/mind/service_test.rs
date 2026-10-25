//! Tests for [`crate::mind::service`]: one wake tick end to end over temp
//! dirs, a fake probe, and the fake session.

use std::path::{Path, PathBuf};

use tempfile::tempdir;

use super::*;
use crate::mind::config::{AuthorityStage, MindPaths, parse};
use crate::mind::pi::FakePi;

const NOW: u64 = 5_000_000;

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
    let shared = Shared::new(
        pi,
        fx.config.authority.as_str(),
        fx.config.paths.state.clone(),
    );
    let cx = TickContext {
        config: &fx.config,
        registry: &fx.registry,
        probe,
        now,
    };
    let mut actuator = DryRun::default();
    let outcome = tick(&shared, &cx, &mut actuator).await.unwrap();
    (outcome, shared.pi.into_inner(), actuator)
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
    assert!(pi.prompts[0].starts_with("Action results:\n- Unlock: refused"));
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
