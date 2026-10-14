//! The remote merger file protocol (`remote_merge`).
//!
//! When the flag file `<home>/REMOTE-MERGE` exists, merge batches are offered
//! to a remote merger through `<home>/merge-jobs/`: this host writes
//! `<bid>.job.json`, the merger claims it by renaming or removing the file,
//! and answers with `<bid>.result.json`. No taker, a timeout, or a bad result
//! file all fall back to merging locally (`None`).

use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use crate::pump::host::Ctx;
use crate::pump::state::{MergeJob, MergeResult};

/// Seconds between polls while a batch is offered or awaited.
const POLL_SECS: u64 = 3;

/// Offer a batch of merge jobs to the remote merger.
///
/// Writes `<home>/merge-jobs/b<now>.job.json` (a JSON array of `jobs`), then
/// waits up to `merge.remote_wait` seconds for the merger to claim the file
/// by renaming or removing it; an unclaimed batch is withdrawn and the log
/// notes the fallback. A claimed batch must produce `<bid>.result.json`
/// within `merge.remote_limit` seconds; its parsed array of results is
/// returned as `Some`.
///
/// `None` means: merge here — no `REMOTE-MERGE` flag, a failed job-file
/// write, no claim in time, no result in time, or an unparseable result file.
#[must_use]
pub fn offer(ctx: &Ctx, jobs: &[MergeJob]) -> Option<Vec<MergeResult>> {
    offer_with(ctx, jobs, POLL_SECS)
}

/// [`offer`] with the poll interval in seconds as a parameter; a zero poll
/// yields instead of sleeping, so tests can drive the fake clock from a
/// helper thread without real waiting.
pub(crate) fn offer_with(ctx: &Ctx, jobs: &[MergeJob], poll: u64) -> Option<Vec<MergeResult>> {
    if !ctx.flag("REMOTE-MERGE") {
        return None;
    }
    let dir = ctx.home.join("merge-jobs");
    if fs::create_dir_all(&dir).is_err() {
        ctx.log("remote merge: cannot create merge-jobs; merging here");
        return None;
    }
    wait_for_open_claims(ctx, &dir, poll);
    let bid = format!("b{}", ctx.now());
    let job = dir.join(format!("{bid}.job.json"));
    if !write_job_file(ctx, &job, jobs) {
        return None;
    }
    let result = dir.join(format!("{bid}.result.json"));
    let t0 = ctx.now();
    // The remote merger claims a batch by renaming or removing the job file;
    // whoever makes it disappear first owns it.
    while ctx.now().saturating_sub(t0) < ctx.cfg.merge.remote_wait && job.exists() {
        sleep(poll);
    }
    if fs::remove_file(&job).is_ok() {
        ctx.log(&format!(
            "remote merger did not take batch {bid}; merging here"
        ));
        return None;
    }
    while ctx.now().saturating_sub(t0) < ctx.cfg.merge.remote_limit && !result.exists() {
        sleep(poll);
    }
    if !result.exists() {
        ctx.log(&format!(
            "remote merger timed out on batch {bid}; merging here"
        ));
        return None;
    }
    let results = read_result_file(ctx, &result)?;
    let secs = ctx.now().saturating_sub(t0);
    ctx.log(&format!(
        "remote batch {bid}: {} results in {secs}s",
        results.len()
    ));
    Some(results)
}

/// Batches a remote merger has claimed and not answered: a file named
/// `b<secs>.<anything>` other than the job, result or temp file, with no
/// `b<secs>.result.json` beside it, offered less than `merge.remote_limit`
/// seconds ago. Such a batch can outlive the pump that offered it (a restart),
/// and its merger will still push the integration branch.
fn open_claims(ctx: &Ctx, dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut open: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let is_tmp = path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("tmp"));
        if name.ends_with(".job.json") || name.ends_with(".result.json") || is_tmp {
            continue;
        }
        let Some(bid) = name.split('.').next() else {
            continue;
        };
        let Some(Ok(secs)) = bid.strip_prefix('b').map(str::parse::<u64>) else {
            continue;
        };
        let answered = dir.join(format!("{bid}.result.json")).exists();
        let live = ctx.now().saturating_sub(secs) < ctx.cfg.merge.remote_limit;
        if !answered && live {
            open.push(bid.to_owned());
        }
    }
    open.sort();
    open.dedup();
    open
}

/// Before offering a new batch, wait while an earlier batch is still claimed
/// and unanswered, so two mergers never push the integration branch at once.
/// The wait ends when every open claim is answered or ages past
/// `merge.remote_limit`.
fn wait_for_open_claims(ctx: &Ctx, dir: &Path, poll: u64) {
    let open = open_claims(ctx, dir);
    if open.is_empty() {
        return;
    }
    ctx.log(&format!(
        "remote merge: waiting for claimed batch {} before offering another",
        open.join(", ")
    ));
    while !open_claims(ctx, dir).is_empty() {
        sleep(poll);
    }
}

/// Write the batch as a JSON array: `.tmp` first, then rename into place.
fn write_job_file(ctx: &Ctx, job: &Path, jobs: &[MergeJob]) -> bool {
    let body = match serde_json::to_string_pretty(jobs) {
        Ok(body) => body,
        Err(err) => {
            ctx.log(&format!("remote merge: cannot serialize the batch: {err}"));
            return false;
        }
    };
    let tmp = tmp_sibling(job);
    if fs::write(&tmp, body).is_err() || fs::rename(&tmp, job).is_err() {
        ctx.log("remote merge: cannot write the job file; merging here");
        return false;
    }
    true
}

/// Load and parse a `<bid>.result.json` file; `None` (with a log line) when
/// it cannot be read or is not a JSON array of results.
fn read_result_file(ctx: &Ctx, result: &Path) -> Option<Vec<MergeResult>> {
    let text = match fs::read_to_string(result) {
        Ok(text) => text,
        Err(err) => {
            ctx.log(&format!(
                "remote merge: cannot read {}: {err}",
                result.display()
            ));
            return None;
        }
    };
    match serde_json::from_str(&text) {
        Ok(results) => Some(results),
        Err(err) => {
            ctx.log(&format!(
                "remote merge: unparseable result file {}: {err}",
                result.display()
            ));
            None
        }
    }
}

/// The `<path>.tmp` sibling used for atomic writes.
fn tmp_sibling(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.tmp", path.to_string_lossy()))
}

/// Pause `secs` between polls; zero yields instead of sleeping.
fn sleep(secs: u64) {
    if secs == 0 {
        thread::yield_now();
    } else {
        thread::sleep(Duration::from_secs(secs));
    }
}
