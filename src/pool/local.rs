//! The machine you are on as the pool provider: a slot is a directory under a
//! root, and the worker is a process `vivi` launches itself, as the current
//! user. No rented machine, no container, no `ssh`.
//!
//! This gives no isolation at all. The slot's scripts and the worker have the
//! whole account of the user who runs `vivi`: every file that user can read or
//! write, and every credential in their environment and home directory. Use
//! it for work you would run in a terminal yourself.
//!
//! - A slot's facts are files in its directory (see [`super::local_slots`]);
//!   nothing is kept anywhere else.
//! - "Running" is a live boot process, told by process id and start time;
//!   stopping ends everything the slot started (see [`super::local_proc`]).
//!   It works on macOS and on Linux.
//! - The build directory, when the pool has one, is a plain directory that
//!   persists, so a fixed pool of N slots holds at most N build directories.
//! - Harness environment values are never stored: the boot process inherits
//!   the environment of the `vivi` process that starts it.
//! - Two source modes. A slot either fetches like an `ssh` slot, or its
//!   checkout is a Git worktree of a repository on this machine (the
//!   `WORKTREE_OF` environment value of the slot): then the result of a job is
//!   a commit on its branch in that repository, and a slot at rest is on a
//!   detached HEAD so no branch is held (see [`super::local_branch`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use super::config::PoolConfig;
use super::config::config_ramdisk::PoolRamDisk;
use super::docker::{env_is_set, unset_names};
use super::docker_cli::{RunOutput, finish};
use super::doctor::Check;
use super::local_proc as proc;
use super::local_slots as slots;
use super::provider::{CreateSpec, ExecOutput, Hold, MachineRecord, PoolProvider};
use super::slot::{META_MAILSPACE, META_POOL, META_SLOT};
use super::ssh_state as state;
use crate::error::VivariumError;
use sha2::{Digest, Sha256};

/// Pool provider for this machine.
pub struct LocalProvider {
    site: Result<super::local_site::LocalSite, String>,
    root: PathBuf,
    /// The repository the pool's slots are worktrees of, as configured; the
    /// doctor looks at it. The slots themselves carry what they need.
    source: Option<PathBuf>,
    build_dir: Option<String>,
    /// The pool's `build_baseline_gib` in bytes, for reclaim of a slot that
    /// carries no recorded baseline.
    build_baseline: Option<u64>,
    ramdisk: PoolRamDisk,
    ramdisk_runner: Arc<dyn crate::pool::ramdisk::Runner>,
    #[cfg(test)]
    available_memory: Option<u64>,
}

impl LocalProvider {
    #[must_use]
    pub fn new(config: &PoolConfig, ctx: &super::runpod::HostContext) -> Self {
        let raw = config.local.root_or_default();
        let site = super::local_site::resolve(
            raw,
            ctx.dir.parent().unwrap_or(ctx.dir.as_path()),
            &ctx.mailspace,
            &ctx.pool,
        )
        .map_err(|error| error.to_string());
        Self {
            root: site
                .as_ref()
                .map_or_else(|_| slots::expand_root(raw), |site| site.root.clone()),
            site,
            source: config.local.worktree_path(),
            build_dir: config.build_dir.clone(),
            build_baseline: config.build_baseline_bytes(),
            ramdisk: config.ramdisk.clone(),
            ramdisk_runner: Arc::new(crate::pool::ramdisk::RealRunner),
            #[cfg(test)]
            available_memory: None,
        }
    }

    fn site(&self) -> Result<&super::local_site::LocalSite, VivariumError> {
        self.site
            .as_ref()
            .map_err(|message| VivariumError::Config(message.clone()))
    }

    #[cfg(test)]
    pub(crate) fn with_runner(
        config: &PoolConfig,
        ctx: &super::runpod::HostContext,
        runner: Arc<dyn crate::pool::ramdisk::Runner>,
    ) -> Self {
        let mut provider = Self::new(config, ctx);
        provider.ramdisk_runner = runner;
        provider
    }

    #[cfg(test)]
    pub(crate) fn with_available_memory(mut self, available: u64) -> Self {
        self.available_memory = Some(available);
        self
    }

    pub(super) fn root(&self) -> &std::path::Path {
        &self.root
    }

    pub(super) fn source(&self) -> Option<&std::path::Path> {
        self.source.as_deref()
    }

