//! One test per Phase 2 pass criterion in
//! `docs/orchestration-reset/README.md`, plus the park path. Each test
//! starts its own Temporal dev server through the installed `temporal` CLI.

mod common;

use std::collections::{BTreeSet, HashMap};

use common::{Harness, QUEUE, need_input, tuning};
use temporalio_client::WorkflowGetResultOptions;
use temporalio_client::WorkflowStartOptions;
use temporalio_client::errors::WorkflowStartError;
use vivi_temporal::fault::{FaultKind, NeedScript, SliceScript};
use vivi_temporal::need::NeedWorkflow;
use vivi_temporal::slice::{SliceWorkflow, slice_branch};
use vivi_temporal::types::{
    AdmissionPolicy, NeedOutcome, ParkCause, RecordCall, Routing, SliceInput, SliceSpec,
};
use vivi_temporal::world::{Event, ImplementStart};
use vivi_temporal::{need_start_options, slice_workflow_id};

fn slice(id: &str) -> SliceScript {
    SliceScript::new(id)
}

fn starts(events: &[Event], slice: &str) -> Vec<ImplementStart> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::ImplementStarted(s) if s.slice == slice => Some(s.clone()),
            _ => None,
        })
        .collect()
}

fn count(events: &[Event], pred: impl Fn(&Event) -> bool) -> usize {
    events.iter().filter(|e| pred(e)).count()
}

fn commits(slice: &str, rounds: &[u32]) -> Vec<String> {
    rounds
        .iter()
        .flat_map(|r| (1..=3).map(move |s| format!("{slice}-r{r}-s{s}")))
        .collect()
}

fn landed(ids: &[&str]) -> NeedOutcome {
    NeedOutcome::Closed {
        landed: ids.iter().map(|s| s.to_string()).collect(),
    }
}

/// Criterion: a 429 never spends an attempt. Throttles on plan, implement
/// and review are waited out for the Retry-After, never retried by Temporal
/// (every implement start is attempt 1), never reach check or review as a
/// failure, and never spend a work round (the slice lands in round 1).
#[tokio::test(flavor = "multi_thread")]
async fn provider_429_never_spends_an_attempt() {
    let script = NeedScript::new(vec![
        slice("u1")
            .implement(&["quota@1", "quota@2"])
            .unwrap()
            .review(&["quota@1"])
            .unwrap(),
    ])
    .plan(&["quota@1"])
    .unwrap();
    let h = Harness::start(vec![("n1", script)]).await;
    let outcome = h
        .with_worker(async {
            h.start_lander(100).await;
            h.run_need("n1", tuning()).await
        })
        .await;
    assert_eq!(outcome, landed(&["u1"]));

    let events = h.events();
    let throttles: Vec<(String, std::time::Duration)> = events
        .iter()
        .filter_map(|e| match e {
            Event::Throttled { step, at, .. } => Some((step.clone(), *at)),
            _ => None,
        })
        .collect();
    let steps: Vec<&str> = throttles.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(steps, vec!["plan", "implement", "implement", "review"]);

    let started = starts(&events, "u1");
    assert_eq!(started.len(), 1, "one implement run: {started:?}");
    assert_eq!((started[0].round, started[0].attempt), (1, 1));
    let last_throttle = throttles[2].1;
    assert!(
        started[0].at - last_throttle >= common::timing().retry_after,
        "implement ran again before the Retry-After"
    );
    assert_eq!(count(&events, |e| matches!(e, Event::Checked { .. })), 1);
    assert_eq!(count(&events, |e| matches!(e, Event::Reviewed { .. })), 1);
    h.shutdown().await;
}

