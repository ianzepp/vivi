//! The `docker` command line as the pool provider.
//!
//! A slot is a container. Stopped, it is a slot at rest and its writable layer
//! is the persistent disk; its main process is the boot script, so it stops
//! when the script exits. Everything is one `docker` subprocess per call, with
//! the daemon (local, or remote through `[pool.docker].host`) chosen by
//! `DOCKER_HOST`.
//!
//! What a container cannot do after creation shapes the rest:
//!
//! - Labels are fixed, so they hold only fixed facts: pool, index, base,
//!   architecture.
//! - The seed mark changes while the container is stopped, so it is a small
//!   file on the slot's own disk, read and written with `docker cp` (which
//!   works on stopped containers). See [`MARKS_DIR`].
//! - The claim (task and claim time) is a separate, never-started claim
//!   container named after the slot. Creating a container with a name that is
//!   in use fails, and the daemon decides which of two creators wins, so the
//!   claim is atomic across processes and machines sharing the daemon. Its
//!   labels carry the task and the claim time; removing it frees the slot.
//! - The environment is fixed too, so the one-time fetch token is delivered as
//!   a file the boot script reads and deletes, and the harness keys are copied
//!   from the creating process (`docker create -e NAME`; no value on the
//!   command line).

use std::collections::BTreeMap;
use std::time::Duration;

use super::bootscript::CHECKOUT_DIR;
use super::config::PoolConfig;
use super::docker_cli::{CALL_TIMEOUT, DockerCli, PULL_TIMEOUT, RunOutput, Runner};
use super::docker_state::{
    self as state, Container, LABEL_CLAIM_OF, LABEL_ROLE, MARKS_DIR, ROLE_CLAIM, ROLE_SLOT,
};
use super::docker_tar as tar;
use super::doctor::Check;
use super::provider::{CreateSpec, ExecOutput, Hold, MachineRecord, PoolProvider};
use super::slot::{self, META_ARCH, META_CLAIMED, META_POOL, META_TASK};
use crate::error::VivariumError;

/// Seconds a stopping container gets before the daemon kills it.
const STOP_GRACE_SECS: &str = "10";
/// Where a delivered one-time secret lands; the boot script reads and removes it.
pub const ONCE_ENV_PATH: &str = "/work/once.env";

/// The resource limits of a slot, from `[pool.docker]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Limits {
    pub memory: String,
    pub cpus: Option<f64>,
}

/// Pool provider backed by the `docker` binary.
pub struct DockerProvider {
    runner: Box<dyn Runner>,
    pub(super) host: Option<String>,
    pub(super) image: String,
    pub(super) limits: Limits,
}

impl DockerProvider {
    #[must_use]
    pub fn new(config: &PoolConfig) -> Self {
        let runner = DockerCli::new(config.docker.host.as_deref());
        Self::with_runner(config, Box::new(runner))
    }

    /// A provider that talks through `runner` (tests).
    #[must_use]
    pub fn with_runner(config: &PoolConfig, runner: Box<dyn Runner>) -> Self {
        Self {
            runner,
            host: config.docker.host.clone(),
            image: config.image.clone(),
            limits: Limits {
                memory: config.docker.memory.clone(),
                cpus: config.docker.cpus,
            },
        }
    }

    /// Where the daemon is, as the listing's `region` column reads.
    fn where_text(&self) -> &str {
        self.host.as_deref().unwrap_or("local")
    }

    /// One call; a failing exit code is returned, not raised.
    pub(super) fn call(
        &self,
        args: &[&str],
        stdin: Option<&[u8]>,
        timeout: Duration,
    ) -> Result<RunOutput, VivariumError> {
        let owned: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
        self.runner.run(&owned, stdin, timeout)
    }

    /// One call that must succeed.
    pub(super) fn require(&self, args: &[&str]) -> Result<RunOutput, VivariumError> {
        let out = self.call(args, None, CALL_TIMEOUT)?;
        if out.succeeded() {
            Ok(out)
        } else {
            Err(failure(args, &out))
        }
    }

    /// Inspect containers by name or id. A name that matches nothing is just
    /// absent from the answer.
    fn inspect(&self, names: &[&str]) -> Result<Vec<Container>, VivariumError> {
        let mut args = vec!["inspect", "--type", "container"];
        args.extend_from_slice(names);
        let out = self.call(&args, None, CALL_TIMEOUT)?;
        let text = out.stdout_text();
        if text.is_empty() || text == "[]" {
            return if out.succeeded() || is_absent(&out.stderr) {
                Ok(Vec::new())
            } else {
                Err(failure(&args, &out))
            };
        }
        state::parse_inspect(&text)
    }

