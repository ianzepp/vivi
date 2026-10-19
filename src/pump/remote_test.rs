//! Tests for [`super::remote`].

use std::fs;
use std::path::{Path, PathBuf};
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
/// The start of every batch id the lane-0 offers of these tests mint at
/// [`START`]: `b<secs>-<target>-<lane>-`, then a process-wide counter.
const BID_PREFIX: &str = "b2000000-default-0-";

fn cfg() -> PumpConfig {
    let mut cfg = PumpConfig::default();
    // Far beyond any clock movement these tests make, so an offer only ends
    // when the merger acts — never because the fake clock ran out under
    // load. Only the two timeout tests shrink a value below.
    cfg.merge.remote_wait = 100_000;
    cfg.merge.remote_limit = 100_000;
    cfg
}

fn job(uid: &str) -> MergeJob {
    MergeJob {
        uid: uid.to_string(),
        target: String::from("default"),
        branch: crate::pump::need::unit_branch(uid, 1),
        title: format!("unit {uid}"),
        need: "56f1d749".to_string(),
        proof: vec!["cargo test".to_string()],
        tools: "plain".to_string(),
        write_scope: vec![format!("src/{uid}.rs")],
        source: None,
    }
}

fn flag_on(ctx: &Ctx) {
    fs::write(ctx.home.join("REMOTE-MERGE"), "").unwrap();
}

fn jobs_dir(ctx: &Ctx) -> PathBuf {
    ctx.home.join("merge-jobs")
}

/// The batch id of the first job file that shows up in `dir` (real-time wait,
/// polling every 2 ms for up to 10 s).
fn find_bid(dir: &Path) -> String {
    for _ in 0..5_000 {
        if let Some(bid) = job_bids(dir).into_iter().next() {
            return bid;
        }
        thread::sleep(Duration::from_millis(2));
    }
    panic!("no job file appeared in {}", dir.display());
}

/// The batch ids of the job files now in `dir`, sorted.
fn job_bids(dir: &Path) -> Vec<String> {
    let mut bids: Vec<String> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.strip_suffix(".job.json").map(str::to_owned)
        })
        .collect();
    bids.sort();
    bids
}

/// The body of the one `*.claimed.json` file in `dir`.
fn claimed_body(dir: &Path) -> String {
    let path = fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.to_string_lossy().ends_with(".claimed.json"))
        .expect("a claimed batch");
    fs::read_to_string(path).unwrap()
}

/// Play the remote merger: once the job file appears, claim it by rename,
/// then write `result` (when given) and advance the fake clock by `advance`
/// seconds.
fn merger(home: PathBuf, result: Option<String>, advance: u64, host: Arc<FakeHost>) {
    thread::spawn(move || {
        let dir = home.join("merge-jobs");
        let bid = find_bid(&dir);
        let job = dir.join(format!("{bid}.job.json"));
        fs::rename(&job, dir.join(format!("{bid}.claimed.json"))).unwrap();
        if let Some(body) = result {
            // Publish atomically: the offer polls for the result file, so a
            // bare create-then-write could be read half-written.
            let tmp = dir.join(format!("{bid}.result.tmp"));
            fs::write(&tmp, body).unwrap();
            fs::rename(&tmp, dir.join(format!("{bid}.result.json"))).unwrap();
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
    assert!(offer(&ctx, 0, &[job("u1")]).is_none());
    assert!(!jobs_dir(&ctx).exists());
}

#[test]
fn an_unclaimed_batch_is_withdrawn_after_remote_wait_and_returns_none() {
    let dir = tempdir().unwrap();
    let mut cfg = cfg();
    cfg.merge.remote_wait = 60;
    let (ctx, host) = test_ctx(dir.path(), cfg);
    flag_on(&ctx);
    host.set_now(START);
    let dir = jobs_dir(&ctx);
    let watch = dir.clone();
    let clock = Arc::clone(&host);
    thread::spawn(move || {
        find_bid(&watch);
        clock.advance(61);
    });
    assert!(offer(&ctx, 0, &[job("u1")]).is_none());
    let want = format!("remote merger did not take batch {BID_PREFIX}");
    assert!(host.logs().iter().any(|l| l.contains(&want)));
    assert!(job_bids(&dir).is_empty(), "the unclaimed job is withdrawn");
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
    let results = offer(&ctx, 0, &[job("u1"), job("u2")]).unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].uid, "u1");
    assert_eq!(results[0].ok, Some(true));
    assert_eq!(results[0].tip.as_deref(), Some("cafe123"));
    assert_eq!(results[1].why.as_deref(), Some("proof-failed"));
    assert_eq!(results[1].outside, vec!["docs/x.toml"]);
    let want = format!("remote batch {BID_PREFIX}");
    assert!(
        host.logs()
            .iter()
            .any(|l| l.contains(&want) && l.ends_with(": 2 results in 0s"))
    );
}

#[test]
fn a_claimed_batch_without_a_result_times_out_and_returns_none() {
    let dir = tempdir().unwrap();
    let mut cfg = cfg();
    cfg.merge.remote_limit = 300;
    let (ctx, host) = test_ctx(dir.path(), cfg);
    flag_on(&ctx);
    host.set_now(START);
    merger(ctx.home.clone(), None, 301, Arc::clone(&host));
    assert!(offer(&ctx, 0, &[job("u1")]).is_none());
    let want = format!("remote merger timed out on batch {BID_PREFIX}");
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
    offer(&ctx, 0, &[job("u1")]).unwrap();
    let claimed = claimed_body(&jobs_dir(&ctx));
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
            "target",
            "title",
            "tools",
            "uid",
            "write_scope"
        ]
    );
    let back: Vec<MergeJob> = serde_json::from_str(&claimed).unwrap();
    assert_eq!(back.len(), 1);
    assert_eq!(back[0].uid, "u1");
    assert_eq!(back[0].branch, "vivi/unit/u1-a1");
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
    let results: Vec<MergeResult> = offer_with(&ctx, 0, &[job("u1")], 0).unwrap();
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
    assert!(offer(&ctx, 0, &[job("u1")]).is_none());
    assert!(
        host.logs()
            .iter()
            .any(|l| l.contains("unparseable result file"))
    );
}

