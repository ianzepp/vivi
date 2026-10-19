use super::format::{self, OutputFormat};
use super::model::{AttemptRecord, Dimension, Filter, NeedWindow, ReportData, View};
use super::views;

fn attempt(id: &str) -> AttemptRecord {
    AttemptRecord {
        need: "n1".into(),
        task: Some("t1".into()),
        attempt: id.into(),
        actor: Some("space/member".into()),
        role: Some("hand".into()),
        pool: Some("local".into()),
        slot: Some("slot-1".into()),
        model: Some("model-x".into()),
        kind: Some("feature".into()),
        start: Some(1_700_000_000),
        end: Some(1_700_000_300),
        agent_seconds: Some(60),
        outcome: "delivered".into(),
        ready_at: Some(1_699_999_900),
        delivered_at: Some(1_700_000_200),
        merged_at: Some(1_700_000_260),
    }
}

#[test]
fn durations_use_exact_ready_delivery_and_merge_times() {
    let mut first = attempt("u1-a1");
    first.start = Some(160);
    first.ready_at = Some(100);
    first.agent_seconds = Some(50);
    first.delivered_at = Some(200);
    first.merged_at = Some(260);
    let mut second = attempt("u1-a2");
    second.start = Some(170);
    second.ready_at = Some(100);
    second.agent_seconds = Some(150);
    second.delivered_at = Some(250);
    second.merged_at = Some(350);
    let mut old = attempt("u2-a1");
    old.ready_at = None;
    old.delivered_at = None;
    old.merged_at = None;
    old.agent_seconds = None;
    let rows = views::rows(
        &ReportData {
            attempts: vec![first, second, old],
            needs: Vec::new(),
        },
        View::Durations,
        &Filter {
            by: Some(Dimension::Need),
            ..Filter::default()
        },
    );
    let wait = rows
        .iter()
        .find(|row| row["measure"] == "wait_while_ready")
        .unwrap();
    assert_eq!(wait["median_seconds"], 65);
    assert_eq!(wait["p90_seconds"], 70);
    assert_eq!(wait["unavailable"], 1);
    let merge = rows
        .iter()
        .find(|row| row["measure"] == "delivered_to_merged")
        .unwrap();
    assert_eq!(merge["median_seconds"], 80);
    assert_eq!(merge["p90_seconds"], 100);
    assert_eq!(merge["unavailable"], 1);
    let agent = rows
        .iter()
        .find(|row| row["measure"] == "agent_time")
        .unwrap();
    assert_eq!(agent["median_seconds"], 100);
    assert_eq!(agent["p90_seconds"], 150);
    assert_eq!(agent["unavailable"], 1);
}

#[test]
fn invalid_or_missing_duration_endpoints_remain_unavailable() {
    let mut invalid_order = attempt("unit-1-a1");
    invalid_order.start = Some(100);
    invalid_order.ready_at = Some(120);
    invalid_order.delivered_at = Some(300);
    invalid_order.merged_at = Some(200);

    let mut missing_merge = attempt("unit-2-a1");
    missing_merge.start = Some(110);
    missing_merge.ready_at = Some(100);
    missing_merge.delivered_at = Some(130);
    missing_merge.merged_at = None;

    let rows = views::rows(
        &ReportData {
            attempts: vec![invalid_order, missing_merge],
            needs: Vec::new(),
        },
        View::Durations,
        &Filter::default(),
    );
    let wait = rows
        .iter()
        .find(|row| row["measure"] == "wait_while_ready")
        .unwrap();
    assert_eq!(wait["samples"], 1);
    assert_eq!(wait["unavailable"], 1);

    let merge = rows
        .iter()
        .find(|row| row["measure"] == "delivered_to_merged")
        .unwrap();
    assert_eq!(merge["samples"], 0);
    assert_eq!(merge["unavailable"], 2);
}

#[test]
fn need_lead_time_effort_and_attempt_count_preserve_unknowns() {
    let rows = views::rows(
        &ReportData {
            attempts: vec![attempt("u1-a1"), attempt("u1-a2")],
            needs: vec![NeedWindow {
                need: "n1".into(),
                opened_at: Some(100),
                done_at: Some(200),
                done: true,
            }],
        },
        View::Needs,
        &Filter::default(),
    );
    assert_eq!(rows[0]["lead_seconds"], 100);
    assert_eq!(rows[0]["effort_seconds"], 120);
    assert_eq!(rows[0]["attempts"], 2);

    let missing = views::rows(
        &ReportData {
            attempts: vec![AttemptRecord {
                need: "n2".into(),
                agent_seconds: None,
                ..AttemptRecord::default()
            }],
            needs: vec![NeedWindow {
                need: "n2".into(),
                opened_at: None,
                done_at: None,
                done: false,
            }],
        },
        View::Needs,
        &Filter::default(),
    );
    assert!(missing[0]["lead_seconds"].is_null());
    assert!(missing[0]["effort_seconds"].is_null());
    assert_eq!(missing[0]["open_needs"], 1);
    assert_eq!(missing[0]["unavailable_agent_seconds"], 1);
}