    /// Every container carrying the pool label, pool members or not.
    fn pool_containers(&self) -> Result<Vec<Container>, VivariumError> {
        let ids = self.require(&[
            "ps",
            "-a",
            "-q",
            "--no-trunc",
            "--filter",
            "label=vivi_pool",
        ])?;
        let ids = ids.stdout_text();
        let names: Vec<&str> = ids.split_whitespace().collect();
        if names.is_empty() {
            return Ok(Vec::new());
        }
        self.inspect(&names)
    }

    /// The marks on a slot's disk: file name to contents. A slot with none
    /// (or already gone) has none.
    fn marks(&self, id: &str) -> Result<BTreeMap<String, String>, VivariumError> {
        let Some(bytes) = self.copy_out(id, MARKS_DIR)? else {
            return Ok(BTreeMap::new());
        };
        Ok(tar::read(&bytes)?
            .into_iter()
            .filter_map(|f| {
                let key = f.path.rsplit('/').next()?.to_string();
                let value = String::from_utf8_lossy(&f.content).trim().to_string();
                (!key.is_empty() && !value.is_empty()).then_some((key, value))
            })
            .collect())
    }

    /// Copy a path out of a (stopped or running) container as a tar stream.
    /// `None` when the path or the container does not exist.
    fn copy_out(&self, id: &str, path: &str) -> Result<Option<Vec<u8>>, VivariumError> {
        let source = format!("{id}:{path}");
        let args = ["cp", source.as_str(), "-"];
        let out = self.call(&args, None, CALL_TIMEOUT)?;
        if out.succeeded() {
            return Ok(Some(out.stdout));
        }
        if is_absent(&out.stderr) || out.stderr.contains("Could not find the file") {
            return Ok(None);
        }
        Err(failure(&args, &out))
    }

    /// Write files (`(absolute guest path, contents)`) into a container,
    /// creating missing directories, without starting it.
    pub(super) fn copy_in(
        &self,
        id: &str,
        files: &[(String, Vec<u8>)],
    ) -> Result<(), VivariumError> {
        let archive = tar::build(files)?;
        let target = format!("{id}:/");
        let out = self.call(&["cp", "-", target.as_str()], Some(&archive), CALL_TIMEOUT)?;
        if out.succeeded() {
            Ok(())
        } else {
            Err(failure(&["cp"], &out))
        }
    }

    fn remove(&self, name: &str) -> Result<(), VivariumError> {
        let args = ["rm", "-f", "-v", name];
        let out = self.call(&args, None, CALL_TIMEOUT)?;
        if out.succeeded() || is_absent(&out.stderr) {
            Ok(())
        } else {
            Err(failure(&args, &out))
        }
    }

    /// Make sure the image is here: pull it when missing.
    fn ensure_image(&self, image: &str) -> Result<(), VivariumError> {
        let args = ["image", "inspect", "--format", "{{.Id}}", image];
        if self.call(&args, None, CALL_TIMEOUT)?.succeeded() {
            return Ok(());
        }
        let pulled = self.call(&["pull", "-q", image], None, PULL_TIMEOUT)?;
        if pulled.succeeded() {
            Ok(())
        } else {
            Err(failure(&["pull"], &pulled))
        }
    }

    /// The architecture a new container runs: what its image's manifest says,
    /// else what the image says, else the daemon's own (an image may be built
    /// for another architecture than the daemon and run emulated, so the
    /// daemon's is the last resort, not the first guess).
    fn container_arch(&self, container: &Container) -> Result<String, VivariumError> {
        if let Some(arch) = &container.arch {
            return Ok(arch.clone());
        }
        let args = [
            "image",
            "inspect",
            "--format",
            "{{.Architecture}}",
            &container.image,
        ];
        let image = self.call(&args, None, CALL_TIMEOUT)?;
        if image.succeeded() && !image.stdout_text().is_empty() {
            return Ok(image.stdout_text());
        }
        let daemon = self.require(&["info", "--format", "{{.Architecture}}"])?;
        Ok(go_arch(&daemon.stdout_text()))
    }

