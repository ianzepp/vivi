//! The `worker` host: a machine reached over SSH that builds one bounded
//! workspace per job.
//!
//! A worker offers capacity, not job types: memory for RAM disks, disk for
//! quota-limited disk images, plain directories, within the budgets of its
//! own `worker.conf`. The pool names the boundary its jobs need
//! (`[pools.<name>.worker] kind` and `size_gib`). A dispatch makes the slot:
//! the driver checks the request against the worker's budgets, builds the
//! boundary, unpacks the slot and seeds it; collecting the result destroys
//! it, and the boundary goes with it. A request the worker cannot hold now
//! is refused as "no free slot", so the pump tries the next pool.
//!
//! The slot verbs are those of the `ssh` provider, spoken by the worker
//! driver (`worker-macos.sh`), so every call, claim and listing goes through
//! [`SshProvider`]. Design: `docs/worker-driver-protocol.md`.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use super::config::PoolConfig;
use super::docker_cli::Runner;
use super::doctor::Check;
use super::provider::{
    CreateSpec, ExecOutput, Hold, MachineRecord, OpenRequest, OpenedSlot, PoolProvider,
    SlotLifetime,
};
use super::run::ONCE_ENV;
use super::slot::{META_MAILSPACE, META_POOL, META_SLOT, META_STATE, STATE_READY};
use super::ssh_state;
use super::ssh::SshProvider;
use super::ssh_cli::SshCli;
use super::ssh_script::WORKER_MACOS_SCRIPT;
use crate::error::VivariumError;

/// How long a new slot's seed (fetch and seed commands) may take.
const SEED_LIMIT: Duration = Duration::from_secs(30 * 60);
/// How often a seeding slot is looked at.
const SEED_POLL: Duration = Duration::from_secs(2);

/// Pool provider backed by a worker driver over SSH.
pub struct WorkerProvider {
    ssh: SshProvider,
    host: String,
    slots: u32,
    request: String,
    seed_limit: Duration,
    seed_poll: Duration,
}

impl WorkerProvider {
    #[must_use]
    pub fn new(config: &PoolConfig, ctx: &super::runpod::HostContext) -> Self {
        let host = config.worker.host.clone().unwrap_or_default();
        Self::with_runner(
            config,
            &ctx.mailspace,
            &ctx.pool,
            Box::new(SshCli::new(&host)),
        )
    }

    /// A provider that talks through `runner` (tests); the runner gets the
    /// remote command line as its one argument. It is bound to the mailspace
    /// only, not to `_pool`: one provider serves its pool and that pool's
    /// `-doctor` test pool, and every caller tells slots apart by their
    /// pool mark.
    #[must_use]
    pub fn with_runner(
        config: &PoolConfig,
        mailspace: &str,
        _pool: &str,
        runner: Box<dyn Runner>,
    ) -> Self {
        let mut through = config.clone();
        through.ssh.host.clone_from(&config.worker.host);
        through.ssh.root = Some(config.worker.root_or_default().to_string());
        let request = config.worker.request();
        Self {
            ssh: SshProvider::with_runner_for_pool(&through, mailspace, "", runner)
                .with_script(WORKER_MACOS_SCRIPT, Some(request.clone())),
            host: config.worker.host.clone().unwrap_or_default(),
            slots: config.slots,
            request,
            seed_limit: SEED_LIMIT,
            seed_poll: SEED_POLL,
        }
    }

    /// The same provider with a shorter seed wait and poll (tests).
    #[must_use]
    pub fn with_seed_pacing(mut self, limit: Duration, poll: Duration) -> Self {
        self.seed_limit = limit;
        self.seed_poll = poll;
        self
    }

    /// The slot indices of `pool` that hold no listed slot, lowest first, or
    /// why the pool is full. A slot being built is not listed yet, so the
    /// driver's `create` is what reserves an index; this only proposes them.
    fn free_indices(&self, pool: &str) -> Result<Vec<u32>, VivariumError> {
        let held: Vec<MachineRecord> = self
            .ssh
            .list()?
            .into_iter()
            .filter(|m| m.metadata.get(META_POOL).is_some_and(|p| p == pool))
            .collect();
        // A slot whose index mark is missing or not a number is a fault on
        // the worker; refuse rather than guess which index it holds.
        let used = held
            .iter()
            .map(|m| {
                let unreadable = || {
                    VivariumError::Message(format!(
                        "slot {} on {} has no readable slot index mark",
                        m.id, self.host
                    ))
                };
                let mark = m.metadata.get(META_SLOT).ok_or_else(unreadable)?;
                mark.parse::<u32>().map_err(|_| unreadable())
            })
            .collect::<Result<Vec<u32>, _>>()?;
        let free: Vec<u32> = (1..=self.slots).filter(|i| !used.contains(i)).collect();
        if free.is_empty() {
            return Err(self.pool_full(pool, held.len()));
        }
        Ok(free)
    }

