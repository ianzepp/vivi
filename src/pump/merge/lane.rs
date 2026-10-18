//! Merge lanes and the pull-model [`Merger`].
//!
//! A lane is one worker thread with its own checkout and build directory.
//! Every configured repository target gets `merge.lanes` of them. The main
//! thread owns the ranking: [`Merger::feed`] hands each idle lane the
//! top-ranked `delivered` units for its target at the moment the lane is
//! free, so a fix unit delivered late is taken by the next idle lane, never
//! queued behind work already sent. A lane never touches the pump state;
//! every effect flows back through one results channel and
//! [`Merger::apply`] stays the only state writer.

use std::any::Any;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::thread;

use crate::pump::host::{Ctx, shell_quote};
use crate::pump::ready::{dependency_holds, note_hold, priority_needs, ranked};
use crate::pump::remote;
use crate::pump::state::{MergeJob, MergeResult, State, Status};

use super::apply::{apply_failure, apply_merged, apply_undecided};
use super::{batch_work, merge_work, tail, undecided_result};

/// The lane main thread's view of one merge lane.
struct Lane {
    /// The repository target whose units this lane merges.
    target: String,
    /// The lane number within its target.
    n: u32,
    /// Batches go to the lane thread through this channel.
    send: Sender<Vec<MergeJob>>,
    /// Uids inside the lane whose results are not applied yet; empty means
    /// the lane is idle.
    busy: Vec<String>,
    /// False once the lane thread could not start or has gone away.
    alive: bool,
}

/// The merge workers: one thread per lane, fed by [`Merger::feed`] and
/// drained by [`Merger::apply`].
///
/// A lane blocks on one batch, offers it to the remote merger and falls back
/// to [`batch_work`] or [`merge_work`] in its own checkout. It never touches
/// the state; every effect on units flows back through the results channel.
pub struct Merger {
    lanes: Mutex<Vec<Lane>>,
    pub(crate) results: Receiver<MergeResult>,
}

/// What a lane thread needs to do its work.
struct LaneWorker {
    /// `<target>/<n>`, for log lines.
    name: String,
    /// The target's own integration checkout, the source for lane clones.
    base: Ctx,
    /// This lane's context: its repository is the lane checkout.
    ctx: Ctx,
}

impl Merger {
    /// Start the lane threads for every configured repository target. They
    /// run until this `Merger` is dropped: dropping closes the batch
    /// channels, which ends each loop.
    #[must_use]
    pub fn start(ctx: &Ctx) -> Merger {
        let (out, results) = channel();
        let mut lanes = Vec::new();
        for id in ctx.cfg.target_configs().keys() {
            let Some(target) = ctx.for_target(id) else {
                continue;
            };
            for n in 0..target.cfg.merge.lanes.max(1) {
                lanes.push(start_lane(&target, n, &out));
            }
        }
        if lanes.iter().any(|lane| !lane.alive) {
            ctx.log("a merge lane did not start; its units stay delivered");
        }
        Merger {
            lanes: Mutex::new(lanes),
            results,
        }
    }

    /// One detached lane of the `default` target: the channel ends a test
    /// drives itself, no thread started.
    #[cfg(test)]
    pub(crate) fn detached() -> (Merger, Sender<MergeResult>, Receiver<Vec<MergeJob>>) {
        let (merger, out, mut lanes) = Merger::detached_lanes(1);
        (merger, out, lanes.remove(0))
    }

    /// `count` detached lanes of the `default` target, lane 0 first.
    #[cfg(test)]
    pub(crate) fn detached_lanes(
        count: u32,
    ) -> (Merger, Sender<MergeResult>, Vec<Receiver<Vec<MergeJob>>>) {
        let (out, results) = channel();
        let mut lanes = Vec::new();
        let mut receivers = Vec::new();
        for n in 0..count {
            let (send, jobs) = channel();
            lanes.push(Lane {
                target: String::from("default"),
                n,
                send,
                busy: Vec::new(),
                alive: true,
            });
            receivers.push(jobs);
        }
        let merger = Merger {
            lanes: Mutex::new(lanes),
            results,
        };
        (merger, out, receivers)
    }