/// Criterion: a timeout resumes from the pushed commit. `u1` hits the
/// agent cap while still heartbeating; `u2`'s host goes dark and misses
/// heartbeats. Each is retried by Temporal (attempt 2) and starts from the
/// commit in its last heartbeat, so no step is redone and no round spent.
#[tokio::test(flavor = "multi_thread")]
async fn timeout_resumes_from_pushed_commit() {
    let script = NeedScript::new(vec![
        slice("u1").implement(&["timeout@1"]).unwrap(),
        slice("u2").implement(&["lost@1"]).unwrap(),
    ]);
    let h = Harness::start(vec![("n1", script)]).await;
    let outcome = h
        .with_worker(async {
            h.start_lander(100).await;
            h.run_need("n1", tuning()).await
        })
        .await;
    assert_eq!(outcome, landed(&["u1", "u2"]));

    let events = h.events();
    for (id, kind) in [("u1", FaultKind::Timeout), ("u2", FaultKind::Lost)] {
        let dark = Event::WentDark {
            slice: id.into(),
            kind,
        };
        assert!(events.contains(&dark), "{id} never went dark");
        let started = starts(&events, id);
        let shape: Vec<(u32, u32, Option<String>)> = started
            .iter()
            .map(|s| (s.round, s.attempt, s.resumed_from.clone()))
            .collect();
        let pushed = format!("{id}-r1-s1");
        assert_eq!(shape, vec![(1, 1, None), (1, 2, Some(pushed))], "{id}");
        let branch = h.world.branch(&slice_branch("n1", id)).unwrap();
        assert_eq!(branch, commits(id, &[1]), "{id} redid or lost a step");
    }
    h.shutdown().await;
}

/// Criterion: a duplicate start is refused, both while the need runs and
/// after it has closed. The plan runs once.
#[tokio::test(flavor = "multi_thread")]
async fn duplicate_start_is_refused() {
    let script = NeedScript::new(vec![slice("u1")]);
    let h = Harness::start(vec![("n1", script)]).await;
    h.with_worker(async {
        h.start_lander(100).await;
        let start = || {
            h.client.start_workflow(
                NeedWorkflow::run,
                need_input("n1", tuning()),
                need_start_options(QUEUE, "n1"),
            )
        };
        let first = start().await.expect("first start");
        let running = start().await;
        assert_refused(running);
        let outcome = first
            .get_result(WorkflowGetResultOptions::default())
            .await
            .unwrap();
        assert_eq!(outcome, landed(&["u1"]));
        let closed = start().await;
        assert_refused(closed);
    })
    .await;
    let planned = count(&h.events(), |e| matches!(e, Event::Planned { .. }));
    assert_eq!(planned, 1);
    h.shutdown().await;
}

/// Criterion: a red check goes back to implement on the same branch. The
/// second round gets the failure text as feedback, starts from the first
/// round's head, and adds its commits to the same branch. A review change
/// request takes the same path.
#[tokio::test(flavor = "multi_thread")]
async fn red_check_returns_to_implement_on_same_branch() {
    let script = NeedScript::new(vec![
        slice("u1").check(&["red@1"]).unwrap(),
        slice("u2").review(&["changes@1"]).unwrap(),
    ]);
    let h = Harness::start(vec![("n1", script)]).await;
    let outcome = h
        .with_worker(async {
            h.start_lander(100).await;
            h.run_need("n1", tuning()).await
        })
        .await;
    assert_eq!(outcome, landed(&["u1", "u2"]));

    let events = h.events();
    for (id, said) in [
        ("u1", "check red at u1-r1-s3"),
        ("u2", "review of u2-r1-s3"),
    ] {
        let started = starts(&events, id);
        assert_eq!(started.len(), 2, "{id}: {started:?}");
        let branch = slice_branch("n1", id);
        assert!(
            started.iter().all(|s| s.branch == branch),
            "{id} changed branch"
        );
        assert_eq!(
            (started[0].feedback.as_deref(), started[0].round),
            (None, 1)
        );
        let feedback = started[1].feedback.clone().unwrap_or_default();
        assert!(feedback.starts_with(said), "{id} feedback: {feedback:?}");
        assert_eq!(started[1].round, 2);
        assert_eq!(
            started[1].resumed_from.as_deref(),
            Some(format!("{id}-r1-s3").as_str())
        );
        assert_eq!(h.world.branch(&branch).unwrap(), commits(id, &[1, 2]));
    }
    h.shutdown().await;
}