    /// True when the slot, as it is right now and ignoring any claim, is free.
    fn is_free_now(&self, id: &str) -> Result<bool, VivariumError> {
        let found = self.inspect(&[id])?;
        let Some(slot) = found.iter().find(|c| c.role() == ROLE_SLOT) else {
            return Ok(false);
        };
        let marks = self.marks(&slot.id)?;
        Ok(slot::is_free(&state::record(
            slot,
            &marks,
            None,
            self.where_text(),
        )))
    }
}

/// The error for a failed call: what ran, and what docker said.
fn failure(args: &[&str], out: &RunOutput) -> VivariumError {
    let verb = args.iter().take(2).copied().collect::<Vec<_>>().join(" ");
    VivariumError::Message(format!("`docker {verb}` failed: {}", out.stderr.trim()))
}

fn is_absent(stderr: &str) -> bool {
    stderr.contains("No such container") || stderr.contains("No such object")
}

/// Harness environment names not set (or empty) according to `is_set`.
#[must_use]
pub fn unset_names(names: &[String], is_set: impl Fn(&str) -> bool) -> Vec<String> {
    names.iter().filter(|n| !is_set(n)).cloned().collect()
}

/// True when the process environment holds a non-empty `name`.
pub(super) fn env_is_set(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| !v.is_empty())
}

/// Go's name for a CPU architecture as `docker info` spells it (`aarch64`,
/// `x86_64`), the names images use (`arm64`, `amd64`).
#[must_use]
pub fn go_arch(name: &str) -> String {
    match name {
        "aarch64" => "arm64",
        "x86_64" => "amd64",
        other => other,
    }
    .to_string()
}

/// Arguments for `docker create`. `passed` are the harness names that are set
/// in the creating process: they go as `-e NAME`, never with a value.
#[must_use]
pub fn create_args(spec: &CreateSpec, limits: &Limits, passed: &[String]) -> Vec<String> {
    let mount = format!("{CHECKOUT_DIR}/{}", spec.build_dir);
    let mut args: Vec<String> = [
        "create",
        "--name",
        &spec.name,
        "--init",
        "--restart",
        "no",
        "--memory",
        &limits.memory,
    ]
    .map(str::to_string)
    .to_vec();
    if let Some(cpus) = limits.cpus {
        args.extend(["--cpus".to_string(), cpus.to_string()]);
    }
    // `exec`: a build output the build runs binaries from must be executable.
    args.extend([
        "--tmpfs".to_string(),
        format!("{mount}:rw,exec,size={}", spec.tmpfs),
    ]);
    let labels = spec
        .metadata
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .chain([format!("{LABEL_ROLE}={ROLE_SLOT}")]);
    for label in labels {
        args.extend(["--label".to_string(), label]);
    }
    for (key, value) in &spec.env {
        args.extend(["-e".to_string(), format!("{key}={value}")]);
    }
    for name in passed {
        args.extend(["-e".to_string(), name.clone()]);
    }
    args.push(spec.image.clone());
    args.extend(spec.command.iter().cloned());
    args
}

/// Quote `value` for a POSIX shell single-quoted string.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// The files a new slot is created with: the spec's own, and the one-time
/// secrets as a file the boot script reads and removes.
pub(super) fn slot_files(spec: &CreateSpec) -> Vec<(String, Vec<u8>)> {
    let mut files: Vec<(String, Vec<u8>)> = spec
        .files
        .iter()
        .map(|(path, text)| (path.clone(), text.clone().into_bytes()))
        .collect();
    if !spec.once_env.is_empty() {
        let lines: Vec<String> = spec
            .once_env
            .iter()
            .map(|(k, v)| format!("export {k}={}\n", shell_quote(v)))
            .collect();
        let text = lines.concat();
        files.push((ONCE_ENV_PATH.to_string(), text.into_bytes()));
    }
    files
}

impl DockerProvider {
    /// Put a new slot's files and its architecture mark on its disk.
    fn deliver_slot(&self, id: &str, spec: &CreateSpec) -> Result<(), VivariumError> {
        let made = self
            .inspect(&[id])?
            .into_iter()
            .next()
            .ok_or_else(|| VivariumError::Message(format!("new container {id} is gone")))?;
        let arch = self.container_arch(&made)?;
        let mut files = slot_files(spec);
        files.push((
            format!("{MARKS_DIR}/{META_ARCH}"),
            format!("{arch}\n").into_bytes(),
        ));
        self.copy_in(id, &files)
    }
}

