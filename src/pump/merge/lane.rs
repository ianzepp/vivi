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
//!
//! Lane 0 of a target works in the target's own integration checkout. Every
//! other lane `n` works in its own clone at
//! `<home>/merge-lanes/<target>/<n>`, created on first use by that lane's
//! first batch ([`ensure_checkout`]) and kept afterwards while the lane has
//! unlanded work; pool clean applies the 2x rule to their build output and
//! does not delete the clones. A lane that cannot work (see [`LANE_FAILS`])
//! is left alone for [`LANE_BACKOFF_SECS`] instead of being retired.

use std::any::Any;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;

use crate::pump::host::{Ctx, shell_quote};
use crate::pump::progress::{LaneProgress, duration_text};
use crate::pump::quarantine;
use crate::pump::ready::{dependency_holds, note_hold, ranked};
use crate::pump::remote;
use crate::pump::state::{MergeJob, MergeResult, State, Status};

use super::apply::{apply_failure, apply_merged, apply_undecided};
use super::fetch::{BASE_REMOTE, ensure_base_remote};
use super::{batch_work, merge_work, prepare_lane, tail, undecided_result};

/// Consecutive batches a lane may lose to lane-level failures before it is
/// put in backoff.
const LANE_FAILS: u32 = 3;

/// Seconds a failing lane is left unfed.
const LANE_BACKOFF_SECS: u64 = 600;

/// `why` codes of an undecided result that say the lane itself cannot work
/// (no checkout, a wedged git, no way to publish), as opposed to a batch
/// whose content is at fault (`publish-conflict`, a fetch of one unit
/// branch, ...).
const LANE_LEVEL: [&str; 8] = [
    "lane-checkout-failed",
    "lane-reset-failed",
    "merge-env-failed",
    "publish-fetch-failed",
    "publish-check-failed",
    "push-failed",
    "proof-missing-tool",
    "merge-error",
];

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
    /// Results of this pump run that merged, for the status line.
    merged: u32,
    /// Results of this pump run that failed as `merge-conflict`.
    conflicts: u32,
    /// False once the lane thread could not start or has gone away.
    alive: bool,
    /// Consecutive batches lost entirely to lane-level failures.
    fails: u32,
    /// The lane is not fed before this epoch second.
    down_until: u64,
    /// For each result of the current batch applied so far: whether it was a
    /// lane-level failure.
    outcomes: Vec<bool>,
    /// What the latest lane-level failure said.
    last_failure: String,
    /// What the lane thread is doing, shared with its context.
    progress: Arc<LaneProgress>,
}

impl Lane {
    /// A lane that has not failed yet.
    fn new(
        target: &str,
        n: u32,
        send: Sender<Vec<MergeJob>>,
        alive: bool,
        progress: Arc<LaneProgress>,
    ) -> Lane {
        Lane {
            target: target.to_string(),
            n,
            send,
            busy: Vec::new(),
            merged: 0,
            conflicts: 0,
            alive,
            fails: 0,
            down_until: 0,
            outcomes: Vec::new(),
            last_failure: String::new(),
            progress,
        }
    }

    /// The status line of this lane at `now`: its state, then the `merged
    /// <m> conflict <c>` counts of this pump run, so a lane at 0% stands out
    /// beside its siblings. A lane whose thread is gone reports only `gone`.
    fn status_line(&self, now: u64) -> String {
        let name = format!("lane {}/{}", self.target, self.n);
        if !self.alive {
            return format!("{name} gone");
        }
        let counts = format!("merged {} conflict {}", self.merged, self.conflicts);
        if !self.busy.is_empty() {
            return format!("{name} busy {} {counts}", self.progress.busy_text(now));
        }
        if self.down_until > now {
            let left = duration_text(self.down_until - now);
            return format!("{name} backoff {left} left {counts}");
        }
        format!("{name} idle {counts}")
    }