    /// Hand every idle lane up to `merge.batch` of the top-ranked `delivered`
    /// units of its target and move exactly those units to `merging`:
    /// `merging` means "inside a lane", the rest stay `delivered`. Priority
    /// units rank first, then fix units, then the rest. Under the
    /// `HOLD-MERGE` flag only fix units and priority units are taken.
    ///
    /// State change only: the lanes touch repositories, never the state, so
    /// a batch sent here is applied later by [`Merger::apply`].
    pub fn feed(&self, ctx: &Ctx, st: &mut State) {
        let mut lanes = self.lock();
        let batches: BTreeMap<String, usize> = ctx
            .cfg
            .target_configs()
            .into_iter()
            .map(|(id, target)| (id, target.merge.map_or(1, |m| m.batch as usize)))
            .collect();
        recheck_held(st, &batches);
        let known: BTreeSet<String> = lanes.iter().map(|lane| lane.target.clone()).collect();
        let mut waiting = eligible(ctx, st, &known);
        for lane in lanes.iter_mut().filter(|l| l.alive && l.busy.is_empty()) {
            let size = batches.get(&lane.target).copied().unwrap_or(1);
            let Some(queue) = waiting.get_mut(&lane.target) else {
                continue;
            };
            let uids: Vec<String> = queue.drain(..size.min(queue.len())).collect();
            if !uids.is_empty() {
                send_batch(ctx, st, lane, uids);
            }
        }
    }

    /// Drain finished merge results into state (`apply`): undecided units
    /// return to `delivered`, decided failures go through
    /// [`crate::pump::collect::fail`], and merges land with their tip and
    /// outside files — closing the need and appending `needs-ready.txt` when
    /// its last unit just became satisfied. A lane whose last outstanding
    /// result is applied is idle again.
    pub fn apply(&self, ctx: &Ctx, st: &mut State) {
        while let Ok(result) = self.results.try_recv() {
            self.release(&result.uid);
            match result.ok {
                None => apply_undecided(ctx, st, &result),
                Some(false) => apply_failure(ctx, st, &result),
                Some(true) => apply_merged(ctx, st, &result),
            }
        }
    }

    /// The lanes, whether or not a panicking thread poisoned the lock.
    fn lock(&self) -> MutexGuard<'_, Vec<Lane>> {
        self.lanes.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A unit's result came back: it is no longer inside any lane.
    fn release(&self, uid: &str) {
        for lane in self.lock().iter_mut() {
            lane.busy.retain(|busy| busy != uid);
        }
    }
}

/// Held units whose repository target is configured again become
/// `delivered` so they can be taken.
fn recheck_held(st: &mut State, targets: &BTreeMap<String, usize>) {
    for unit in st.units.values_mut() {
        if unit.status == Status::Held
            && unit.branch.is_some()
            && targets.contains_key(unit.target_id())
        {
            unit.status = Status::Delivered;
        }
    }
}

/// The `delivered` units that may be merged now, in rank order, grouped by
/// target. Dependency holds, the `HOLD-MERGE` flag and the target check are
/// applied here; a unit naming a target no lane serves is held and reported.
fn eligible(
    ctx: &Ctx,
    st: &mut State,
    known: &BTreeSet<String>,
) -> BTreeMap<String, VecDeque<String>> {
    let hold = ctx.flag("HOLD-MERGE");
    let needs = priority_needs(ctx);
    let holds = dependency_holds(ctx, st);
    let mut waiting: BTreeMap<String, VecDeque<String>> = BTreeMap::new();
    for uid in ranked(ctx, st, st.order.clone()) {
        let dependency = st
            .units
            .get(&uid)
            .and_then(|unit| holds.get(&unit.need))
            .and_then(|deps| deps.first())
            .cloned();
        if note_hold(ctx, st, &uid, Status::Delivered, dependency) {
            continue;
        }
        let Some(unit) = st.units.get_mut(&uid) else {
            continue;
        };
        if hold && !uid.starts_with(&ctx.cfg.fix_prefix) && !needs.contains(&unit.need) {
            continue;
        }
        if unit.status != Status::Delivered {
            continue;
        }
        let target = unit.target_id().to_string();
        if !known.contains(&target) {
            unit.status = Status::Held;
            let detail = format!("repository target `{target}` is not configured");
            ctx.exception("unknown-target", &uid, &detail);
            continue;
        }
        waiting.entry(target).or_default().push_back(uid);
    }
    waiting
}