    fn pool_full(&self, pool: &str, held: usize) -> VivariumError {
        VivariumError::Message(format!(
            "no free slot in pool {pool}: it allows {} slot(s) at once on {} and holds {held}",
            self.slots, self.host
        ))
    }

    /// Make, seed and claim the slot of one job. The driver reserves the
    /// index atomically under its lock: a `create` that finds the index
    /// taken (another dispatch, or a leftover) fails as "slot exists" and the
    /// next free index is tried. A dispatch that fails after its index was
    /// reserved destroys that slot, so a failed dispatch never keeps capacity.
    fn open(&self, request: &OpenRequest) -> Result<OpenedSlot, VivariumError> {
        let pool = &request.pool.pool;
        for index in self.free_indices(pool)? {
            let mut spec = request.spec.clone();
            spec.metadata
                .insert(META_SLOT.to_string(), index.to_string());
            spec.name = format!("{}-{pool}-slot-{index}", request.pool.mailspace);
            request.progress.say(&format!(
                "{pool}: building slot {index} on {} ({})",
                self.host, self.request
            ));
            let id = match self.ssh.create(&spec) {
                Ok(id) => id,
                Err(e) if slot_taken(&e) => continue,
                Err(e) if refused_before_building(&e) => return Err(e),
                Err(e) => return Err(self.torn_down(&slot_id_of_spec(&spec, index), e)),
            };
            return match self.ready(&id, request) {
                Ok(hold) => Ok(OpenedSlot {
                    id,
                    name: spec.name,
                    index,
                    hold,
                    hardware: format!("{} {}", self.host, self.request),
                    created_at: chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
                    abandoned: Vec::new(),
                }),
                Err(e) => Err(self.torn_down(&id, e)),
            };
        }
        Err(self.pool_full(pool, self.slots as usize))
    }

    /// `error` with slot `id` torn down after it. A teardown that fails is
    /// part of the error: the slot may still hold capacity on the worker.
    fn torn_down(&self, id: &str, error: VivariumError) -> VivariumError {
        match self.ssh.destroy(id) {
            Ok(()) => error,
            Err(undo) => VivariumError::Message(format!(
                "{error}; AND the teardown of slot {id} on {} failed: {undo}; it may still hold capacity, remove it with `vivi pool destroy {id}`",
                self.host
            )),
        }
    }

    /// Wait for the seed of slot `id`, mark it ready and claim it.
    fn ready(&self, id: &str, request: &OpenRequest) -> Result<Hold, VivariumError> {
        request.progress.say("seeding the slot");
        let started = Instant::now();
        loop {
            let seen = self.ssh.machine(id)?;
            if seen.state == "stopped" {
                if seen.exit_code != Some(0) {
                    return Err(self.seed_failure(id, seen.exit_code));
                }
                break;
            }
            if started.elapsed() >= self.seed_limit {
                return Err(VivariumError::Message(format!(
                    "the seed of slot {id} on {} did not finish within {}s",
                    self.host,
                    self.seed_limit.as_secs()
                )));
            }
            std::thread::sleep(self.seed_poll);
        }
        self.ssh.seal(id, &ONCE_ENV, META_STATE, STATE_READY)?;
        self.ssh
            .claim(id, request.task, request.now)?
            .ok_or_else(|| {
                VivariumError::Message(format!("slot {id} would not be claimed after its seed"))
            })
    }

    /// Why the seed failed, from the tail of the slot's boot log.
    fn seed_failure(&self, id: &str, code: Option<i64>) -> VivariumError {
        let tail = self
            .ssh
            .exec(id, "tail -n 3 \"$WORK/boot.log\" 2>/dev/null", 20)
            .map_or_else(|_| String::new(), |o| o.stdout.trim().replace('\n', "; "));
        let code = code.map_or_else(|| "unknown".to_string(), |c| c.to_string());
        VivariumError::Message(format!(
            "the seed of slot {id} on {} failed (exit {code}): {tail}",
            self.host
        ))
    }
}

/// The driver's id of the slot `spec` builds at `index`.
fn slot_id_of_spec(spec: &CreateSpec, index: u32) -> String {
    let field = |key: &str| spec.metadata.get(key).map_or("", String::as_str);
    ssh_state::slot_id(field(META_MAILSPACE), field(META_POOL), &index.to_string())
}

/// The driver refused to build because the index is already held. The slot
/// is someone else's, so it must not be torn down.
fn slot_taken(error: &VivariumError) -> bool {
    error.to_string().contains("slot exists: ")
}

/// The driver refused before it built anything (a budget), so there is
/// nothing to tear down.
fn refused_before_building(error: &VivariumError) -> bool {
    error.to_string().contains("no free slot: ")
}