impl PoolProvider for DockerProvider {
    /// A Docker daemon needs no home to be made.
    fn ensure_home(&self) -> Result<Option<String>, VivariumError> {
        Ok(None)
    }

    fn doctor(&self, harness_env: &[String]) -> Vec<Check> {
        self.doctor_checks(harness_env)
    }

    fn harness_env_gaps(&self, names: &[String]) -> Result<Vec<String>, VivariumError> {
        Ok(unset_names(names, env_is_set))
    }

    /// The environment is fixed when a slot is created, so the names a
    /// dispatch could still supply are none.
    fn env_gaps_at_dispatch(&self, _names: &[String]) -> Result<Vec<String>, VivariumError> {
        Ok(Vec::new())
    }

    fn env_gap_hint(&self, missing: &[String]) -> String {
        format!(
            "warning: {} not set in the environment of this vivi process. Docker slots take their harness environment from it when they are created, so slots created now will run jobs without it. Export {} and create the slots again (`vivi pool destroy --all --yes`, then `vivi pool create`); the values then live in each container's configuration on the Docker daemon.",
            missing.join(", "),
            missing
                .iter()
                .map(|n| format!("{n}=<value>"))
                .collect::<Vec<_>>()
                .join(" ")
        )
    }

    fn live_test_notice(&self) -> String {
        "it creates two small containers on the Docker daemon for a few minutes and removes them afterwards (Ctrl-C stops it and cleans up)".to_string()
    }

    fn supplies_build_mount(&self) -> bool {
        true
    }

    fn build_persists(&self) -> bool {
        false
    }

    fn runs_unprivileged(&self) -> bool {
        false
    }

    fn list(&self) -> Result<Vec<MachineRecord>, VivariumError> {
        let containers = self.pool_containers()?;
        let slots: Vec<&Container> = containers
            .iter()
            .filter(|c| c.role() == ROLE_SLOT)
            .collect();
        let marks: Vec<Result<BTreeMap<String, String>, VivariumError>> =
            std::thread::scope(|scope| {
                let tasks: Vec<_> = slots
                    .iter()
                    .map(|s| scope.spawn(|| self.marks(&s.id)))
                    .collect();
                tasks
                    .into_iter()
                    .map(|t| {
                        t.join().unwrap_or_else(|_| {
                            Err(VivariumError::Message("marks reader panicked".into()))
                        })
                    })
                    .collect()
            });
        slots
            .iter()
            .zip(marks)
            .map(|(slot, marks)| {
                let claim = state::claim_for(&containers, &slot.id);
                Ok(state::record(slot, &marks?, claim, self.where_text()))
            })
            .collect()
    }

    fn create(&self, spec: &CreateSpec) -> Result<String, VivariumError> {
        self.ensure_image(&spec.image)?;
        let passed: Vec<String> = spec
            .passthrough_env
            .iter()
            .filter(|n| env_is_set(n))
            .cloned()
            .collect();
        let args = create_args(spec, &self.limits, &passed);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let id = state::short(&self.require(&refs)?.stdout_text()).to_string();
        let started = self
            .deliver_slot(&id, spec)
            .and_then(|()| self.require(&["start", &id]).map(drop));
        if let Err(e) = started {
            if let Err(undo) = self.remove(&id) {
                eprintln!("warning: could not remove {}: {undo}", spec.name);
            }
            return Err(e);
        }
        Ok(id)
    }

    fn destroy(&self, id: &str) -> Result<(), VivariumError> {
        self.remove(&state::claim_name(id))?;
        self.remove(id)
    }

    fn destroy_command(&self, id: &str) -> String {
        let host = self
            .host
            .as_deref()
            .map_or(String::new(), |h| format!("DOCKER_HOST={h} "));
        format!("{host}docker rm -f -v {id} {}", state::claim_name(id))
    }

    fn machine(&self, id: &str) -> Result<MachineRecord, VivariumError> {
        let claim_name = state::claim_name(id);
        let found = self.inspect(&[id, &claim_name])?;
        let Some(slot) = found
            .iter()
            .find(|c| c.role() == ROLE_SLOT && c.id == state::short(id))
        else {
            return Ok(MachineRecord::gone(id));
        };
        let marks = self.marks(&slot.id)?;
        let claim = state::claim_for(&found, &slot.id);
        Ok(state::record(slot, &marks, claim, self.where_text()))
    }