    /// The directory of slot `id`, which must be a slot id.
    fn dir(&self, id: &str) -> Result<PathBuf, VivariumError> {
        let site = self.site()?;
        let prefix = if site.relative {
            format!("{}/", site.pool)
        } else {
            format!("{}/{}/", site.mailspace, site.pool)
        };
        if slots::slot_id_ok(id)
            && id
                .strip_prefix(&prefix)
                .is_some_and(|slot| slot.starts_with("slot-"))
        {
            Ok(site.root.join(id))
        } else {
            Err(VivariumError::Message(format!(
                "not a slot of this pool: {id}"
            )))
        }
    }

    /// The directory of a slot that exists.
    fn known(&self, id: &str) -> Result<PathBuf, VivariumError> {
        let dir = self.dir(id)?;
        if slots::is_known(&dir) {
            Ok(dir)
        } else {
            Err(VivariumError::Message(format!("no such slot: {id}")))
        }
    }
}

/// The id a new slot gets from its spec and resolved storage site.
fn new_slot_id(
    site: &super::local_site::LocalSite,
    spec: &CreateSpec,
) -> Result<String, VivariumError> {
    let field = |key: &str| {
        spec.metadata
            .get(key)
            .ok_or_else(|| VivariumError::Message(format!("slot spec lacks {key}")))
    };
    let mailspace = field(META_MAILSPACE)?;
    let pool = field(META_POOL)?;
    if mailspace.as_str() != site.mailspace || pool.as_str() != site.pool {
        return Err(VivariumError::Message(
            "slot spec does not belong to this local pool".into(),
        ));
    }
    let id = if site.relative {
        format!("{pool}/slot-{}", field(META_SLOT)?)
    } else {
        state::slot_id(mailspace, pool, field(META_SLOT)?)
    };
    if slots::slot_id_ok(&id) {
        Ok(id)
    } else {
        Err(VivariumError::Message(format!("not a slot id: {id}")))
    }
}

pub(super) fn refuse_foreign_worktree(
    id: &str,
    dir: &Path,
    configured: &Path,
) -> Result<(), VivariumError> {
    let Ok(recorded) = std::fs::read_to_string(dir.join(slots::WORKTREE_OF_FILE)) else {
        return Ok(());
    };
    let recorded = PathBuf::from(recorded.trim());
    let recorded_real = std::fs::canonicalize(&recorded).unwrap_or_else(|_| recorded.clone());
    let configured_real =
        std::fs::canonicalize(configured).unwrap_or_else(|_| configured.to_path_buf());
    if recorded_real == configured_real {
        return Ok(());
    }
    Err(VivariumError::Message(format!(
        "slot {id} is a worktree of {}, not of this pool's repository {}; remake it from this pool's repository with `vivi pool destroy {id}` then `vivi pool create`",
        recorded.display(),
        configured.display()
    )))
}

/// Tear down the RAM disk owned by `dir`, if its marker names one exact
/// attached local-pool disk. Unknown or malformed markers are retained and
/// fail closed; teardown failures retain the marker for retry and diagnosis.
pub(super) fn teardown_ramdisk(
    dir: &Path,
    id: &str,
    runner: &dyn crate::pool::ramdisk::Runner,
) -> Result<(), VivariumError> {
    let marker_path = dir.join("ramdisk");
    let record = match read_ramdisk_record(&marker_path, dir, id)? {
        Some(record) => record,
        None => return Ok(()),
    };
    let disk = find_attached_ramdisk(&record, runner)?;
    teardown_owned_ramdisk(&marker_path, runner, &record, &disk)
}

fn read_ramdisk_record(
    marker_path: &Path,
    dir: &Path,
    id: &str,
) -> Result<Option<RamDiskRecord>, VivariumError> {
    match marker_path.symlink_metadata() {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Ok(metadata) if metadata.file_type().is_file() => {}
        _ => {
            let detail = format!(
                "malformed RAM disk marker {}; preserving it",
                marker_path.display()
            );
            eprintln!("warning: {detail}");
            return Err(VivariumError::Message(detail));
        }
    }
    let marker = std::fs::read_to_string(marker_path)?;
    let Some(record) = ramdisk_record(&marker, dir, id) else {
        eprintln!(
            "warning: malformed RAM disk marker {}; preserving it",
            marker_path.display()
        );
        return Err(VivariumError::Message(format!(
            "malformed RAM disk marker {}",
            marker_path.display()
        )));
    };
    Ok(Some(record))
}