    /// The last result of a batch is in: count the batch as lost when every
    /// one of its jobs failed for a lane-level reason, else reset the count.
    /// [`LANE_FAILS`] lost batches in a row put the lane in backoff and raise
    /// the `merge-lane-down` exception.
    fn settle(&mut self, ctx: &Ctx) {
        let lost = !self.outcomes.is_empty() && self.outcomes.iter().all(|f| *f);
        self.outcomes.clear();
        if !lost {
            self.fails = 0;
            return;
        }
        self.fails += 1;
        if self.fails < LANE_FAILS {
            return;
        }
        self.down_until = ctx.now() + LANE_BACKOFF_SECS;
        let name = format!("{}/{}", self.target, self.n);
        let detail = format!(
            "{} batches lost in a row; not fed for {LANE_BACKOFF_SECS}s. Last: {}",
            self.fails, self.last_failure
        );
        ctx.log(&format!("lane {name} is down for {LANE_BACKOFF_SECS}s"));
        ctx.exception("merge-lane-down", &name, &detail);
    }
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

/// The lane's RAM disk request: a fresh disk of `gib` GiB per batch, driven
/// through `runner`. Admission (memory only — no SSD fallback) happens before
/// the batch runs; mounting the disk is unit .2.
pub(crate) struct LaneRam {
    pub(crate) gib: u64,
    pub(crate) runner: std::sync::Arc<dyn crate::pool::ramdisk::Runner>,
    /// Bytes to admit against; `None` samples the machine now. Tests pin this
    /// so admission does not depend on the host's free memory.
    pub(crate) available: Option<u64>,
}

/// What a lane thread needs to do its work.
pub(crate) struct LaneWorker {
    /// The lane number within its target.
    n: u32,
    /// The target's own integration checkout, the source for lane clones.
    base: Ctx,
    /// This lane's context: its repository is the lane checkout.
    ctx: Ctx,
    /// The target's `[pump.ramdisk]` request, when a lane disk is configured.
    ram: Option<LaneRam>,
}

impl LaneWorker {
    /// The checkout this batch operates on (the RAM worktree when enabled).
    #[cfg(test)]
    pub(crate) fn repo_path(&self) -> &std::path::Path {
        &self.ctx.repo
    }
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
            let lane_count = crate::routing::merge_lane_count(&ctx.routing, &target.cfg);
            for n in 0..lane_count {
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
            let progress = Arc::new(LaneProgress::new(&format!("default/{n}")));
            lanes.push(Lane::new("default", n, send, true, progress));
            receivers.push(jobs);
        }
        let merger = Merger {
            lanes: Mutex::new(lanes),
            results,
        };
        (merger, out, receivers)
    }

    /// One status line per lane, in lane order: `lane default/0 busy 8
    /// units 12m03s: check 1/4 stage 1 (2m10s) merged 12 conflict 36`,
    /// `lane default/1 idle merged 12 conflict 36`, `lane default/2 backoff
    /// 7m00s left merged 12 conflict 36` or `lane default/3 gone`. The
    /// `merged`/`conflict` counts are per pump run, not persisted.
    #[must_use]
    pub fn lane_lines(&self, now: u64) -> Vec<String> {
        self.lock()
            .iter()
            .map(|lane| lane.status_line(now))
            .collect()
    }

