//! The provider seam: what the pool needs from a machine host.

use std::collections::BTreeMap;

use super::collect::STALE_CLAIM_SECS;
use super::doctor::Check;
use super::progress::Progress;
use super::slot::PoolId;
use crate::error::VivariumError;

/// The proof of an exclusive claim: what a provider hands back when a claim
/// succeeds, and takes back to deliver the job and to end the claim window.
/// What it holds is the provider's own business (a lease nonce, the id of a
/// claim marker).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hold {
    pub nonce: String,
}

/// What a command run inside a machine produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i64,
}

/// One machine as the provider reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineRecord {
    pub id: String,
    pub name: String,
    /// Provider state word, e.g. `started`, `stopped`.
    pub state: String,
    pub region: String,
    pub created_at: String,
    pub metadata: BTreeMap<String, String>,
    /// Exit code of the machine's most recent process exit, when known.
    pub exit_code: Option<i64>,
}

/// A provider-prepared per-job repository cache and, for a local worktree,
/// the source repository owning its checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedWorkspace {
    pub cache_key: String,
    pub local_source: Option<String>,
}

impl MachineRecord {
    /// What the provider's answer for a machine that no longer exists reads
    /// as: state `destroyed`, nothing else known.
    #[must_use]
    pub fn gone(id: &str) -> Self {
        Self {
            id: id.to_string(),
            name: String::new(),
            state: "destroyed".to_string(),
            region: String::new(),
            created_at: String::new(),
            metadata: BTreeMap::new(),
            exit_code: None,
        }
    }
}

/// Everything needed to create and start one slot. Settings that belong to
/// one machine host (size, region, memory limits) are the host's own and are
/// not here.
#[derive(Clone)]
pub struct CreateSpec {
    pub name: String,
    pub image: String,
    /// Fixed facts about the slot (pool, index, base). A host may add its own.
    pub metadata: BTreeMap<String, String>,
    /// Environment for the machine. Holds no secret: it may be printed in a
    /// command line.
    pub env: BTreeMap<String, String>,
    /// Environment the seed needs once (the fetch token). May carry a secret:
    /// never print it. The host must not leave it on the machine once
    /// [`PoolProvider::seal`] has run.
    pub once_env: BTreeMap<String, String>,
    /// Names the machine's environment gets from the environment of the
    /// process that creates it (the harness keys). Names only; a name that is
    /// unset there is left out. Hosts with their own secret store ignore it.
    pub passthrough_env: Vec<String>,
    /// `(guest path, contents)` files written into the machine.
    pub files: Vec<(String, String)>,
    /// Command the machine runs on every boot.
    pub command: Vec<String>,
    /// Build output directory relative to the checkout (none: the pool has no
    /// build directory handling), and the size of the memory disk over it.
    /// Used by hosts that mount it themselves (see
    /// [`PoolProvider::supplies_build_mount`]).
    pub build_dir: Option<String>,
    pub tmpfs: String,
}

impl std::fmt::Debug for CreateSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreateSpec")
            .field("name", &self.name)
            .field("image", &self.image)
            .field("metadata", &self.metadata)
            .field("env_keys", &self.env.keys().collect::<Vec<_>>())
            .field("once_env_keys", &self.once_env.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

/// How a host's slots live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotLifetime {
    /// Slots exist between jobs: `create` seeds them, a dispatch claims a free
    /// one, and freeing it stops it (Fly, Docker, SSH, local).
    Resting,
    /// A slot exists only for one job: a dispatch makes it with
    /// [`PoolProvider::open_for_dispatch`], and collecting its result destroys
    /// it. `slots` is the most that may exist at once. Nothing rests, so
    /// `create` and a scale-up do not apply, and an uncollected slot is a bill
    /// that grows until it is destroyed.
    PerDispatch,
}