fn find_attached_ramdisk(
    record: &RamDiskRecord,
    runner: &dyn crate::pool::ramdisk::Runner,
) -> Result<crate::pool::ramdisk::Disk, VivariumError> {
    for mut disk in crate::pool::ramdisk::mounted_checked(runner).map_err(VivariumError::Message)? {
        if disk.volume != record.volume
            || disk.mount != record.mount
            || !crate::pool::ramdisk::same_device_id(&disk.device, &record.volume_device)
        {
            continue;
        }
        let backing =
            crate::pool::ramdisk::backing_device_for_volume(runner, &disk.device, &disk.volume)
                .map_err(VivariumError::Message)?;
        if !crate::pool::ramdisk::same_device_id(&backing, &record.device) {
            continue;
        }
        disk.backing_device = Some(record.device.clone());
        return Ok(disk);
    }
    let detail = format!(
        "RAM disk {} for job {} at {} is not the exact attached device; preserving marker",
        record.volume,
        record.job,
        record.mount.display()
    );
    eprintln!("warning: {detail}");
    Err(VivariumError::Message(detail))
}

fn teardown_owned_ramdisk(
    marker_path: &Path,
    runner: &dyn crate::pool::ramdisk::Runner,
    record: &RamDiskRecord,
    disk: &crate::pool::ramdisk::Disk,
) -> Result<(), VivariumError> {
    if let Some(error) = remove_ramdisk_worktree(record) {
        eprintln!(
            "warning: RAM disk {} worktree removal failed: {error}",
            record.volume
        );
        return Err(VivariumError::Message(format!(
            "RAM disk {} cleanup incomplete: {error}",
            record.volume
        )));
    }
    if let Err(error) = crate::pool::ramdisk::teardown(runner, disk) {
        eprintln!(
            "warning: RAM disk {} teardown verification failed: {error}",
            record.volume
        );
        return Err(VivariumError::Message(error));
    }
    std::fs::remove_file(&marker_path).map_err(|error| {
        VivariumError::Message(format!(
            "RAM disk {} detached but marker removal failed: {error}",
            record.volume
        ))
    })?;
    eprintln!(
        "RAM disk {} detached ({} GiB released)",
        record.volume, record.size_gib
    );
    Ok(())
}

/// Parsed fields from the local provider's slot RAM-disk marker.
struct RamDiskRecord {
    device: String,
    volume_device: String,
    volume: String,
    size_gib: u64,
    job: String,
    mount: PathBuf,
    source: Option<PathBuf>,
}

/// Parse and confine a slot RAM-disk marker to its `<slot>/targets/<key>`
/// mount path. A root `repo` marker could hide the seeded checkout, so it is
/// never accepted. The OS listing proves the exact mounted volume before
/// teardown.
fn ramdisk_record(marker: &str, dir: &Path, id: &str) -> Option<RamDiskRecord> {
    let field = |name: &str| {
        marker
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{name}=")))
    };
    let device = field("device")?.to_string();
    let volume_device = field("volume_device")?.to_string();
    let volume = field("volume")?.to_string();
    let job = field("job")?.to_string();
    let expected_volume = match pool_job_volume(id, &job) {
        Ok(volume) => volume,
        Err(_) => return None,
    };
    let size_gib = match field("size_gib")?.parse() {
        Ok(size_gib) => size_gib,
        Err(_) => return None,
    };
    let mount = PathBuf::from(field("mount")?);
    let source = field("source")
        .filter(|source| !source.is_empty())
        .map(PathBuf::from);
    let relative = mount.strip_prefix(dir.join("targets"));
    let target_mount = relative.as_ref().is_ok_and(|relative| {
        relative.components().count() == 1
            && relative
                .components()
                .all(|part| matches!(part, std::path::Component::Normal(_)))
    });
    if volume != expected_volume
        || !crate::pool::ramdisk::same_device_id(&device, &device)
        || !crate::pool::ramdisk::same_device_id(&volume_device, &volume_device)
        || job.is_empty()
        || !target_mount
    {
        return None;
    }
    Some(RamDiskRecord {
        device,
        volume_device,
        volume,
        size_gib,
        job,
        mount,
        source,
    })
}