/// Criterion: no slice runs twice at once. Slices that are retried by
/// Temporal, waited out, or looped never overlap their own work, and a
/// second start of a running slice's workflow id is refused.
#[tokio::test(flavor = "multi_thread")]
async fn no_slice_runs_twice_at_once() {
    let script = NeedScript::new(vec![
        slice("u1").implement(&["lost@1"]).unwrap(),
        slice("u2").implement(&["timeout@1"]).unwrap(),
        slice("u3").check(&["red@1"]).unwrap(),
        slice("u4").implement(&["quota@1"]).unwrap(),
    ]);
    let h = Harness::start(vec![("n1", script)]).await;
    let outcome = h
        .with_worker(async {
            h.start_lander(100).await;
            let need = h
                .client
                .start_workflow(
                    NeedWorkflow::run,
                    need_input("n1", tuning()),
                    need_start_options(QUEUE, "n1"),
                )
                .await
                .unwrap();
            wait_until(|| !starts(&h.events(), "u1").is_empty()).await;
            let dup = SliceInput {
                need: "n1".into(),
                slice: SliceSpec {
                    id: "u1".into(),
                    ..SliceSpec::default()
                },
                lander_id: common::LANDER.into(),
                tuning: tuning(),
                routing: Routing::single(QUEUE),
                target: common::TARGET.into(),
                branch_prefix: "vivi/".into(),
            };
            let opts = WorkflowStartOptions::new(QUEUE, slice_workflow_id("n1", "u1")).build();
            let second = h.client.start_workflow(SliceWorkflow::run, dup, opts).await;
            assert_refused(second);
            need.get_result(WorkflowGetResultOptions::default())
                .await
                .unwrap()
        })
        .await;
    assert_eq!(outcome, landed(&["u1", "u2", "u3", "u4"]));

    let max: HashMap<String, u32> = h.world.max_concurrency().unwrap();
    for id in ["u1", "u2", "u3", "u4"] {
        assert_eq!(max.get(id), Some(&1), "{id} overlapped itself: {max:?}");
    }
    h.shutdown().await;
}

