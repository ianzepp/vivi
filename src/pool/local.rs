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
use std::time::Duration;

use super::config::PoolConfig;
use super::docker::{env_is_set, unset_names};
use super::docker_cli::{RunOutput, finish};
use super::doctor::Check;
use super::local_proc as proc;
use super::local_slots as slots;
use super::provider::{CreateSpec, ExecOutput, Hold, MachineRecord, PoolProvider};
use super::slot::{META_MAILSPACE, META_POOL, META_SLOT};
use super::ssh_state::{self as state, is_slot_id};
use crate::error::VivariumError;

/// Pool provider for this machine.
pub struct LocalProvider {
    root: PathBuf,
    /// The repository the pool's slots are worktrees of, as configured; the
    /// doctor looks at it. The slots themselves carry what they need.
    source: Option<PathBuf>,
    build_dir: Option<String>,
    /// The pool's `build_baseline_gib` in bytes, for reclaim of a slot that
    /// carries no recorded baseline.
    build_baseline: Option<u64>,
}

impl LocalProvider {
    #[must_use]
    pub fn new(config: &PoolConfig) -> Self {
        Self {
            root: slots::expand_root(config.local.root_or_default()),
            source: config.local.worktree_path(),
            build_dir: config.build_dir.clone(),
            build_baseline: config.build_baseline_bytes(),
        }
    }

    pub(super) fn root(&self) -> &std::path::Path {
        &self.root
    }

    pub(super) fn source(&self) -> Option<&std::path::Path> {
        self.source.as_deref()
    }

    /// The directory of slot `id`, which must be a slot id.
    fn dir(&self, id: &str) -> Result<PathBuf, VivariumError> {
        if is_slot_id(id) {
            Ok(self.root.join(id))
        } else {
            Err(VivariumError::Message(format!("not a slot id: {id}")))
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

/// The id a new slot gets from its spec: `<mailspace>/<pool>/slot-<index>`.
fn new_slot_id(spec: &CreateSpec) -> Result<String, VivariumError> {
    let field = |key: &str| {
        spec.metadata
            .get(key)
            .ok_or_else(|| VivariumError::Message(format!("slot spec lacks {key}")))
    };
    let id = state::slot_id(field(META_MAILSPACE)?, field(META_POOL)?, field(META_SLOT)?);
    if is_slot_id(&id) {
        Ok(id)
    } else {
        Err(VivariumError::Message(format!("not a slot id: {id}")))
    }
}

/// Refuse a slot whose `worktree_of` names a repository other than the pool's
/// configured source: its jobs would commit into a foreign repository. A
/// missing file (a fetch slot) or an unreadable one claims as before.
fn refuse_foreign_worktree(
    id: &str,
    dir: &Path,
    configured: &Path,
) -> Result<(), VivariumError> {
    let file = dir.join(slots::WORKTREE_OF_FILE);
    let Ok(recorded) = std::fs::read_to_string(&file) else {
        return Ok(());
    };
    let recorded = recorded.trim();
    let real =
        |path: &str| std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path));
    if real(recorded) == real(&configured.to_string_lossy()) {
        return Ok(());
    }
    Err(VivariumError::Message(format!(
        "slot {id} is a worktree of {recorded}, not of this pool's repository {}; \
         remake it from this pool's repository with `vivi pool destroy {id}` then `vivi pool create`",
        configured.display()
    )))
}

impl PoolProvider for LocalProvider {
    fn prepare_target_checkout(
        &self,
        id: &str,
        target: &super::dispatch::target::JobTarget,
        start: &str,
    ) -> Result<(), VivariumError> {
        let Some(source) = &target.local_source else {
            return Ok(());
        };
        if target.cache_key.len() != 64 || !target.cache_key.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(VivariumError::Config("invalid target checkout key".into()));
        }
        let targets = self.known(id)?.join("targets");
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
        let checkout = cache.join(slots::CHECKOUT);
        let repo = super::local_git::Repo::new(std::path::Path::new(source));
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
        slots::list(&self.root)
    }

    fn create(&self, spec: &CreateSpec) -> Result<String, VivariumError> {
        let id = new_slot_id(spec)?;
        slots::create(&self.root, &id, spec)?;
        if let Err(e) = self.start(&id) {
            if let Err(undo) = self.destroy(&id) {
                eprintln!("warning: could not remove {}: {undo}", spec.name);
            }
            return Err(e);
        }
        Ok(id)
    }

    fn destroy(&self, id: &str) -> Result<(), VivariumError> {
        self.dir(id)?;
        slots::destroy(&self.root, id)
    }

    fn destroy_command(&self, id: &str) -> String {
        format!(
            "rm -rf {}/{id} (and, for a worktree slot, `git worktree prune` in its repository)",
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