    /// The claim is the creation of a container named after the slot: the
    /// daemon refuses a second creator.
    fn claim(&self, id: &str, task: &str, now: u64) -> Result<Option<Hold>, VivariumError> {
        let found = self.inspect(&[id])?;
        let Some(slot) = found.iter().find(|c| c.role() == ROLE_SLOT) else {
            return Err(VivariumError::Message(format!("slot {id} is gone")));
        };
        let pool = slot.labels.get(META_POOL).map_or("", String::as_str);
        let name = state::claim_name(&slot.id);
        let labels = [
            (META_POOL, pool.to_string()),
            (LABEL_ROLE, ROLE_CLAIM.to_string()),
            (LABEL_CLAIM_OF, slot.id.clone()),
            (META_TASK, task.to_string()),
            (META_CLAIMED, now.to_string()),
        ];
        let mut args = vec!["create".to_string(), "--name".to_string(), name];
        for (key, value) in labels {
            args.extend(["--label".to_string(), format!("{key}={value}")]);
        }
        args.push(slot.image.clone());
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let made = self.call(&refs, None, CALL_TIMEOUT)?;
        if !made.succeeded() {
            return if made.stderr.contains("is already in use") {
                Ok(None)
            } else {
                Err(failure(&refs, &made))
            };
        }
        let claim_id = made.stdout_text();
        // The listing may be stale: look at the slot again, now that nobody
        // else can claim it, before keeping the claim.
        match self.is_free_now(&slot.id) {
            Ok(true) => Ok(Some(Hold { nonce: claim_id })),
            other => {
                self.remove(&claim_id)?;
                other.map(|_| None)
            }
        }
    }

    fn unclaim(&self, id: &str) -> Result<(), VivariumError> {
        self.remove(&state::claim_name(id))
    }

    /// The claim marker stays: it is the mark, and it ends only with
    /// [`PoolProvider::unclaim`].
    fn release(&self, _id: &str, _hold: &Hold) -> Result<(), VivariumError> {
        Ok(())
    }

    fn put_file(
        &self,
        id: &str,
        _hold: &Hold,
        path: &str,
        content: &[u8],
    ) -> Result<(), VivariumError> {
        self.copy_in(id, &[(path.to_string(), content.to_vec())])
    }

    /// Environment names are not dropped: a one-time secret never lived in the
    /// container's configuration (see [`ONCE_ENV_PATH`]). The mark is a file.
    fn seal(
        &self,
        id: &str,
        _drop_env: &[&str],
        key: &str,
        value: &str,
    ) -> Result<(), VivariumError> {
        let path = format!("{MARKS_DIR}/{key}");
        self.copy_in(id, &[(path, format!("{value}\n").into_bytes())])
    }

    fn start(&self, id: &str) -> Result<(), VivariumError> {
        self.require(&["start", id]).map(drop)
    }

    fn stop(&self, id: &str) -> Result<(), VivariumError> {
        self.require(&["stop", "-t", STOP_GRACE_SECS, id]).map(drop)
    }

    fn exec(&self, id: &str, script: &str, timeout_secs: u32) -> Result<ExecOutput, VivariumError> {
        let args = ["exec", id, "bash", "-c", script];
        let wait = Duration::from_secs(u64::from(timeout_secs) + 15);
        let out = self.call(&args, None, wait)?;
        // docker's own failure (a container that is not running) is not the
        // script's exit code.
        if !out.succeeded() && out.stderr.contains("Error response from daemon") {
            return Err(failure(&["exec"], &out));
        }
        Ok(ExecOutput {
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: out.stderr,
            exit_code: i64::from(out.code),
        })
    }

    fn read_stopped(
        &self,
        id: &str,
        paths: &[&str],
    ) -> Result<Option<BTreeMap<String, Vec<u8>>>, VivariumError> {
        let mut files = BTreeMap::new();
        for path in paths {
            let Some(bytes) = self.copy_out(id, path)? else {
                continue;
            };
            if let Some(file) = tar::read(&bytes)?.into_iter().next() {
                files.insert((*path).to_string(), file.content);
            }
        }
        Ok(Some(files))
    }
}

#[cfg(test)]
#[path = "docker_test.rs"]
mod tests;