/// What a per-dispatch host needs to make the slot of one job.
pub struct OpenRequest<'a> {
    pub pool: &'a PoolId,
    /// The task the slot is for; it is part of the slot's mark on the host.
    pub task: &'a str,
    /// Dispatch time, epoch seconds.
    pub now: u64,
    /// The job's time budget in seconds.
    pub budget_secs: u64,
    /// What the slot is made from, as for a resting slot (the slot index in
    /// its metadata is the host's to choose).
    pub spec: CreateSpec,
    /// Where the steps of making the slot are reported as they start.
    pub progress: &'a Progress,
    /// Seconds a new machine may take to become reachable, for this dispatch
    /// only (none: the pool's own setting). Only a host that makes machines
    /// per dispatch has such a wait.
    pub ready_timeout_secs: Option<u64>,
}

/// A machine a per-dispatch host made and terminated again because it did not
/// become reachable in time. It billed while it existed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Abandoned {
    pub id: String,
    /// The hardware type that was being tried.
    pub gpu: String,
    /// Seconds from the start of the create to the machine's confirmed absence.
    pub waited_secs: u64,
    /// Approximate charge in millionths of a dollar (hourly price times the
    /// time waited); none when the host did not say the price.
    pub cost_micros: Option<u64>,
}

/// The slot a per-dispatch host made: running, seeded, and claimed by the
/// task, ready for the job archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenedSlot {
    /// The machine id (what every later call names).
    pub id: String,
    /// The slot's display name, `<mailspace>-<pool>-slot-<index>`.
    pub name: String,
    pub index: u32,
    pub hold: Hold,
    /// What the host gave the slot, for the dispatch record (for example the
    /// hardware it runs on). Empty when the host has nothing to say.
    pub hardware: String,
    pub created_at: String,
    /// Machines made and terminated before this one because they did not
    /// become reachable, in the order they were tried.
    pub abandoned: Vec<Abandoned>,
}

/// A machine host. Implementations must keep secrets out of errors.
/// Methods after `destroy` act on one machine by id.
pub trait PoolProvider {
    /// Resolved root a local slot id is joined to. Directory providers use
    /// this so commands act on the same site the provider opened.
    fn local_root(&self) -> Option<std::path::PathBuf> {
        None
    }

    /// Resolved directory containing this provider's slots. A clean sweep
    /// uses it as the boundary for orphan worktrees.
    fn local_pool_dir(&self) -> Option<std::path::PathBuf> {
        None
    }

    /// RAM-disk settings copied into the job bundle when this provider mounts
    /// RAM for each dispatched job. Empty on hosts without local RAM disks.
    fn ramdisk_job_settings(&self) -> Option<(u64, Vec<(String, String)>)> {
        None
    }

    /// Prepare one job's repository, when it has a target, and any provider
    /// resources needed before the job archive is delivered.
    fn prepare_job(
        &self,
        id: &str,
        target: Option<&super::dispatch::target::JobTarget>,
        start: &str,
        job: &str,
    ) -> Result<Option<PreparedWorkspace>, VivariumError> {
        match target {
            Some(target) => self
                .prepare_target_checkout(id, target, start, job)
                .map(|()| None),
            None => Ok(None),
        }
    }

    /// Prepare a target-specific local worktree after the shared slot is claimed.
    fn prepare_target_checkout(
        &self,
        _id: &str,
        target: &super::dispatch::target::JobTarget,
        _start: &str,
        _job: &str,
    ) -> Result<(), VivariumError> {
        if target.local_source.is_some() {
            return Err(VivariumError::Config(
                "local repository targets require a local provider".into(),
            ));
        }
        Ok(())
    }

    /// How this host's slots live. Resting unless the host says otherwise.
    fn slot_lifetime(&self) -> SlotLifetime {
        SlotLifetime::Resting
    }

    /// Make, seed and claim the slot of one job (per-dispatch hosts only).
    /// The machine is running when this returns; on failure nothing it made
    /// is left behind.
    ///
    /// # Errors
    /// Returns an error if no slot can be made (naming why) or the host fails.
    fn open_for_dispatch(&self, _request: &OpenRequest) -> Result<OpenedSlot, VivariumError> {
        Err(VivariumError::Message(
            "this host keeps resting slots; there is nothing to make per dispatch".into(),
        ))
    }