/// Remove only the `repo` worktree under a RAM mount when its source repo is
/// recorded and its Git marker is still present. Fetch-mode pools own a clone
/// on the RAM disk and need no external worktree removal.
fn remove_ramdisk_worktree(record: &RamDiskRecord) -> Option<String> {
    let source = record.source.as_deref()?;
    let worktree = record.mount.join("repo");
    if !worktree.join(".git").exists() {
        return None;
    }
    let repo = crate::pool::local_git::Repo::new(source);
    let expected = match repo.common_dir() {
        Ok(common) => common,
        Err(error) => return Some(error.to_string()),
    };
    let actual = match crate::pool::local_git::Repo::new(&worktree).common_dir() {
        Ok(common) => common,
        Err(error) => return Some(error.to_string()),
    };
    if actual != expected {
        return Some(format!(
            "RAM worktree {} belongs to another repository; preserving it",
            worktree.display()
        ));
    }
    repo.remove_worktree(&worktree)
        .err()
        .map(|error| error.to_string())
}

/// Remove a prior SSD-backed checkout at this exact cache path before the
/// RAM disk covers it, after proving it belongs to the configured source.
fn remove_cached_worktree(source: &str, checkout: &Path) -> Result<(), VivariumError> {
    if !checkout.exists() {
        return Ok(());
    }
    let repo = crate::pool::local_git::Repo::new(Path::new(source));
    let actual = crate::pool::local_git::Repo::new(checkout).common_dir()?;
    if actual != repo.common_dir()? {
        return Err(VivariumError::Config(
            "target cache belongs to another repository".into(),
        ));
    }
    repo.remove_worktree(checkout)
}

/// Start a remote-backed job from a fresh, empty Git repository on its RAM
/// mount. The job runner fetches the requested commit and pushes by URL.
/// The volume label for a local slot job: `vivi-<pool>-<slot>-<job>` under a
/// project-relative root, or `vivi-<mailspace>-<pool>-<slot>-<job>` otherwise.
fn pool_job_volume(id: &str, job: &str) -> Result<String, VivariumError> {
    let mut parts = id.split('/');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(pool), Some(slot), None, None) => {
            Ok(crate::pool::ramdisk::volume_name(&[pool, slot, job]))
        }
        (Some(mailspace), Some(pool), Some(slot), None) => {
            Ok(crate::pool::ramdisk::volume_name(&[
                mailspace, pool, slot, job,
            ]))
        }
        _ => Err(VivariumError::Message(format!("not a slot id: {id}"))),
    }
}

/// The marker stays on the SSD slot root so unclaim, destroy, and pump restart
/// can prove ownership after the RAM volume itself is gone.
fn ramdisk_marker(
    disk: &crate::pool::ramdisk::Disk,
    job: &str,
    source: &str,
) -> Result<String, VivariumError> {
    let backing = disk.backing_device.as_deref().ok_or_else(|| {
        VivariumError::Message("new RAM disk has no verified physical backing".into())
    })?;
    Ok(format!(
        "device={backing}\nvolume_device={}\nvolume={}\nsize_gib={}\njob={job}\nmount={}\nsource={source}\n",
        disk.device,
        disk.volume,
        disk.size_gib,
        disk.mount.display()
    ))
}

/// Prepare a default repository workspace for a job without an explicit
/// target. The returned key is added to its bundle so the runner works under
/// `<slot>/targets/<key>/repo`, on the mounted disk.
fn prepare_default_job_ramdisk(
    provider: &LocalProvider,
    id: &str,
    job: &str,
    start: &str,
) -> Result<super::provider::PreparedWorkspace, VivariumError> {
    let slot = provider.known(id)?;
    let key = hex::encode(Sha256::digest(format!("{id}\0{job}")));
    let targets = slot.join("targets");
    let cache = targets.join(&key);
    reject_target_symlinks(&targets, &cache)?;
    std::fs::create_dir_all(&cache)?;
    let source = provider.source.as_deref();
    let source_text = source.map(|path| path.to_string_lossy().into_owned());
    mount_job_ramdisk(provider, id, job, &slot, &cache, source_text.as_deref())?;
    if let Some(source) = source {
        let checkout = cache.join(slots::CHECKOUT);
        crate::pool::local_git::Repo::new(source).add_worktree(&checkout, start)?;
        std::fs::write(
            cache.join(slots::WORKTREE_OF_FILE),
            source_text.as_deref().unwrap_or_default(),
        )?;
    }
    Ok(super::provider::PreparedWorkspace {
        cache_key: key,
        local_source: source_text,
    })
}

