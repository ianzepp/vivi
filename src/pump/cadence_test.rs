use super::*;

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
        task: String::from("t"),
        started_at: 1_000,
        ended_at,
        outcome: Some(String::from(if ended_at.is_some() {
            "done"
        } else {
            "running"
        })),
        attempt: 1,
        retry_at,
    }
}

#[test]
fn core_state_round_trips_through_extra() {
    let empty = State::default();
    assert_eq!(state(&empty), CadenceState::default());

    let mut cadence = CadenceState {
        scanned_at: 5_000,
        ..CadenceState::default()
    };
    cadence.scans.insert(
        String::from("head-1"),
        scan("head-1", Some("head"), "active", Some("4h")),
    );
    cadence
        .runs
        .insert(String::from("head-1"), run(Some(4_900), None));

    let mut st = State::default();
    set_state(&mut st, &cadence);
    assert!(st.extra.contains_key(EXTRA_KEY));
    assert_eq!(state(&st), cadence);

    st.extra
        .insert(String::from(EXTRA_KEY), serde_json::json!(7));
    assert_eq!(state(&st), CadenceState::default());
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

#[test]
fn core_parse_role_rows_skips_prefix_noise() {
    let out = concat!(
        "pump log noise\n",
        r#"[{"name":"head-1","kind":"head","status":"active","#,
        r#""cadence":"4h","pool":"fast"},{"name":"mind","status":"active"}]"#,
    );
    let rows = parse_role_rows(out);
    assert_eq!(rows.len(), 2);
    let mut expected = scan("head-1", Some("head"), "active", Some("4h"));
    expected.pool = Some(String::from("fast"));
    assert_eq!(rows[0], expected);
    assert_eq!(rows[0].pool.as_deref(), Some("fast"));
    assert_eq!(rows[1], scan("mind", None, "active", None));
    assert!(parse_role_rows("no json").is_empty());
}

#[test]
fn core_parse_schedule_reads_state_and_age() {
    let prefix = "pump log noise\n";
    let with_age = r#"{"name":"head-1","schedule":{"state":"due","cadence_seconds":14400,"age_seconds":15000}}"#;
    assert_eq!(
        parse_schedule(&format!("{prefix}{with_age}")),
        Some((String::from("due"), Some(15_000)))
    );

    let no_age = r#"{"name":"head-1","schedule":{"state":"due","cadence_seconds":14400}}"#;
    assert_eq!(
        parse_schedule(&format!("{prefix}{no_age}")),
        Some((String::from("due"), None))
    );

    assert_eq!(parse_schedule("garbage"), None);
    assert_eq!(parse_schedule(r#"{"name":"head-1"}"#), None);
}