#[test]
fn an_offer_waits_while_an_earlier_batch_is_claimed_and_unanswered() {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), cfg());
    flag_on(&ctx);
    host.set_now(START);
    let jobs = jobs_dir(&ctx);
    fs::create_dir_all(&jobs).unwrap();
    // A batch an earlier pump process offered, still being merged elsewhere.
    fs::write(jobs.join("b1999900.claimed.json"), "[]").unwrap();
    let earlier = jobs.join("b1999900.result.json");
    let watch = jobs.clone();
    let answer = earlier.clone();
    let probe = thread::spawn(move || {
        thread::sleep(Duration::from_millis(100));
        // No new batch may be offered while the earlier one is open.
        let offered_early = !job_bids(&watch).is_empty();
        fs::write(&answer, "[]").unwrap();
        offered_early
    });
    let body =
        json!([{"uid": "u1", "ok": true, "tip": "cafe123", "outside": [], "secs": 1}]).to_string();
    merger(ctx.home.clone(), Some(body), 0, Arc::clone(&host));
    let results = offer_with(&ctx, 0, &[job("u1")], 0).unwrap();
    assert!(
        !probe.join().unwrap(),
        "offered a batch while another was claimed"
    );
    assert_eq!(results.len(), 1);
    assert!(
        host.logs()
            .iter()
            .any(|l| l.contains("waiting for claimed batch b1999900"))
    );
}

#[test]
fn a_claim_older_than_remote_limit_does_not_hold_the_offer() {
    let dir = tempdir().unwrap();
    let mut cfg = cfg();
    cfg.merge.remote_limit = 300;
    let (ctx, host) = test_ctx(dir.path(), cfg);
    flag_on(&ctx);
    host.set_now(START);
    let jobs = jobs_dir(&ctx);
    fs::create_dir_all(&jobs).unwrap();
    fs::write(jobs.join(format!("b{}.claimed.json", START - 301)), "[]").unwrap();
    let body =
        json!([{"uid": "u1", "ok": true, "tip": "cafe123", "outside": [], "secs": 1}]).to_string();
    merger(ctx.home.clone(), Some(body), 0, Arc::clone(&host));
    assert!(offer(&ctx, 0, &[job("u1")]).is_some());
    assert!(
        !host
            .logs()
            .iter()
            .any(|l| l.contains("waiting for claimed batch"))
    );
}

#[test]
fn two_lanes_offering_in_the_same_second_get_distinct_batches_and_their_own_results() {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), cfg());
    flag_on(&ctx);
    host.set_now(START);
    let jobs = jobs_dir(&ctx);
    let (one, two) = (ctx.clone(), ctx.clone());
    let first = thread::spawn(move || offer_with(&one, 0, &[job("u1")], 0));
    let second = thread::spawn(move || offer_with(&two, 1, &[job("u2")], 0));

    // Both job files exist at once: neither overwrote the other.
    let mut bids: Vec<String> = Vec::new();
    for _ in 0..5_000 {
        bids = job_bids(&jobs);
        if bids.len() == 2 {
            break;
        }
        thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(bids.len(), 2, "{bids:?}");
    assert!(bids.iter().any(|b| b.starts_with("b2000000-default-0-")));
    assert!(bids.iter().any(|b| b.starts_with("b2000000-default-1-")));

    // The merger answers each batch with the uid of its own job only.
    for bid in &bids {
        let body = fs::read_to_string(jobs.join(format!("{bid}.job.json"))).unwrap();
        let sent: Vec<MergeJob> = serde_json::from_str(&body).unwrap();
        fs::remove_file(jobs.join(format!("{bid}.job.json"))).unwrap();
        let answer = json!([{"uid": sent[0].uid, "ok": true, "tip": bid}]).to_string();
        // Write then rename, as the real merger does: an offer that sees the
        // result path must never read a half-written file.
        let tmp = jobs.join(format!("{bid}.result.json.tmp"));
        fs::write(&tmp, answer).unwrap();
        fs::rename(&tmp, jobs.join(format!("{bid}.result.json"))).unwrap();
    }
    let got_one = first.join().unwrap().unwrap();
    let got_two = second.join().unwrap().unwrap();
    assert_eq!(got_one.len(), 1);
    assert_eq!(got_one[0].uid, "u1");
    assert_eq!(got_two.len(), 1);
    assert_eq!(got_two[0].uid, "u2");
}