    /// Hand every idle lane up to `merge.batch` of the top-ranked `delivered`
    /// units of its target and move exactly those units to `merging`:
    /// `merging` means "inside a lane", the rest stay `delivered`. Priority
    /// units rank first, then fix units, then the rest. Under the
    /// `HOLD-MERGE` flag only fix units, units whose need is at or above
    /// the configured priority floor, and units whose write scope is clear
    /// of every red crate are taken.
    ///
    /// State change only: the lanes touch repositories, never the state, so
    /// a batch sent here is applied later by [`Merger::apply`].
    pub fn feed(&self, ctx: &Ctx, st: &mut State) {
        let mut lanes = self.lock();
        let now = ctx.now();
        let batches: BTreeMap<String, usize> = ctx
            .cfg
            .target_configs()
            .into_iter()
            .map(|(id, target)| (id, target.merge.map_or(1, |m| m.batch as usize)))
            .collect();
        recheck_held(st, &batches);
        let known: BTreeSet<String> = lanes.iter().map(|lane| lane.target.clone()).collect();
        let mut waiting = eligible(ctx, st, &known);
        let idle = |l: &&mut Lane| l.alive && l.busy.is_empty() && l.down_until <= now;
        for lane in lanes.iter_mut().filter(idle) {
            let size = batches.get(&lane.target).copied().unwrap_or(1);
            let Some(queue) = waiting.get_mut(&lane.target) else {
                continue;
            };
            let uids = take_batch(st, queue, size);
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
            self.release(ctx, &result);
            match result.ok {
                None if result.quarantine.is_some() => quarantine::set_aside(ctx, st, &result),
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

    /// A unit's result came back: it is no longer inside any lane, and the
    /// lane that held it learns how the job went.
    fn release(&self, ctx: &Ctx, result: &MergeResult) {
        for lane in self.lock().iter_mut() {
            if !lane.busy.contains(&result.uid) {
                continue;
            }
            lane.busy.retain(|busy| busy != &result.uid);
            if result.ok == Some(true) {
                lane.merged += 1;
            }
            if result.ok == Some(false) && result.why.as_deref() == Some("merge-conflict") {
                lane.conflicts += 1;
            }
            let lane_level = lane_level_failure(result);
            if lane_level {
                lane.last_failure = format!(
                    "{}: {}",
                    result.why.as_deref().unwrap_or(""),
                    result.detail.as_deref().unwrap_or("")
                );
            }
            lane.outcomes.push(lane_level);
            if lane.busy.is_empty() {
                lane.settle(ctx);
            }
        }
    }
}

/// True when `result` is an undecided one whose reason says the lane, not
/// the unit, could not work.
fn lane_level_failure(result: &MergeResult) -> bool {
    result.ok.is_none()
        && result.quarantine.is_none()
        && result
            .why
            .as_deref()
            .is_some_and(|why| LANE_LEVEL.contains(&why))
}

/// The next batch from the front of `queue`: up to `size` units, in rank
/// order. A solo unit (its quarantine repair went stuck) goes alone, and in
/// a batch of others it waits at the front of the queue for the next lane.
pub(super) fn take_batch(st: &State, queue: &mut VecDeque<String>, size: usize) -> Vec<String> {
    let solo = |uid: &String| st.units.get(uid).is_some_and(|unit| unit.solo);
    let Some(first) = queue.pop_front() else {
        return Vec::new();
    };
    if solo(&first) {
        return vec![first];
    }
    let mut batch = vec![first];
    let mut skipped = Vec::new();
    while batch.len() < size {
        match queue.pop_front() {
            Some(uid) if solo(&uid) => skipped.push(uid),
            Some(uid) => batch.push(uid),
            None => break,
        }
    }
    for uid in skipped.into_iter().rev() {
        queue.push_front(uid);
    }
    batch
}

/// Held units whose repository target is configured again become
/// `delivered` so they can be taken.
pub(super) fn recheck_held(st: &mut State, targets: &BTreeMap<String, usize>) {
    for unit in st.units.values_mut() {
        if unit.status == Status::Held
            && unit.branch.is_some()
            && targets.contains_key(unit.target_id())
        {
            unit.status = Status::Delivered;
        }
    }
}

/// The crates named by the current red tests: the first whitespace token of
/// every key of the state's red map and of every target's red map. A key
/// reads `<crate> <test>`.
fn red_crates(st: &State) -> BTreeSet<String> {
    let mut crates = BTreeSet::new();
    for name in st
        .red
        .keys()
        .chain(st.target_states.values().flat_map(|t| t.red.keys()))
    {
        if let Some(krate) = name.split_whitespace().next() {
            crates.insert(krate.to_string());
        }
    }
    crates
}

/// True when any `/`-separated segment of any write scope entry names a red
/// crate: `crates/radix-cli/src/lib.rs` touches `radix-cli`, while
/// `docs/x.md` does not.
fn touches_crate(write_scope: &[String], red: &BTreeSet<String>) -> bool {
    write_scope
        .iter()
        .any(|path| path.split('/').any(|segment| red.contains(segment)))
}

/// The `delivered` units that may be merged now, in rank order, grouped by
/// target. Dependency holds, the `HOLD-MERGE` flag and the target check are
/// applied here; a unit naming a target no lane serves is held and reported.
/// Under the hold a unit is taken when it is fix-prefixed, or its need's
/// priority level reaches the configured floor (at least 1), or red tests
/// exist and its write scope touches none of their crates.
pub(super) fn eligible(
    ctx: &Ctx,
    st: &mut State,
    known: &BTreeSet<String>,
) -> BTreeMap<String, VecDeque<String>> {
    let hold = ctx.flag("HOLD-MERGE");
    let floor = ctx.cfg.gate.hold_priority_floor.max(1);
    let levels = crate::pump::priority::levels(ctx);
    let red = red_crates(st);
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
        if hold && !uid.starts_with(&ctx.cfg.fix_prefix) {
            let level = levels.get(&unit.need).copied().unwrap_or(0);
            let clear = !red.is_empty() && !touches_crate(&unit.write_scope, &red);
            if level < floor && !clear {
                continue;
            }
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
            source: unit
                .pool
                .as_deref()
                .and_then(|pool| ctx.pool_sources.get(pool).cloned()),
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
    lane.progress.begin(uids.len(), ctx.now());
    lane.busy = uids;
}

/// Start the thread of lane `n` of `target`. Lane 0 works in the target's
/// own integration checkout (warm build directory, no cold clone); every
/// other lane works in `<home>/merge-lanes/<target>/<n>`, cloned on first
/// use.
fn start_lane(target: &Ctx, n: u32, out: &Sender<MergeResult>) -> Lane {
    let (send, jobs) = channel();
    let name = format!("{}/{n}", target.target_id);
    let progress = Arc::new(LaneProgress::new(&name));
    let ctx = if n == 0 {
        target.with_lane(progress.clone())
    } else {
        target
            .with_repo(lane_dir(target, n))
            .with_lane(progress.clone())
    };
    let worker = LaneWorker {
        n,
        base: target.clone(),
        ctx,
        ram: target.cfg.ramdisk.lane_gib.map(|gib| LaneRam {
            gib,
            runner: std::sync::Arc::new(crate::pool::ramdisk::RealRunner),
            available: None,
        }),
    };
    let out = out.clone();
    let spawned = thread::Builder::new()
        .name(format!("pump-merge-{}-{n}", target.target_id))
        .spawn(move || lane_loop(&worker, &jobs, &out));
    Lane::new(&target.target_id, n, send, spawned.is_ok(), progress)
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
        dispatch_results(&lane.ctx, out, &batch, done);
    }
}

/// Make sure the lane checkout exists, then merge the batch in it. A checkout
/// that cannot be made leaves every job undecided, to be re-fed later.
pub(crate) fn lane_work(lane: &LaneWorker, jobs: &[MergeJob]) -> Vec<MergeResult> {
    if let Err(detail) = ensure_checkout(&lane.base, &lane.ctx) {
        lane.ctx.log(&format!("checkout failed: {detail}"));
        return undecided_all(jobs, "lane-checkout-failed", &detail);
    }
    match &lane.ram {
        None => run_batch(lane, jobs),
        Some(ram) => ramdisk_batch(lane, ram, jobs),
    }
}

/// Admit the batch onto the lane's RAM disk: free memory (minus what attached
/// disks hold and the margin) must cover the disk. A job that does not fit
/// stays queued — every result undecided, so the units return to `Delivered`
/// and are re-fed later; there is no SSD fallback. After admission, a fresh
/// disk holds a detached worktree for the batch.
fn ramdisk_batch(lane: &LaneWorker, ram: &LaneRam, jobs: &[MergeJob]) -> Vec<MergeResult> {
    let available = ram
        .available
        .unwrap_or_else(crate::pool::ramdisk::available_bytes);
    if let Err(err) = crate::pool::ramdisk::admit_with(
        &*ram.runner,
        available,
        ram.gib,
        lane.ctx.cfg.ramdisk.margin_gib,
    ) {
        return undecided_all(jobs, "ramdisk-wait", &err);
    }
    ramdisk_batch_after_admit(lane, ram, jobs, run_batch)
}

/// Mount the admitted disk, register a detached worktree under it, and run
/// the batch with a context whose repository is that worktree.
pub(crate) fn ramdisk_batch_after_admit(
    lane: &LaneWorker,
    ram: &LaneRam,
    jobs: &[MergeJob],
    run: impl FnOnce(&LaneWorker, &[MergeJob]) -> Vec<MergeResult>,
) -> Vec<MergeResult> {
    let volume = lane_volume(lane);
    let mount = lane_mount(&volume);
    let disk = match crate::pool::ramdisk::mount(&*ram.runner, &volume, ram.gib, &mount) {
        Ok(disk) => disk,
        Err(error) => {
            lane.ctx.log(&format!(
                "WARNING lane ramdisk {volume} mount failed: {error}"
            ));
            return undecided_all(jobs, "ramdisk-mount-failed", &error);
        }
    };
    let worktree = disk.mount.join("w");
    let repo = crate::pool::local_git::Repo::new(&lane.ctx.repo);
    let guard = LaneRamGuard::new(lane, ram, repo.clone(), worktree.clone(), disk);
    if let Err(error) = repo.add_worktree(&worktree, "HEAD") {
        let detail = error.to_string();
        lane.ctx
            .log(&format!("lane ramdisk worktree failed: {detail}"));
        drop(guard);
        return undecided_all(jobs, "ramdisk-worktree-failed", &detail);
    }
    let mounted_lane = LaneWorker {
        n: lane.n,
        base: lane.base.clone(),
        ctx: lane.ctx.with_repo(worktree),
        ram: None,
    };
    let results = run(&mounted_lane, jobs);
    drop(guard);
    results
}

/// The volume name of this lane's RAM disk: `vivi-<mailspace|merge>-lane-
/// <target>-<n>`. A lane handles one batch at a time, so it can reuse this
/// recognizable name after the previous disk has been detached.
fn lane_volume(lane: &LaneWorker) -> String {
    let scope = if lane.ctx.mailspace.is_empty() {
        "merge"
    } else {
        lane.ctx.mailspace.as_str()
    };
    crate::pool::ramdisk::volume_name(&[
        scope,
        "lane",
        lane.ctx.target_id.as_str(),
        &lane.n.to_string(),
    ])
}

/// A distinct mount point for each lane identity under the system temp root.
fn lane_mount(volume: &str) -> std::path::PathBuf {
    std::env::temp_dir().join("vivi-ramdisk").join(volume)
}

/// RAII for one RAM-backed worktree. This guard also runs when `run_batch`
/// panics and `lane_loop` catches the unwind.
struct LaneRamGuard<'a> {
    ctx: &'a Ctx,
    runner: &'a dyn crate::pool::ramdisk::Runner,
    repo: crate::pool::local_git::Repo,
    worktree: std::path::PathBuf,
    disk: crate::pool::ramdisk::Disk,
}

impl<'a> LaneRamGuard<'a> {
    fn new(
        lane: &'a LaneWorker,
        ram: &'a LaneRam,
        repo: crate::pool::local_git::Repo,
        worktree: std::path::PathBuf,
        disk: crate::pool::ramdisk::Disk,
    ) -> Self {
        Self {
            ctx: &lane.ctx,
            runner: &*ram.runner,
            repo,
            worktree,
            disk,
        }
    }
}

impl Drop for LaneRamGuard<'_> {
    fn drop(&mut self) {
        if let Err(error) = self.repo.remove_worktree(&self.worktree) {
            self.ctx.log(&format!(
                "WARNING lane ramdisk {} worktree removal failed: {error}",
                self.disk.volume
            ));
        }
        match crate::pool::ramdisk::teardown(self.runner, &self.disk) {
            Ok(()) => self.ctx.log(&format!(
                "ramdisk {} detached ({} GiB released)",
                self.disk.volume, self.disk.size_gib
            )),
            Err(error) => self.ctx.log(&format!(
                "WARNING lane ramdisk {} teardown verification failed: {error}",
                self.disk.volume
            )),
        }
    }
}

/// One undecided result per job: `ok: None` keeps the unit `Delivered`, so
/// the pump re-feeds it later.
fn undecided_all(jobs: &[MergeJob], why: &str, detail: &str) -> Vec<MergeResult> {
    jobs.iter()
        .map(|j| undecided_result(&j.uid, why, detail))
        .collect()
}

/// A lane worker for tests: no thread, no channel, the caller picks the RAM
/// request.
#[cfg(test)]
pub(crate) fn detached_worker(base: &Ctx, n: u32, ram: Option<LaneRam>) -> LaneWorker {
    LaneWorker {
        n,
        base: base.clone(),
        ctx: base.clone(),
        ram,
    }
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
    lane.step(&format!("clone start: {}", lane.repo.display()));
    let started = lane.now();
    let out = base.sh(&cmd, None, 900);
    if out.code != 0 {
        return Err(tail(&out.text, 300));
    }
    lane.log(&format!(
        "clone done {}s",
        lane.now().saturating_sub(started)
    ));
    // A local-worktree target keeps its unit branches in the base repository
    // only; the clone fetches them from there as `pump-base`.
    if !base.cfg.targets.is_empty() && !ensure_base_remote(lane, &base.repo) {
        lane.log(&format!(
            "cannot add remote {BASE_REMOTE} to {}",
            lane.repo.display()
        ));
    }
    Ok(())
}

/// Offer the batch to the remote merger, else merge here: [`batch_work`]
/// for more than one job, [`merge_work`] for exactly one, then the
/// `batch of <n>: <m> merged` log line (lane-tagged by the lane context).
fn run_batch(lane: &LaneWorker, jobs: &[MergeJob]) -> Vec<MergeResult> {
    let ctx = &lane.ctx;
    let done = match remote::offer(ctx, lane.n, jobs) {
        Some(results) => results,
        None => match prepare_lane(ctx) {
            Err(failure) => {
                let name = format!("{}/{}", ctx.target_id, lane.n);
                ctx.exception("lane-reset-failed", &name, &failure);
                jobs.iter()
                    .map(|job| undecided_result(&job.uid, "lane-reset-failed", &failure))
                    .collect()
            }
            Ok(()) if jobs.len() > 1 => batch_work(ctx, jobs),
            Ok(()) => vec![merge_work(ctx, &jobs[0])],
        },
    };
    let merged = done.iter().filter(|r| r.ok == Some(true)).count();
    ctx.log(&format!("batch of {}: {merged} merged", jobs.len()));
    done
}

/// Send one result per job, results deduplicated by uid, and every job the
/// batch's results never named gets an `ok=None, why="merge-undecided"`
/// result. A result for a uid that is not in `jobs` (a remote merger mixing
/// up batches) is dropped and logged: that unit belongs to another lane, and
/// forwarding it would apply it twice.
pub(crate) fn dispatch_results(
    ctx: &Ctx,
    out: &Sender<MergeResult>,
    jobs: &[MergeJob],
    done: Vec<MergeResult>,
) {
    let mut seen: Vec<String> = Vec::new();
    for result in done {
        if !jobs.iter().any(|job| job.uid == result.uid) {
            ctx.log(&format!(
                "dropped a result for {}: it is not in this batch",
                result.uid
            ));
            continue;
        }
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
