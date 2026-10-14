//! Tests for [`super::remote`].

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use serde_json::json;
use tempfile::tempdir;

use crate::pump::config::PumpConfig;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::remote::{offer, offer_with};
use crate::pump::state::{MergeJob, MergeResult};

/// Clock value at offer time, so every test knows the batch id.
const START: u64 = 2_000_000;
/// The batch id the offer mints at [`START`].
const BID: &str = "b2000000";

fn cfg() -> PumpConfig {
    let mut cfg = PumpConfig::default();
    cfg.merge.remote_wait = 60;
    cfg.merge.remote_limit = 300;
    cfg
}

fn job(uid: &str) -> MergeJob {
    MergeJob {
        uid: uid.to_string(),
        branch: format!("unit/{uid}-a1"),
        title: format!("unit {uid}"),
        need: "56f1d749".to_string(),
        proof: vec!["cargo test".to_string()],
        tools: "plain".to_string(),
        write_scope: vec![format!("src/{uid}.rs")],
    }
}

fn flag_on(ctx: &Ctx) {
    fs::write(ctx.home.join("REMOTE-MERGE"), "").unwrap();
}

fn jobs_dir(ctx: &Ctx) -> PathBuf {
    ctx.home.join("merge-jobs")
}

fn wait_for(path: &PathBuf) {
    for _ in 0..5_000 {
        if path.exists() {
            return;
        }
        thread::sleep(Duration::from_millis(1));
    }
}

/// Play the remote merger: once the job file appears, claim it by rename,
/// then write `result` (when given) and advance the fake clock by `advance`
/// seconds.
fn merger(home: PathBuf, result: Option<String>, advance: u64, host: Arc<FakeHost>) {
    thread::spawn(move || {
        let dir = home.join("merge-jobs");
        let job = dir.join(format!("{BID}.job.json"));
        wait_for(&job);
        fs::rename(&job, dir.join(format!("{BID}.claimed.json"))).unwrap();
        if let Some(body) = result {
            fs::write(dir.join(format!("{BID}.result.json")), body).unwrap();
        }
        if advance > 0 {
            host.advance(advance);
        }
    });
}

#[test]
fn no_remote_merge_flag_returns_none_and_writes_nothing() {
    let dir = tempdir().unwrap();
    let (ctx, _host) = test_ctx(dir.path(), cfg());
    assert!(offer(&ctx, &[job("u1")]).is_none());
    assert!(!jobs_dir(&ctx).exists());
}

#[test]
fn an_unclaimed_batch_is_withdrawn_after_remote_wait_and_returns_none() {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), cfg());
    flag_on(&ctx);
    host.set_now(START);
    let dir = jobs_dir(&ctx);
    let watch = dir.clone();
    let clock = Arc::clone(&host);
    thread::spawn(move || {
        wait_for(&watch.join(format!("{BID}.job.json")));
        clock.advance(61);
    });
    assert!(offer(&ctx, &[job("u1")]).is_none());
    let want = format!("remote merger did not take batch {BID}; merging here");
    assert!(host.logs().iter().any(|l| l.contains(&want)));
    assert!(!dir.join(format!("{BID}.job.json")).exists());
}

#[test]
fn a_claimed_batch_with_a_result_returns_the_results() {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), cfg());
    flag_on(&ctx);
    host.set_now(START);
    let body = json!([
        {"uid": "u1", "ok": true, "tip": "cafe123", "outside": [], "secs": 9},
        {"uid": "u2", "ok": false, "why": "proof-failed", "detail": "exit 101",
         "outside": ["docs/x.toml"], "secs": 3}
    ])
    .to_string();
    merger(ctx.home.clone(), Some(body), 0, Arc::clone(&host));
    let results = offer(&ctx, &[job("u1"), job("u2")]).unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].uid, "u1");
    assert_eq!(results[0].ok, Some(true));
    assert_eq!(results[0].tip.as_deref(), Some("cafe123"));
    assert_eq!(results[1].why.as_deref(), Some("proof-failed"));
    assert_eq!(results[1].outside, vec!["docs/x.toml"]);
    let want = format!("remote batch {BID}: 2 results in 0s");
    assert!(host.logs().iter().any(|l| l.contains(&want)));
}

#[test]
fn a_claimed_batch_without_a_result_times_out_and_returns_none() {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), cfg());
    flag_on(&ctx);
    host.set_now(START);
    merger(ctx.home.clone(), None, 301, Arc::clone(&host));
    assert!(offer(&ctx, &[job("u1")]).is_none());
    let want = format!("remote merger timed out on batch {BID}; merging here");
    assert!(host.logs().iter().any(|l| l.contains(&want)));
}

#[test]
fn the_job_file_is_a_json_array_of_merge_jobs_with_the_documented_keys() {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), cfg());
    flag_on(&ctx);
    host.set_now(START);
    let body = json!([{"uid": "u1", "ok": true}]).to_string();
    merger(ctx.home.clone(), Some(body), 0, Arc::clone(&host));
    offer(&ctx, &[job("u1")]).unwrap();
    let claimed = fs::read_to_string(jobs_dir(&ctx).join(format!("{BID}.claimed.json"))).unwrap();
    let values: Vec<serde_json::Value> = serde_json::from_str(&claimed).unwrap();
    assert_eq!(values.len(), 1);
    let mut keys: Vec<&str> = values[0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "branch",
            "need",
            "proof",
            "title",
            "tools",
            "uid",
            "write_scope"
        ]
    );
    let back: Vec<MergeJob> = serde_json::from_str(&claimed).unwrap();
    assert_eq!(back.len(), 1);
    assert_eq!(back[0].uid, "u1");
    assert_eq!(back[0].branch, "unit/u1-a1");
    assert_eq!(back[0].need, "56f1d749");
    assert_eq!(back[0].tools, "plain");
    assert_eq!(back[0].proof, vec!["cargo test"]);
    assert_eq!(back[0].write_scope, vec!["src/u1.rs"]);
}

#[test]
fn a_result_with_ok_null_and_a_why_round_trips() {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), cfg());
    flag_on(&ctx);
    host.set_now(START);
    let body =
        json!([{"uid": "u1", "ok": null, "why": "proof-failed", "detail": "exit 101"}]).to_string();
    merger(ctx.home.clone(), Some(body), 0, Arc::clone(&host));
    let results: Vec<MergeResult> = offer_with(&ctx, &[job("u1")], 0).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].uid, "u1");
    assert_eq!(results[0].ok, None);
    assert_eq!(results[0].why.as_deref(), Some("proof-failed"));
    assert_eq!(results[0].detail.as_deref(), Some("exit 101"));
    let again = serde_json::to_string(&results).unwrap();
    assert!(again.contains(r#""ok":null"#));
    assert!(again.contains(r#""why":"proof-failed""#));
}

#[test]
fn an_unparseable_result_file_returns_none() {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), cfg());
    flag_on(&ctx);
    host.set_now(START);
    merger(
        ctx.home.clone(),
        Some("not a json array".to_string()),
        0,
        Arc::clone(&host),
    );
    assert!(offer(&ctx, &[job("u1")]).is_none());
    assert!(
        host.logs()
            .iter()
            .any(|l| l.contains("unparseable result file"))
    );
}