    /// Seconds a claimed slot may exist with no dispatch recorded before it
    /// counts as stale: the longest a dispatch spends between claiming a
    /// slot and recording it.
    fn setup_grace_secs(&self) -> u64 {
        STALE_CLAIM_SECS
    }

    /// How a machine that runs with nothing to explain it is costing money,
    /// as the doctor's leak finding words it.
    fn billing_note(&self) -> String {
        "billed by the second on Fly".to_string()
    }

    /// When the live test (`doctor --test`) bills, the plain-words reason a
    /// caller must pass `--allow-spend` to run it, with the price when the
    /// host can read it. `None`: the test needs no such flag.
    fn live_test_spend_gate(&self) -> Option<String> {
        None
    }

    /// Make sure the pool's home on the host exists (a Fly application; a
    /// Docker daemon needs none). `Some(text)` when this call created it,
    /// saying what was made.
    ///
    /// # Errors
    /// Returns an error if the host cannot list or create it.
    fn ensure_home(&self) -> Result<Option<String>, VivariumError>;

    /// The host's own health checks for `vivi pool doctor`: read-only, under
    /// the host's own heading, each failure naming its fix. The harness
    /// environment names are the ones the machines must receive.
    fn doctor(&self, harness_env: &[String]) -> Vec<Check>;

    /// Which of the harness environment `names` the host has no value for
    /// (names only; no value is read). A Fly app holds them as secrets; a
    /// Docker slot takes them from the creating process's environment.
    ///
    /// # Errors
    /// Returns an error if the host cannot be asked.
    fn harness_env_gaps(&self, names: &[String]) -> Result<Vec<String>, VivariumError>;

    /// Like [`PoolProvider::harness_env_gaps`], for a dispatch to slots that
    /// already exist. A host whose slots fixed their environment when they
    /// were created has nothing to report here.
    ///
    /// # Errors
    /// Returns an error if the host cannot be asked.
    fn env_gaps_at_dispatch(&self, names: &[String]) -> Result<Vec<String>, VivariumError>;

    /// What to tell the user about `missing` harness environment names:
    /// the names and the way to supply them, never a value.
    fn env_gap_hint(&self, missing: &[String]) -> String;

    /// A line for the progress output of the live test, saying what it costs
    /// on this host.
    fn live_test_notice(&self) -> String;

    /// True when the host mounts the memory disk over the build directory
    /// itself (from [`CreateSpec::build_dir`]), because the boot script
    /// cannot. The boot script then skips its own mount.
    fn supplies_build_mount(&self) -> bool;

    /// True when the build directory is a plain directory that keeps its
    /// contents between boots (a slot that is a directory on a host). The
    /// boot script then mounts no memory disk and neither packs nor restores
    /// the build output: a seeded slot simply keeps its warm build.
    fn build_persists(&self) -> bool;

    /// True when the slot's scripts run as an ordinary user on a host they do
    /// not own. They then create no user, install nothing, mount nothing and
    /// change no ownership; a missing tool is a failure to report.
    fn runs_unprivileged(&self) -> bool;

    /// All machines of the pool's home, pool members or not.
    ///
    /// # Errors
    /// Returns an error if the host cannot be reached or answers badly.
    fn list(&self) -> Result<Vec<MachineRecord>, VivariumError>;

    /// Create and start one machine; returns its id without waiting for the
    /// machine to finish booting.
    ///
    /// # Errors
    /// Returns an error if the machine could not be created.
    fn create(&self, spec: &CreateSpec) -> Result<String, VivariumError>;

    /// Destroy one machine by id, whatever its state.
    ///
    /// # Errors
    /// Returns an error if the machine could not be destroyed.
    fn destroy(&self, id: &str) -> Result<(), VivariumError>;

    /// The shell command that removes one machine by hand, for a report that
    /// cannot remove it itself.
    fn destroy_command(&self, id: &str) -> String;

