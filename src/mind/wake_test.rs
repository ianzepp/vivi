//! Tests for [`crate::mind::wake`].

use std::path::PathBuf;

use super::*;

const NOW: u64 = 2_000_000;

fn finished(ended_at: u64, seen: &[&str]) -> WakeRecord {
    WakeRecord {
        started_at: ended_at.saturating_sub(30),
        ended_at: Some(ended_at),
        outcome: Some("success".into()),
        attempt: 0,
        retry_at: None,
        seen: seen.iter().map(|s| (*s).to_string()).collect(),
    }
}

fn inputs<'a>(last: Option<&'a WakeRecord>, signals: &'a [ProjectSignals]) -> WakeInputs<'a> {
    WakeInputs {
        now: NOW,
        heartbeat: "30m",
        last,
        recent: &[],
        max_wakes_per_hour: 6,
        paused: false,
        signals,
    }
}

fn stalled(root: &str) -> ProjectSignals {
    ProjectSignals {
        root: PathBuf::from(root),
        verdict: "stalled".into(),
        ..ProjectSignals::default()
    }
}

fn ok(root: &str) -> ProjectSignals {
    ProjectSignals {
        root: PathBuf::from(root),
        verdict: "ok".into(),
        ..ProjectSignals::default()
    }
}

fn reasons(decision: &WakeDecision) -> Vec<WakeReason> {
    match decision {
        WakeDecision::Wake { reasons, .. } => reasons.clone(),
        WakeDecision::Sleep(why) => panic!("expected wake, slept: {why}"),
    }
}

#[test]
fn first_start_wakes_on_heartbeat_never() {
    let decision = decide(&inputs(None, &[]));
    assert_eq!(
        reasons(&decision),
        vec![WakeReason::Heartbeat {
            state: "never".into()
        }]
    );
}

#[test]
fn recent_wake_sleeps_until_heartbeat_is_due() {
    let last = finished(NOW - 600, &[]);
    assert_eq!(
        decide(&inputs(Some(&last), &[ok("/a")])),
        WakeDecision::Sleep("nothing due")
    );
    let old = finished(NOW - 2000, &[]);
    assert_eq!(
        reasons(&decide(&inputs(Some(&old), &[]))),
        vec![WakeReason::Heartbeat {
            state: "due".into()
        }]
    );
    let ancient = finished(NOW - 4000, &[]);
    assert_eq!(
        reasons(&decide(&inputs(Some(&ancient), &[]))),
        vec![WakeReason::Heartbeat {
            state: "overdue".into()
        }]
    );
}

#[test]
fn a_new_alarm_wakes_between_heartbeats() {
    let last = finished(NOW - 60, &[]);
    let signals = [stalled("/a"), ok("/b")];
    let decision = decide(&inputs(Some(&last), &signals));
    assert_eq!(
        reasons(&decision),
        vec![WakeReason::Signal {
            fingerprint: signals[0].fingerprint()
        }]
    );
    let WakeDecision::Wake { seen, attempt, .. } = decision else {
        panic!("expected wake");
    };
    assert_eq!(seen, vec![signals[0].fingerprint()]);
    assert_eq!(attempt, 1);
}

#[test]
fn an_alarm_already_seen_waits_for_the_heartbeat() {
    let signals = [stalled("/a")];
    let print = signals[0].fingerprint();
    let last = finished(NOW - 60, &[&print]);
    assert_eq!(
        decide(&inputs(Some(&last), &signals)),
        WakeDecision::Sleep("nothing due")
    );
}

#[test]
fn every_signal_kind_alarms() {
    let base = ok("/a");
    let kinds = [
        ProjectSignals {
            verdict: "degraded".into(),
            ..base.clone()
        },
        ProjectSignals {
            locked_needs: 1,
            ..base.clone()
        },
        ProjectSignals {
            held_steps: 2,
            ..base.clone()
        },
        ProjectSignals {
            red_main: true,
            ..base.clone()
        },
        ProjectSignals {
            repair_alarm: true,
            ..base.clone()
        },
    ];
    assert!(!base.alarming());
    for signal in &kinds {
        assert!(signal.alarming(), "{signal:?}");
    }
    assert!(
        !ProjectSignals {
            verdict: "unknown".into(),
            ..base
        }
        .alarming()
    );
}

#[test]
fn in_flight_wake_blocks_the_next() {
    let mut last = finished(NOW - 5000, &[]);
    last.ended_at = None;
    assert_eq!(
        decide(&inputs(Some(&last), &[stalled("/a")])),
        WakeDecision::Sleep("wake in flight")
    );
}

#[test]
fn failed_wake_backs_off_with_cadence_backoff() {
    let mut record = WakeRecord {
        started_at: NOW - 100,
        attempt: 2,
        ..WakeRecord::default()
    };
    finish(&mut record, false, NOW - 10);
    assert_eq!(record.outcome.as_deref(), Some("failed"));
    assert_eq!(
        record.retry_at,
        Some(NOW - 10 + crate::pump::cadence::backoff_secs(2))
    );
    assert_eq!(
        decide(&inputs(Some(&record), &[stalled("/a")])),
        WakeDecision::Sleep("backing off after a failed wake")
    );
    let later = WakeInputs {
        now: NOW + 10_000,
        ..inputs(Some(&record), &[])
    };
    let WakeDecision::Wake { attempt, .. } = decide(&later) else {
        panic!("expected wake after backoff");
    };
    assert_eq!(attempt, 3);
}

#[test]
fn success_resets_attempts() {
    let mut record = WakeRecord {
        attempt: 4,
        retry_at: Some(1),
        ..WakeRecord::default()
    };
    finish(&mut record, true, NOW);
    assert_eq!(record.attempt, 0);
    assert_eq!(record.retry_at, None);
    assert_eq!(record.ended_at, Some(NOW));
}

#[test]
fn paused_and_capped_minds_sleep() {
    let paused = WakeInputs {
        paused: true,
        ..inputs(None, &[])
    };
    assert_eq!(decide(&paused), WakeDecision::Sleep("paused"));
    let recent = [NOW - 10, NOW - 20, NOW - 4000];
    let capped = WakeInputs {
        recent: &recent,
        max_wakes_per_hour: 2,
        ..inputs(None, &[])
    };
    assert_eq!(decide(&capped), WakeDecision::Sleep("wake cap reached"));
    let roomy = WakeInputs {
        max_wakes_per_hour: 3,
        ..capped
    };
    assert!(matches!(decide(&roomy), WakeDecision::Wake { .. }));
}