#[test]
fn repeated_failures_name_the_unit_and_count_both_attempts() {
    let mut first = attempt("unit-7-a1");
    first.outcome = "proof-failed".into();
    let mut second = attempt("unit-7-a2");
    second.outcome = "proof-failed".into();
    let rows = views::rows(
        &ReportData {
            attempts: vec![first, second],
            needs: Vec::new(),
        },
        View::Failures,
        &Filter::default(),
    );
    assert_eq!(rows[0]["count"], 2);
    assert_eq!(rows[0]["repeat_units"], serde_json::json!(["unit-7"]));
}

#[test]
fn throughput_counts_lifecycle_events_in_their_own_buckets() {
    let mut failed = attempt("unit-2-a1");
    failed.outcome = "proof-failed".into();
    failed.delivered_at = None;
    failed.merged_at = None;
    let rows = views::rows(
        &ReportData {
            attempts: vec![attempt("unit-1-a1"), failed],
            needs: Vec::new(),
        },
        View::Throughput,
        &Filter {
            by: Some(Dimension::Need),
            ..Filter::default()
        },
    );
    assert_eq!(rows[0]["dispatched"], 2);
    assert_eq!(rows[0]["delivered"], 1);
    assert_eq!(rows[0]["merged"], 1);
    assert_eq!(rows[0]["failed"], 1);
}

#[test]
fn throughput_keeps_a_missing_merge_time_unavailable() {
    let mut delivered = attempt("unit-1-a1");
    delivered.merged_at = None;
    let rows = views::rows(
        &ReportData {
            attempts: vec![delivered],
            needs: Vec::new(),
        },
        View::Throughput,
        &Filter {
            by: Some(Dimension::Need),
            ..Filter::default()
        },
    );
    assert_eq!(rows[0]["merged"], serde_json::Value::Null);
    assert_eq!(rows[0]["unavailable"], 1);
}

#[test]
fn throughput_filters_each_lifecycle_event_by_its_own_time() {
    let mut delivery_inside = attempt("unit-1-a1");
    delivery_inside.need = "n1".into();
    delivery_inside.start = Some(100);
    delivery_inside.delivered_at = Some(210);
    delivery_inside.merged_at = Some(240);

    let mut lifecycle_outside = attempt("unit-2-a1");
    lifecycle_outside.need = "n2".into();
    lifecycle_outside.start = Some(220);
    lifecycle_outside.delivered_at = Some(300);
    lifecycle_outside.merged_at = Some(310);

    let rows = views::rows(
        &ReportData {
            attempts: vec![delivery_inside, lifecycle_outside],
            needs: Vec::new(),
        },
        View::Throughput,
        &Filter {
            since: Some(200),
            until: Some(250),
            by: Some(Dimension::Need),
            ..Filter::default()
        },
    );
    let first = rows.iter().find(|row| row["group"] == "n1").unwrap();
    assert_eq!(first["dispatched"], 0);
    assert_eq!(first["delivered"], 1);
    assert_eq!(first["merged"], 1);

    let second = rows.iter().find(|row| row["group"] == "n2").unwrap();
    assert_eq!(second["dispatched"], 1);
    assert_eq!(second["delivered"], 0);
    assert_eq!(second["merged"], 0);
}

#[test]
fn csv_uses_stable_columns_and_quotes_delimiters() {
    let rows = vec![serde_json::json!({"need":"n1", "outcome":"failed, retry"})];
    let output =
        format::render(&rows, &["need", "outcome", "merged_at"], OutputFormat::Csv).unwrap();
    assert_eq!(
        output,
        "need,outcome,merged_at\nn1,\"failed, retry\",unavailable\n"
    );
}

#[test]
fn json_lines_writes_one_record_per_line() {
    let rows = vec![
        serde_json::json!({"attempt":"a1"}),
        serde_json::json!({"attempt":"a2"}),
    ];
    let output = format::render(&rows, &["attempt"], OutputFormat::JsonLines).unwrap();
    assert_eq!(output.lines().count(), 2);
    assert!(output.contains("\"attempt\":\"a1\""));
    assert!(output.contains("\"attempt\":\"a2\""));
}