/// Mark `uids` as `merging` and send them to the lane as one batch. A lane
/// that cannot take the batch is retired and its units return to `delivered`.
fn send_batch(ctx: &Ctx, st: &mut State, lane: &mut Lane, uids: Vec<String>) {
    let mut jobs = Vec::new();
    for uid in &uids {
        let Some(unit) = st.units.get_mut(uid) else {
            continue;
        };
        unit.status = Status::Merging;
        jobs.push(MergeJob {
            uid: uid.clone(),
            target: unit.target_id().to_string(),
            branch: unit.branch.clone().unwrap_or_default(),
            title: unit.title.clone(),
            need: unit.need.clone(),
            proof: unit.proof.clone(),
            tools: unit.tools.clone(),
            write_scope: unit.write_scope.clone(),
        });
    }
    ctx.log(&format!(
        "lane {}/{} takes {}: {}",
        lane.target,
        lane.n,
        jobs.len(),
        uids.join(",")
    ));
    if lane.send.send(jobs).is_err() {
        lane.alive = false;
        for uid in &uids {
            if let Some(unit) = st.units.get_mut(uid) {
                unit.status = Status::Delivered;
            }
        }
        ctx.log(&format!("lane {}/{} is gone", lane.target, lane.n));
        return;
    }
    lane.busy = uids;
}

/// Start the thread of lane `n` of `target`. Lane 0 works in the target's
/// own integration checkout (warm build directory, no cold clone); every
/// other lane works in `<home>/merge-lanes/<target>/<n>`, cloned on first
/// use.
fn start_lane(target: &Ctx, n: u32, out: &Sender<MergeResult>) -> Lane {
    let (send, jobs) = channel();
    let ctx = if n == 0 {
        target.clone()
    } else {
        target.with_repo(lane_dir(target, n))
    };
    let worker = LaneWorker {
        name: format!("{}/{n}", target.target_id),
        base: target.clone(),
        ctx,
    };
    let out = out.clone();
    let spawned = thread::Builder::new()
        .name(format!("pump-merge-{}-{n}", target.target_id))
        .spawn(move || lane_loop(&worker, &jobs, &out));
    Lane {
        target: target.target_id.clone(),
        n,
        send,
        busy: Vec::new(),
        alive: spawned.is_ok(),
    }
}

/// The checkout directory of lane `n` of `target`.
fn lane_dir(target: &Ctx, n: u32) -> std::path::PathBuf {
    target
        .home
        .join("merge-lanes")
        .join(&target.target_id)
        .join(n.to_string())
}

/// The lane loop: block on one batch — the loop ends when the sender is
/// dropped — merge it, and hand out exactly one result per job. A panic in
/// the work becomes one `merge-error` result per job, never a dead lane.
fn lane_loop(lane: &LaneWorker, batches: &Receiver<Vec<MergeJob>>, out: &Sender<MergeResult>) {
    while let Ok(batch) = batches.recv() {
        let work = AssertUnwindSafe(|| lane_work(lane, &batch));
        let done = match catch_unwind(work) {
            Ok(results) => results,
            Err(err) => {
                let detail = panic_text(&err);
                lane.ctx.log(&format!("MERGE ERROR {detail}"));
                batch
                    .iter()
                    .map(|j| undecided_result(&j.uid, "merge-error", &detail))
                    .collect()
            }
        };
        dispatch_results(out, &batch, done);
    }
}