    /// One machine as the host reports it right now. A machine that no
    /// longer exists is not an error: it reads as [`MachineRecord::gone`]
    /// (state `destroyed`), so a dispatch whose machine vanished can be closed.
    ///
    /// # Errors
    /// Returns an error if the host fails.
    fn machine(&self, id: &str) -> Result<MachineRecord, VivariumError>;

    /// Claim the machine for `task`: atomically take it and leave the task
    /// and claim time as durable marks (`META_TASK`, `META_CLAIMED`) that
    /// outlive the returned [`Hold`]. `Ok(None)` means another claimant has
    /// it, or it is no longer free. Two claimants, in this process or
    /// another, never both get `Some`.
    ///
    /// # Errors
    /// Returns an error if the host fails for any other reason; a half-made
    /// claim is undone first.
    fn claim(&self, id: &str, task: &str, now: u64) -> Result<Option<Hold>, VivariumError>;

    /// Drop the claim marks. Absent marks are not an error.
    ///
    /// # Errors
    /// Returns an error if the host refuses.
    fn unclaim(&self, id: &str) -> Result<(), VivariumError>;

    /// End the claim window of `hold`; the marks stay.
    ///
    /// # Errors
    /// Returns an error if the host refuses the release.
    fn release(&self, id: &str, hold: &Hold) -> Result<(), VivariumError>;

    /// Put a file into the stopped machine (it stays stopped), holding
    /// `hold`. The file is there the next time the machine starts.
    ///
    /// # Errors
    /// Returns an error if the update is refused.
    fn put_file(
        &self,
        id: &str,
        hold: &Hold,
        path: &str,
        content: &[u8],
    ) -> Result<(), VivariumError>;

    /// Of a stopped machine (it stays stopped): drop the environment names
    /// that carried one-time secrets (a host that never put them in the
    /// machine's configuration has nothing to drop) and set one mark.
    ///
    /// # Errors
    /// Returns an error if the update is refused.
    fn seal(
        &self,
        id: &str,
        drop_env: &[&str],
        key: &str,
        value: &str,
    ) -> Result<(), VivariumError>;

    /// Start a stopped machine; returns once the host accepted it.
    ///
    /// # Errors
    /// Returns an error if the machine cannot be started.
    fn start(&self, id: &str) -> Result<(), VivariumError>;

    /// Start a machine while retaining the claim that protects delivery.
    /// Providers with machine leases can attach the hold to the start request;
    /// providers whose claim is independent of the host start use `start`.
    ///
    /// # Errors
    /// Returns an error if the machine cannot be started.
    fn start_claimed(&self, id: &str, _hold: &Hold) -> Result<(), VivariumError> {
        self.start(id)
    }

    /// Stop a machine while retaining the claim that protects the operation.
    /// Providers with machine leases can attach the hold to the stop request.
    ///
    /// # Errors
    /// Returns an error if the machine cannot be stopped.
    fn stop_claimed(&self, id: &str, _hold: &Hold) -> Result<(), VivariumError> {
        self.stop(id)
    }

    /// Stop a machine; returns once the host accepted it.
    ///
    /// # Errors
    /// Returns an error if the machine cannot be stopped.
    fn stop(&self, id: &str) -> Result<(), VivariumError>;

    /// Run `script` under `bash -c` inside the running machine.
    ///
    /// # Errors
    /// Returns an error if the machine is not running or the call fails.
    fn exec(&self, id: &str, script: &str, timeout_secs: u32) -> Result<ExecOutput, VivariumError>;

    /// Read files out of a stopped machine without starting it: the files at
    /// `paths` that exist, keyed by path. `Ok(None)` means this host cannot,
    /// and the caller starts the machine and uses [`PoolProvider::exec`].
    ///
    /// # Errors
    /// Returns an error if the host fails.
    fn read_stopped(
        &self,
        id: &str,
        paths: &[&str],
    ) -> Result<Option<BTreeMap<String, Vec<u8>>>, VivariumError>;
}