impl PoolProvider for WorkerProvider {
    fn slot_lifetime(&self) -> SlotLifetime {
        SlotLifetime::PerDispatch
    }

    fn open_for_dispatch(&self, request: &OpenRequest) -> Result<OpenedSlot, VivariumError> {
        self.open(request)
    }

    fn setup_grace_secs(&self) -> u64 {
        self.seed_limit.as_secs() + 60
    }

    fn billing_note(&self) -> String {
        format!("held on {} until it is destroyed", self.host)
    }

    /// The worker root is made by the first slot; nothing to make beforehand.
    fn ensure_home(&self) -> Result<Option<String>, VivariumError> {
        Ok(None)
    }

    fn doctor(&self, _harness_env: &[String]) -> Vec<Check> {
        const GROUP: &str = "worker";
        match self.ssh.probe() {
            Err(e) => vec![Check::fail(
                GROUP,
                "host",
                format!("cannot run the worker driver on {}: {e}", self.host),
                format!(
                    "check that `ssh -o BatchMode=yes {} true` succeeds without a prompt",
                    self.host
                ),
            )],
            Ok(probe) if !probe.missing.is_empty() => vec![Check::fail(
                GROUP,
                "tools",
                format!("missing on {}: {}", self.host, probe.missing.join(", ")),
                "the macOS worker driver needs git, tar, perl, lsof, diskutil, hdiutil and plutil",
            )],
            Ok(probe) => vec![Check::ok(GROUP, "host", probe.system)],
        }
    }

    fn harness_env_gaps(&self, names: &[String]) -> Result<Vec<String>, VivariumError> {
        self.ssh.harness_env_gaps(names)
    }

    fn env_gaps_at_dispatch(&self, names: &[String]) -> Result<Vec<String>, VivariumError> {
        self.ssh.env_gaps_at_dispatch(names)
    }

    fn env_gap_hint(&self, missing: &[String]) -> String {
        self.ssh.env_gap_hint(missing)
    }

    fn live_test_notice(&self) -> String {
        format!(
            "it builds two {} slots on {}, runs stand-in jobs in them as the login user, and tears them down afterwards (Ctrl-C stops it and cleans up)",
            self.request, self.host
        )
    }

    fn supplies_build_mount(&self) -> bool {
        false
    }

    fn build_persists(&self) -> bool {
        true
    }

    fn runs_unprivileged(&self) -> bool {
        true
    }

    fn reclaims_dead_slots(&self) -> bool {
        true
    }

    fn list(&self) -> Result<Vec<MachineRecord>, VivariumError> {
        self.ssh.list()
    }

    /// A worker makes a slot per dispatch; there is nothing to create ahead.
    fn create(&self, _spec: &CreateSpec) -> Result<String, VivariumError> {
        Err(VivariumError::Message(
            "a worker pool makes one slot per dispatch; `create` does not apply".into(),
        ))
    }

    fn destroy(&self, id: &str) -> Result<(), VivariumError> {
        self.ssh.destroy(id)
    }

    fn destroy_command(&self, id: &str) -> String {
        format!(
            "vivi pool destroy {id}   (it stops the slot's processes and removes its boundary on {})",
            self.host
        )
    }

    fn machine(&self, id: &str) -> Result<MachineRecord, VivariumError> {
        self.ssh.machine(id)
    }

    fn claim(&self, id: &str, task: &str, now: u64) -> Result<Option<Hold>, VivariumError> {
        self.ssh.claim(id, task, now)
    }

    fn unclaim(&self, id: &str) -> Result<(), VivariumError> {
        self.ssh.unclaim(id)
    }

    fn release(&self, id: &str, hold: &Hold) -> Result<(), VivariumError> {
        self.ssh.release(id, hold)
    }

    fn put_file(
        &self,
        id: &str,
        hold: &Hold,
        path: &str,
        content: &[u8],
    ) -> Result<(), VivariumError> {
        self.ssh.put_file(id, hold, path, content)
    }

    fn seal(
        &self,
        id: &str,
        drop_env: &[&str],
        key: &str,
        value: &str,
    ) -> Result<(), VivariumError> {
        self.ssh.seal(id, drop_env, key, value)
    }

    fn start(&self, id: &str) -> Result<(), VivariumError> {
        self.ssh.start(id)
    }

    fn stop(&self, id: &str) -> Result<(), VivariumError> {
        self.ssh.stop(id)
    }

    fn exec(&self, id: &str, script: &str, timeout_secs: u32) -> Result<ExecOutput, VivariumError> {
        self.ssh.exec(id, script, timeout_secs)
    }

    fn read_stopped(
        &self,
        id: &str,
        paths: &[&str],
    ) -> Result<Option<BTreeMap<String, Vec<u8>>>, VivariumError> {
        self.ssh.read_stopped(id, paths)
    }
}

#[cfg(test)]
#[path = "worker_test.rs"]
mod tests;
