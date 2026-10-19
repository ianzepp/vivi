use super::*;
use crate::pump::state::State;

fn scan(name: &str, kind: Option<&str>, status: &str, cadence: Option<&str>) -> RoleScan {
    RoleScan {
        name: String::from(name),
        kind: kind.map(String::from),
        status: String::from(status),
        cadence: cadence.map(String::from),
        pool: None,
    }
}

fn run(ended_at: Option<u64>, retry_at: Option<u64>) -> RoleRun {
    RoleRun {
        task: String::from("task-1"),
        started_at: 9_000,
        ended_at,
        outcome: ended_at.map(|_| String::from("done")),
        attempt: 1,
        retry_at,
    }
}

#[test]
fn core_parse_role_rows_skips_prefix_noise() {
    let out = "scanning pool here at 2026-10-19\n\
        [{\"name\":\"head-1\",\"kind\":\"head\",\"status\":\"active\",\"cadence\":\"4h\",\"pool\":\"here\"},\n\
         {\"name\":\"hand-1\",\"kind\":\"hand\",\"status\":\"active\",\"cadence\":null,\"pool\":null}]\n";
    let rows = parse_role_rows(out);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].name, "head-1");
    assert_eq!(rows[0].kind.as_deref(), Some("head"));
    assert_eq!(rows[0].status, "active");
    assert_eq!(rows[0].cadence.as_deref(), Some("4h"));
    assert_eq!(rows[0].pool.as_deref(), Some("here"));
    assert_eq!(rows[1].name, "hand-1");
    assert_eq!(rows[1].kind, None);
    assert_eq!(rows[1].cadence, None);
    assert!(parse_role_rows("no json array anywhere").is_empty());
    assert!(parse_role_rows("").is_empty());
}

#[test]
fn core_parse_schedule_reads_state_and_age() {
    let out = "{\"role\":\"head-1\",\"pid\":42,\"schedule\":{\"state\":\"ok\",\
        \"cadence\":\"4h\",\"cadence_seconds\":14400,\"age_seconds\":3600}}";
    assert_eq!(parse_schedule(out), Some((String::from("ok"), Some(3_600))));

    let without_age = "{\"role\":\"head-2\",\"schedule\":{\"state\":\"never\",\"cadence\":\"4h\"}}";
    assert_eq!(
        parse_schedule(without_age),
        Some((String::from("never"), None))
    );

    assert_eq!(parse_schedule("not json at all"), None);
    assert_eq!(parse_schedule("{\"no_schedule\":true}"), None);
    assert_eq!(parse_schedule("{\"schedule\":{\"no_state\":1}}"), None);
}

#[test]
fn core_launchable_respects_status_inflight_and_backoff() {
    let now = 10_000;
    let live = scan("head-1", Some("head"), "active", Some("4h"));
    assert!(launchable(&live, None, now));
    assert!(launchable(&live, Some(&run(Some(now - 1), None)), now));

    for status in ["paused", "Parked", "RETIRED"] {
        let stopped = scan("head-1", Some("head"), status, Some("4h"));
        assert!(!launchable(&stopped, None, now), "status {status} must block");
    }

    assert!(!launchable(
        &scan("hand-1", Some("hand"), "active", Some("4h")),
        None,
        now
    ));
    assert!(!launchable(&scan("no-kind", None, "active", Some("4h")), None, now));
    assert!(!launchable(&scan("head-1", Some("head"), "active", None), None, now));

    // An open (still running) run blocks a new launch.
    assert!(!launchable(&live, Some(&run(None, None)), now));

    // A pending retry blocks until its retry_at passes.
    assert!(!launchable(&live, Some(&run(Some(now), Some(now + 60))), now));
    assert!(launchable(&live, Some(&run(Some(now), Some(now - 1))), now));
}

#[test]
fn core_state_round_trips_through_extra() {
    let mut st = State::default();
    assert_eq!(state(&st), CadenceState::default());

    let mut cadence = CadenceState::default();
    cadence.scanned_at = 5_000;
    cadence
        .scans
        .insert(String::from("head-1"), scan("head-1", Some("head"), "active", Some("4h")));
    cadence
        .runs
        .insert(String::from("head-1"), run(Some(4_900), None));
    set_state(&mut st, &cadence);
    assert!(st.extra.contains_key(EXTRA_KEY));

    let decoded = state(&st);
    assert_eq!(decoded, cadence);
    assert_eq!(decoded.scans["head-1"].cadence.as_deref(), Some("4h"));
    assert_eq!(decoded.runs["head-1"].ended_at, Some(4_900));

    // An unreadable payload decodes to the default, never an error.
    st.extra.insert(String::from(EXTRA_KEY), serde_json::json!(7));
    assert_eq!(state(&st), CadenceState::default());
}

#[test]
fn core_next_due() {
    let now = 10_000;
    assert_eq!(next_due(now, "due", 900, Some(1_000)), None);
    assert_eq!(next_due(now, "overdue", 900, Some(2_000)), None);
    assert_eq!(next_due(now, "never", 900, None), None);
    assert_eq!(next_due(now, "nonsense", 100, Some(50)), None);

    // ok: cadence 100 plus 10% grace = 110; age 50 → due 60s from now.
    assert_eq!(next_due(now, "ok", 100, Some(50)), Some(now + 60));
    // Exactly at the horizon, and beyond it: due now.
    assert_eq!(next_due(now, "ok", 100, Some(110)), None);
    assert_eq!(next_due(now, "ok", 100, Some(200)), None);
    // ok without an age cannot be placed on the schedule: due now.
    assert_eq!(next_due(now, "ok", 100, None), None);
}

#[test]
fn core_backoff_secs_doubles_and_caps() {
    assert_eq!(backoff_secs(0), 60);
    assert_eq!(backoff_secs(1), 60);
    assert_eq!(backoff_secs(2), 120);
    assert_eq!(backoff_secs(3), 240);
    assert_eq!(backoff_secs(6), 1_920);
    assert_eq!(backoff_secs(99), 1_920);
}