/// Reject symlinked target cache paths before the provider mounts a volume
/// over them.
fn reject_target_symlinks(targets: &Path, cache: &Path) -> Result<(), VivariumError> {
    for path in [targets, cache, &cache.join(slots::CHECKOUT)] {
        if path
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err(VivariumError::Config(
                "target checkout cannot be a symlink".into(),
            ));
        }
    }
    Ok(())
}

/// Mount a fresh volume after memory admission, then atomically record its
/// owner on the slot root for teardown and pump-start recovery.
fn mount_job_ramdisk(
    provider: &LocalProvider,
    id: &str,
    job: &str,
    slot: &Path,
    cache: &Path,
    source: Option<&str>,
) -> Result<(), VivariumError> {
    super::ssh::one_line(job)?;
    if let Some(source) = source {
        super::ssh::one_line(source)?;
    }
    teardown_marked_ramdisk(slot, id, provider.ramdisk_runner.as_ref())?;
    if let Some(source) = source {
        remove_cached_worktree(source, &cache.join(slots::CHECKOUT))?;
    }
    mount_ramdisk_at(provider, id, job, slot, cache, source.unwrap_or_default())
}

fn teardown_marked_ramdisk(
    slot: &Path,
    id: &str,
    runner: &dyn crate::pool::ramdisk::Runner,
) -> Result<(), VivariumError> {
    let marker = slot.join("ramdisk");
    match marker.symlink_metadata() {
        Ok(_) => teardown_ramdisk(slot, id, runner),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(VivariumError::Message(format!(
            "cannot inspect RAM disk marker {}: {error}",
            marker.display()
        ))),
    }
}