/// Criterion: a Lander bisection ejects exactly the bad units. Six slices
/// queue together; two break the gate. Bisection merges the four good ones
/// and ejects only the two bad ones, which go back to implement with the
/// gate's text until their rounds run out and the need parks.
#[tokio::test(flavor = "multi_thread")]
async fn lander_bisection_ejects_exactly_bad_units() {
    let ids = ["u1", "u2", "u3", "u4", "u5", "u6"];
    let bad = ["u2", "u5"];
    let slices = ids
        .iter()
        .map(|id| {
            if bad.contains(id) {
                slice(id).bad()
            } else {
                slice(id)
            }
        })
        .collect();
    let h = Harness::start(vec![("n1", NeedScript::new(slices))]).await;
    let mut t = tuning();
    t.max_work_rounds = 2;
    let outcome = h
        .with_worker(async {
            h.start_lander(1_500).await;
            h.run_need("n1", t).await
        })
        .await;

    let NeedOutcome::Parked {
        slice: parked,
        cause,
    } = outcome
    else {
        panic!("need should park on the bad slices: {outcome:?}");
    };
    assert_eq!(parked, "u2");
    let ParkCause::WorkExhausted {
        rounds: 2,
        last_failure,
    } = cause
    else {
        panic!("wrong cause: {cause:?}");
    };
    assert!(last_failure.starts_with("gate red"), "{last_failure}");

    let main: BTreeSet<String> = h.world.main().unwrap().into_iter().collect();
    let good: BTreeSet<String> = ["u1", "u3", "u4", "u6"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(main, good);
    assert_eq!(h.world.main().unwrap().len(), 4, "a unit merged twice");

    let gates: Vec<(Vec<String>, bool)> = h
        .events()
        .into_iter()
        .filter_map(|e| match e {
            Event::Gate { batch, pass } => Some((batch, pass)),
            _ => None,
        })
        .collect();
    let ejected: BTreeSet<&str> = gates
        .iter()
        .filter(|(b, pass)| !pass && b.len() == 1)
        .map(|(b, _)| b[0].as_str())
        .collect();
    assert_eq!(
        ejected,
        bad.into_iter().collect::<BTreeSet<_>>(),
        "gates: {gates:?}"
    );
    assert!(
        gates.iter().any(|(b, pass)| !pass && b.len() > 1),
        "no batch was bisected: {gates:?}"
    );
    h.shutdown().await;
}

/// Exhausted rounds or attempts park the need with a typed cause: work
/// (two red checks) and infra (three lost hosts). Nothing restarts at plan.
#[tokio::test(flavor = "multi_thread")]
async fn exhausted_attempts_park_with_cause() {
    let work = NeedScript::new(vec![slice("u1").check(&["red@1", "red@2"]).unwrap()]);
    let infra = NeedScript::new(vec![
        slice("v1")
            .implement(&["lost@1", "lost@2", "lost@3"])
            .unwrap(),
    ]);
    let h = Harness::start(vec![("n1", work), ("n2", infra)]).await;
    let mut t = tuning();
    t.max_work_rounds = 2;
    let (n1, n2) = h
        .with_worker(async {
            // Slices ask the Lander for admission before they start.
            h.start_lander(100).await;
            tokio::join!(h.run_need("n1", t.clone()), h.run_need("n2", t.clone()))
        })
        .await;

    let NeedOutcome::Parked {
        cause: ParkCause::WorkExhausted {
            rounds,
            last_failure,
        },
        ..
    } = n1
    else {
        panic!("n1 should park on work: {n1:?}");
    };
    assert_eq!(rounds, 2);
    assert!(
        last_failure.starts_with("check red at u1-r2-s3"),
        "{last_failure}"
    );
    let NeedOutcome::Parked {
        slice,
        cause: ParkCause::InfraExhausted { step, .. },
    } = n2
    else {
        panic!("n2 should park on infra: {n2:?}");
    };
    assert_eq!((slice.as_str(), step.as_str()), ("v1", "implement"));

    for need in ["n1", "n2"] {
        let calls = h.record.calls_for(need).unwrap();
        assert!(
            matches!(calls.last(), Some(RecordCall::Park { .. })),
            "{need}: {calls:?}"
        );
    }
    assert_eq!(
        count(&h.events(), |e| matches!(e, Event::Planned { .. })),
        2
    );
    h.shutdown().await;
}

fn assert_refused<T>(start: Result<T, WorkflowStartError>) {
    match start {
        Err(WorkflowStartError::AlreadyStarted { .. }) => {}
        Err(other) => panic!("refused for the wrong reason: {other:?}"),
        Ok(_) => panic!("a duplicate start was accepted"),
    }
}

async fn wait_until(cond: impl Fn() -> bool) {
    for _ in 0..200 {
        if cond() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("condition not met within 5s");
}

/// Scheduling rule: the hot-file mutex. `u1` and `u2` both write under the
/// hot path `crates/hot`, so `u2` does not start implementing until `u1`
/// has landed and released its lease. `u3` writes elsewhere and is not
/// held back.
#[tokio::test(flavor = "multi_thread")]
async fn hot_file_mutex_serializes_overlapping_slices() {
    let script = NeedScript::new(vec![
        slice("u1").scope(&["crates/hot/src/a.rs"]),
        slice("u2").scope(&["crates/hot/src/b.rs"]),
        slice("u3").scope(&["crates/cold/src/c.rs"]),
    ]);
    let h = Harness::start(vec![("n1", script)]).await;
    let policy = AdmissionPolicy {
        hot_paths: vec!["crates/hot".into()],
        max_backlog_ms: 0,
        land_ms_per_unit: 0,
    };
    let outcome = h
        .with_worker(async {
            h.start_lander_with(100, policy).await;
            h.run_need("n1", tuning()).await
        })
        .await;
    assert_eq!(outcome, landed(&["u1", "u2", "u3"]));

    let events = h.events();
    let first_start = |id: &str| {
        events
            .iter()
            .position(|e| matches!(e, Event::ImplementStarted(s) if s.slice == id))
            .unwrap()
    };
    let merged = |id: &str| {
        events
            .iter()
            .position(|e| matches!(e, Event::Merged { batch } if batch.iter().any(|s| s == id)))
            .unwrap()
    };
    let (a, b) = (first_start("u1"), first_start("u2"));
    let (first, second) = if a < b { ("u1", "u2") } else { ("u2", "u1") };
    assert!(
        first_start(second) > merged(first),
        "{second} started before {first} landed: {events:?}"
    );
    assert!(first_start("u3") < merged(first), "u3 was held back");
    h.shutdown().await;
}