/// Make sure the lane checkout exists, then merge the batch in it. A checkout
/// that cannot be made leaves every job undecided, to be re-fed later.
fn lane_work(lane: &LaneWorker, jobs: &[MergeJob]) -> Vec<MergeResult> {
    if let Err(detail) = ensure_checkout(&lane.base, &lane.ctx) {
        lane.ctx
            .log(&format!("lane {} checkout failed: {detail}", lane.name));
        return jobs
            .iter()
            .map(|j| undecided_result(&j.uid, "lane-checkout-failed", &detail))
            .collect();
    }
    run_batch(&lane.ctx, &lane.name, jobs)
}

/// Create the lane's own clone when it does not exist yet: a separate
/// repository that borrows the objects of the target's integration checkout
/// (`--reference`) and fetches from the same remote URL that checkout uses,
/// so concurrent lanes never share refs. The target's own checkout is lane 0
/// and needs nothing.
fn ensure_checkout(base: &Ctx, lane: &Ctx) -> Result<(), String> {
    if lane.repo == base.repo || lane.repo.join(".git").exists() {
        return Ok(());
    }
    let remote = base.cfg.remote.as_str();
    let url = base.git(&format!("remote get-url {}", shell_quote(remote)), 30);
    let url = if url.code == 0 { url.text.trim() } else { "" };
    if url.is_empty() {
        return Err(format!(
            "no URL for remote `{remote}` in {}",
            base.repo.display()
        ));
    }
    if let Some(parent) = lane.repo.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let cmd = format!(
        "git clone -q --reference {} -o {} --branch {} {} {}",
        shell_quote(&base.repo.to_string_lossy()),
        shell_quote(remote),
        shell_quote(&base.cfg.branch),
        shell_quote(url),
        shell_quote(&lane.repo.to_string_lossy())
    );
    base.log(&format!("cloning lane checkout {}", lane.repo.display()));
    let out = base.sh(&cmd, None, 900);
    if out.code != 0 {
        return Err(tail(&out.text, 300));
    }
    Ok(())
}

/// Offer the batch to the remote merger, else merge here: [`batch_work`]
/// for more than one job, [`merge_work`] for exactly one, then the
/// `lane <target>/<n> batch of <n>: <m> merged` log line.
fn run_batch(ctx: &Ctx, name: &str, jobs: &[MergeJob]) -> Vec<MergeResult> {
    let done = match remote::offer(ctx, jobs) {
        Some(results) => results,
        None if jobs.len() > 1 => batch_work(ctx, jobs),
        None => vec![merge_work(ctx, &jobs[0])],
    };
    let merged = done.iter().filter(|r| r.ok == Some(true)).count();
    ctx.log(&format!(
        "lane {name} batch of {}: {merged} merged",
        jobs.len()
    ));
    done
}

/// Send one result per job, results deduplicated by uid, and every job the
/// batch's results never named gets an `ok=None, why="merge-undecided"`
/// result.
fn dispatch_results(out: &Sender<MergeResult>, jobs: &[MergeJob], done: Vec<MergeResult>) {
    let mut seen: Vec<String> = Vec::new();
    for result in done {
        if seen.contains(&result.uid) {
            continue;
        }
        seen.push(result.uid.clone());
        if out.send(result).is_err() {
            return;
        }
    }
    for job in jobs {
        if seen.iter().any(|uid| uid == &job.uid) {
            continue;
        }
        let result = undecided_result(&job.uid, "merge-undecided", "");
        if out.send(result).is_err() {
            return;
        }
    }
}

/// The panic payload as one line of at most 600 chars, like python's
/// flattened traceback tail.
fn panic_text(err: &Box<dyn Any + Send>) -> String {
    let text = err
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| err.downcast_ref::<&str>().map(|s| (*s).to_string()))
        .unwrap_or_else(|| String::from("unknown panic"));
    tail(&text.replace('\n', " | "), 600)
}
