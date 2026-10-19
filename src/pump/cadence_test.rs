//! Tests for [`crate::pump::cadence`].

use super::*;

fn scan(name: &str, kind: Option<&str>, status: &str, cadence: Option<&str>) -> RoleScan {
    RoleScan {
        name: name.to_string(),
        kind: kind.map(String::from),
        status: status.to_string(),
        cadence: cadence.map(String::from),
        pool: Some(String::from("default")),
    }
}

fn open_run() -> RoleRun {
    RoleRun {
        task: String::from("0123abcd"),
        started_at: 900,
        ended_at: None,
        outcome: Some(String::from("running")),
        attempt: 1,
        retry_at: None,
    }
}

#[test]
fn core_parse_role_rows_skips_prefix_noise() {
    let out = "note: scanning roles\n[\
        {\"name\":\"head-cto\",\"kind\":\"head\",\"status\":\"active\",\
         \"cadence\":\"3h\",\"pool\":\"default\",\"charter\":\"ignored\"},\
        {\"name\":\"hand-1\",\"kind\":\"hand\",\"status\":\"paused\"}]";
    let rows = parse_role_rows(out);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].name, "head-cto");
    assert_eq!(rows[0].kind.as_deref(), Some("head"));
    assert_eq!(rows[0].cadence.as_deref(), Some("3h"));
    assert_eq!(rows[0].pool.as_deref(), Some("default"));
    assert_eq!(rows[1].status, "paused");
    assert_eq!(rows[1].cadence, None);

    assert!(parse_role_rows("no json here").is_empty());
    assert!(parse_role_rows("[not json").is_empty());
    assert!(parse_role_rows("[]").is_empty());
}

#[test]
fn core_parse_schedule_reads_state_and_age() {
    let out = "{\"name\":\"head-cto\",\"schedule\":{\
        \"state\":\"due\",\"action_required\":false,\"age_seconds\":4200}}";
    assert_eq!(
        parse_schedule(out),
        Some((String::from("due"), Some(4200)))
    );

    let ok = "noise\n{\"schedule\":{\"state\":\"never\",\"age_seconds\":null}}";
    assert_eq!(parse_schedule(ok), Some((String::from("never"), None)));

    assert_eq!(parse_schedule("{}"), None);
    assert_eq!(parse_schedule("no json"), None);
    assert_eq!(parse_schedule("{\"schedule\":{\"codence\":\"3h\"}}"), None);
}

#[test]
fn core_launchable_respects_status_inflight_and_backoff() {
    let now = 1_000;
    let head = scan("head-cto", Some("head"), "active", Some("3h"));
    assert!(launchable(&head, None, now));

    // Cadence and kind are required.
    assert!(!launchable(
        &scan("head-cto", Some("head"), "active", None),
        None,
        now
    ));
    assert!(!launchable(
        &scan("head-cto", Some("head"), "active", Some("  ")),
        None,
        now
    ));
    assert!(!launchable(
        &scan("hand-1", Some("hand"), "active", Some("3h")),
        None,
        now
    ));
    assert!(!launchable(
        &scan("no-kind", None, "active", Some("3h")),
        None,
        now
    ));

    // Disabled statuses, case-insensitive.
    for status in ["paused", "PARKED", "retired"] {
        assert!(!launchable(
            &scan("head-cto", Some("head"), status, Some("3h")),
            None,
            now
        ));
    }

    // An open run blocks; a finished run with backoff pending blocks too.
    assert!(!launchable(&head, Some(&open_run()), now));
    let backed_off = RoleRun {
        ended_at: Some(950),
        outcome: Some(String::from("failed")),
        attempt: 2,
        retry_at: Some(1_100),
        ..open_run()
    };
    assert!(!launchable(&head, Some(&backed_off), now));
    let retry_passed = RoleRun {
        ended_at: Some(950),
        outcome: Some(String::from("done")),
        attempt: 1,
        retry_at: Some(1_000),
        ..open_run()
    };
    assert!(launchable(&head, Some(&retry_passed), now));

    // `scoper` is an auto-run kind even though it is not a head.
    assert!(launchable(
        &scan("scoper", Some("Scoper"), "active", Some("1h")),
        None,
        now
    ));
}

#[test]
fn core_state_round_trips_through_extra() {
    let mut st = State::default();
    assert_eq!(state(&st), CadenceState::default());

    let mut cadence = CadenceState::default();
    cadence.scanned_at = 1_000;
    cadence.scans.insert(
        String::from("head-cto"),
        scan("head-cto", Some("head"), "active", Some("3h")),
    );
    cadence.runs.insert(
        String::from("head-cto"),
        RoleRun {
            ended_at: Some(1_050),
            outcome: Some(String::from("done")),
            retry_at: Some(1_110),
            ..open_run()
        },
    );
    set_state(&mut st, &cadence);
    assert_eq!(state(&st), cadence);

    // Invalid stored values degrade to the default instead of panicking.
    st.extra
        .insert(String::from(EXTRA_KEY), serde_json::json!("garbage"));
    assert_eq!(state(&st), CadenceState::default());

    // A state file without the key reads as default.
    st.extra.remove(EXTRA_KEY);
    assert_eq!(state(&st), CadenceState::default());
}

#[test]
fn core_next_due() {
    let now = 1_000_000;
    // ok: last signal 50s ago, cadence 100s, grace 10s → 60s from now.
    assert_eq!(
        next_due(now, "ok", 100, Some(50)),
        Some(1_000_060),
        "ok is due one cadence plus grace after the last signal"
    );
    // case and whitespace tolerance
    assert_eq!(next_due(now, " OK ", 100, Some(50)), Some(1_000_060));
    // A stale ok is due now, never in the past.
    assert_eq!(next_due(now, "ok", 100, Some(500)), Some(now));
    // Missing age cannot be scheduled: treat as due now.
    assert_eq!(next_due(now, "ok", 100, None), None);

    for state in ["due", "overdue", "never", "none"] {
        assert_eq!(next_due(now, state, 100, Some(10)), None, "{state}");
    }
}

#[test]
fn core_backoff_doubles_and_caps() {
    assert_eq!(backoff_secs(0), BACKOFF_BASE);
    assert_eq!(backoff_secs(1), BACKOFF_BASE);
    assert_eq!(backoff_secs(2), BACKOFF_BASE * 2);
    assert_eq!(backoff_secs(3), BACKOFF_BASE * 4);
    assert_eq!(backoff_secs(6), BACKOFF_BASE * 32);
    assert_eq!(backoff_secs(99), BACKOFF_BASE * 32);
}