fn mount_ramdisk_at(
    provider: &LocalProvider,
    id: &str,
    job: &str,
    slot: &Path,
    mount: &Path,
    source: &str,
) -> Result<(), VivariumError> {
    let _admission_lock = crate::pool::ramdisk::lock_admission().map_err(VivariumError::Message)?;
    let want = provider.ramdisk.size_gib;
    crate::pool::ramdisk::admit_with(
        provider.ramdisk_runner.as_ref(),
        available_memory(provider),
        want,
        provider.ramdisk.margin_gib,
    )
    .map_err(VivariumError::Message)?;
    let volume = pool_job_volume(id, job)?;
    let disk = crate::pool::ramdisk::mount(provider.ramdisk_runner.as_ref(), &volume, want, mount)
        .map_err(VivariumError::Message)?;
    let marker = match ramdisk_marker(&disk, job, source) {
        Ok(marker) => marker,
        Err(error) => {
            if let Err(teardown) =
                crate::pool::ramdisk::teardown(provider.ramdisk_runner.as_ref(), &disk)
            {
                eprintln!(
                    "warning: RAM disk {} teardown verification failed: {teardown}",
                    disk.volume
                );
            }
            return Err(error);
        }
    };
    if let Err(error) = slots::replace_file(&slot.join("ramdisk"), marker.as_bytes()) {
        if let Err(teardown) =
            crate::pool::ramdisk::teardown(provider.ramdisk_runner.as_ref(), &disk)
        {
            eprintln!(
                "warning: RAM disk {} teardown verification failed after marker write error: {teardown}",
                disk.volume
            );
        }
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
fn available_memory(provider: &LocalProvider) -> u64 {
    provider
        .available_memory
        .unwrap_or_else(crate::pool::ramdisk::available_bytes)
}

#[cfg(not(test))]
fn available_memory(_provider: &LocalProvider) -> u64 {
    crate::pool::ramdisk::available_bytes()
}

impl PoolProvider for LocalProvider {
    fn local_root(&self) -> Option<PathBuf> {
        self.site.as_ref().ok().map(|site| site.root.clone())
    }

    fn local_pool_dir(&self) -> Option<PathBuf> {
        self.site.as_ref().ok().map(|site| site.pool_dir.clone())
    }

    fn ramdisk_job_settings(&self) -> Option<(u64, Vec<(String, String)>)> {
        self.ramdisk
            .enabled
            .then(|| (self.ramdisk.size_gib, self.ramdisk.job_env()))
    }

    fn prepare_job(
        &self,
        id: &str,
        target: Option<&super::dispatch::target::JobTarget>,
        start: &str,
        job: &str,
    ) -> Result<Option<super::provider::PreparedWorkspace>, VivariumError> {
        match target {
            Some(target) => self
                .prepare_target_checkout(id, target, start, job)
                .map(|()| None),
            None if self.ramdisk.enabled => {
                prepare_default_job_ramdisk(self, id, job, start).map(Some)
            }
            None => Ok(None),
        }
    }

    fn prepare_target_checkout(
        &self,
        id: &str,
        target: &super::dispatch::target::JobTarget,
        start: &str,
        job: &str,
    ) -> Result<(), VivariumError> {
        let source = target.local_source.as_deref();
        if source.is_none() && !self.ramdisk.enabled {
            return Ok(());
        }
        if target.cache_key.len() != 64 || !target.cache_key.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(VivariumError::Config("invalid target checkout key".into()));
        }
        let slot = self.known(id)?;
        let targets = slot.join("targets");
        let cache = targets.join(&target.cache_key);
        for path in [&targets, &cache, &cache.join(slots::CHECKOUT)] {
            if path
                .symlink_metadata()
                .is_ok_and(|m| m.file_type().is_symlink())
            {
                return Err(VivariumError::Config(
                    "target checkout cannot be a symlink".into(),
                ));
            }
        }
        std::fs::create_dir_all(&cache)?;
        if self.ramdisk.enabled {
            mount_job_ramdisk(self, id, job, &slot, &cache, source)?;
        }
        let Some(source) = source else {
            return Ok(());
        };
        let checkout = cache.join(slots::CHECKOUT);
        let repo = super::local_git::Repo::new(Path::new(source));
        if checkout.exists() {
            let actual = super::local_git::Repo::new(&checkout).common_dir()?;
            if actual != repo.common_dir()? {
                return Err(VivariumError::Config(
                    "target cache belongs to another repository".into(),
                ));
            }
        } else {
            repo.add_worktree(&checkout, start)?;
        }
        std::fs::write(cache.join(slots::WORKTREE_OF_FILE), source)?;
        Ok(())
    }

    /// The root is made by the first `create`; nothing to make beforehand.
    fn ensure_home(&self) -> Result<Option<String>, VivariumError> {
        Ok(None)
    }

    fn doctor(&self, harness_env: &[String]) -> Vec<Check> {
        self.doctor_checks(harness_env)
    }

    fn harness_env_gaps(&self, names: &[String]) -> Result<Vec<String>, VivariumError> {
        Ok(unset_names(names, env_is_set))
    }

    /// The values are read from the environment of the `vivi` process that
    /// starts the job, so a dispatch can still be told.
    fn env_gaps_at_dispatch(&self, names: &[String]) -> Result<Vec<String>, VivariumError> {
        Ok(unset_names(names, env_is_set))
    }

    fn env_gap_hint(&self, missing: &[String]) -> String {
        format!(
            "warning: {} not set in the environment of this vivi process. Jobs on local slots read the harness environment from the vivi process that starts them; no value is stored anywhere. Export {} before the dispatch.",
            missing.join(", "),
            missing.join(", ")
        )
    }

    fn live_test_notice(&self) -> String {
        format!(
            "it makes two slot directories under {} on this machine, runs stand-in jobs there as you, and removes them afterwards (Ctrl-C stops it and cleans up)",
            self.root.display()
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

    fn list(&self) -> Result<Vec<MachineRecord>, VivariumError> {
        proc::reap();
        let records = slots::list(self.site()?)?;
        if let Some(configured) = self.source.as_deref() {
            for record in &records {
                let dir = self.dir(&record.id)?;
                refuse_foreign_worktree(&record.id, &dir, configured)?;
            }
        }
        Ok(records)
    }

    fn create(&self, spec: &CreateSpec) -> Result<String, VivariumError> {
        let site = self.site()?;
        let id = new_slot_id(site, spec)?;
        slots::create(&site.root, &id, spec)?;
        if let Err(e) = self.start(&id) {
            if let Err(undo) = self.destroy(&id) {
                eprintln!("warning: could not remove {}: {undo}", spec.name);
            }
            return Err(e);
        }
        Ok(id)
    }

    fn destroy(&self, id: &str) -> Result<(), VivariumError> {
        let dir = self.dir(id)?;
        if let Some(configured) = self.source.as_deref() {
            refuse_foreign_worktree(id, &dir, configured)?;
        }
        teardown_ramdisk(&dir, id, self.ramdisk_runner.as_ref())?;
        slots::destroy(&self.site()?.root, id)
    }

    fn destroy_command(&self, id: &str) -> String {
        format!(
            "rm -rf {}/{id} (removes the slot directory only)",
            self.root.display()
        )
    }

    fn machine(&self, id: &str) -> Result<MachineRecord, VivariumError> {
        proc::reap();
        let dir = self.dir(id)?;
        Ok(if slots::is_known(&dir) {
            slots::record_of(&dir, id)
        } else {
            MachineRecord::gone(id)
        })
    }

    fn claim(&self, id: &str, task: &str, now: u64) -> Result<Option<Hold>, VivariumError> {
        super::ssh::one_line(task)?;
        let dir = self.dir(id)?;
        if let Some(configured) = self.source.as_deref() {
            refuse_foreign_worktree(id, &dir, configured)?;
        }
        slots::claim(&dir, task, now)
    }

    /// The slot is free again: back on a detached HEAD, so its branch is free.
    fn unclaim(&self, id: &str) -> Result<(), VivariumError> {
        let dir = self.dir(id)?;
        teardown_ramdisk(&dir, id, self.ramdisk_runner.as_ref())?;
        slots::unclaim(&dir)?;
        slots::detach_if_idle(&dir);
        let _ignored =
            super::clean::after_slot_freed(&dir, self.build_dir.as_deref(), self.build_baseline);
        Ok(())
    }

    /// The claim file stays: it is the mark, and it ends only with
    /// [`PoolProvider::unclaim`].
    fn release(&self, _id: &str, _hold: &Hold) -> Result<(), VivariumError> {
        Ok(())
    }

    fn put_file(
        &self,
        id: &str,
        hold: &Hold,
        path: &str,
        content: &[u8],
    ) -> Result<(), VivariumError> {
        slots::put(&self.known(id)?, hold, path, content)
    }

    /// Nothing is dropped from an environment (a one-time secret lived in a
    /// file the boot script removed); the mark is a file.
    fn seal(
        &self,
        id: &str,
        _drop_env: &[&str],
        key: &str,
        value: &str,
    ) -> Result<(), VivariumError> {
        slots::seal(&self.known(id)?, key, value)
    }

    fn start(&self, id: &str) -> Result<(), VivariumError> {
        proc::start(&self.known(id)?)
    }

    /// Everything the slot started ends; a worktree slot goes back to a
    /// detached HEAD.
    fn stop(&self, id: &str) -> Result<(), VivariumError> {
        let dir = self.known(id)?;
        proc::stop(&dir)?;
        slots::detach_if_idle(&dir);
        Ok(())
    }

    fn exec(&self, id: &str, script: &str, timeout_secs: u32) -> Result<ExecOutput, VivariumError> {
        let dir = self.known(id)?;
        let wait = Duration::from_secs(u64::from(timeout_secs) + 15);
        let child = Command::new("bash")
            .args(["-c", script])
            .current_dir(&dir)
            .env("WORK", &dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| VivariumError::Message(format!("failed to run `bash`: {e}")))?;
        let RunOutput {
            code,
            stdout,
            stderr,
        } = finish(child, None, wait, "bash")?;
        Ok(ExecOutput {
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr,
            exit_code: i64::from(code),
        })
    }

    /// Any slot can be read without starting it: it is a directory.
    fn read_stopped(
        &self,
        id: &str,
        paths: &[&str],
    ) -> Result<Option<BTreeMap<String, Vec<u8>>>, VivariumError> {
        slots::read_files(&self.known(id)?, paths).map(Some)
    }
}

#[cfg(test)]
#[path = "local_test.rs"]
mod tests;

#[cfg(test)]
#[path = "local_worktree_test.rs"]
mod worktree_tests;
