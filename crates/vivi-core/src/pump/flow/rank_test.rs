//! Tests for flow ranking.

use std::collections::BTreeMap;

use serde_json::json;

use super::{flow_key, need_key, single_flight};
use crate::pump::jobs::{JobSpec, JobStatus, PoolJob};

fn job(id: &str, queued_at: u64, payload: serde_json::Value) -> PoolJob {
    let mut job = PoolJob::from(JobSpec::new(id, "hand", "hand", "s", "b"));
    job.queued_at = queued_at;
    job.payload = payload;
    job
}

fn merge_main(id: &str, target: &str, kind: &str) -> PoolJob {
    let mut job = job(
        id,
        1,
        json!({"kind": kind, "class": "merge-main", "need": "n"}),
    );
    job.target = Some(target.to_owned());
    job
}

#[test]
fn repair_before_cto_before_priority_before_age() {
    let priorities = BTreeMap::from([("hi".to_owned(), 5), ("lo".to_owned(), 1)]);
    let mut jobs = vec![
        job(
            "old-lo",
            1,
            json!({"kind": "build", "class": "hand", "need": "lo"}),
        ),
        job(
            "new-hi",
            9,
            json!({"kind": "build", "class": "hand", "need": "hi"}),
        ),
        job(
            "cto",
            9,
            json!({"kind": "build", "class": "cto", "need": "lo"}),
        ),
        job(
            "repair",
            9,
            json!({"kind": "repair", "class": "hand", "need": "lo"}),
        ),
    ];
    jobs.sort_by_key(|job| flow_key(job, &priorities));
    let ids: Vec<&str> = jobs.iter().map(|job| job.id.as_str()).collect();
    assert_eq!(ids, ["repair", "cto", "new-hi", "old-lo"]);
}

#[test]
fn equal_ranks_oldest_first() {
    let priorities = BTreeMap::new();
    let payload = json!({"kind": "build", "class": "hand", "need": "n"});
    let newer = flow_key(&job("a", 20, payload.clone()), &priorities);
    let older = flow_key(&job("b", 10, payload), &priorities);
    assert!(older < newer);
}

#[test]
fn old_pipeline_job_has_no_key() {
    let plain = job("x", 1, json!({"need": "n"}));
    assert_eq!(flow_key(&plain, &BTreeMap::new()), None);
}

#[test]
fn need_key_orders_repair_priority_age_handle() {
    let mut keys = vec![
        need_key(false, 9, 1, "a"),
        need_key(true, 0, 9, "b"),
        need_key(false, 1, 1, "c"),
        need_key(false, 9, 1, "0"),
    ];
    keys.sort();
    let handles: Vec<&str> = keys.iter().map(|key| key.3.as_str()).collect();
    assert_eq!(handles, ["b", "0", "a", "c"]);
}

#[test]
fn second_merge_main_for_target_waits() {
    let pending = vec![
        merge_main("m1", "t", "build"),
        merge_main("m2", "t", "build"),
    ];
    let kept = single_flight(pending, &BTreeMap::new());
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].id, "m1");
}

#[test]
fn dispatched_merge_main_blocks_target() {
    let mut running = merge_main("run", "t", "build");
    running.status = JobStatus::Dispatched;
    let jobs = BTreeMap::from([(running.id.clone(), running)]);
    let kept = single_flight(vec![merge_main("m1", "t", "build")], &jobs);
    assert!(kept.is_empty());
}

#[test]
fn merge_main_for_another_target_passes() {
    let mut running = merge_main("run", "t", "build");
    running.status = JobStatus::Dispatched;
    let jobs = BTreeMap::from([(running.id.clone(), running)]);
    let other = merge_main("m1", "u", "build");
    let plain = job("h", 1, json!({"kind": "build", "class": "hand"}));
    let kept = single_flight(vec![other, plain], &jobs);
    assert_eq!(kept.len(), 2);
}

#[test]
fn repair_kind_merge_main_sorted_first_is_kept() {
    let priorities = BTreeMap::new();
    let mut pending = vec![
        merge_main("build-m", "t", "build"),
        merge_main("repair-m", "t", "repair"),
    ];
    pending.sort_by_key(|job| flow_key(job, &priorities));
    let kept = single_flight(pending, &BTreeMap::new());
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].id, "repair-m");
}
