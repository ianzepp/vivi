//! The `ssh` command line as the pool provider: a slot is a directory on a
//! host reachable over SSH, and the agent runs there as the login user.
//!
//! This is the minimal form of a pool host. It needs bash, git and tar on the
//! host and nothing else, and it gives no isolation: the slot's scripts and
//! the agent have the login user's access to everything that user can reach.
//! See `ssh-slot.sh` for the layout of a slot directory and what each remote
//! verb does.
//!
//! - A slot's facts are files in its directory (marks, the claim, the boot
//!   process id, the last exit code); nothing is kept locally.
//! - "Running" is a live process carrying the slot's tag in its environment;
//!   stopping ends everything that carries it.
//! - The claim is a hard link of a complete file, which fails if the claim
//!   exists, so two claimants on different machines never both win.
//! - The build directory is a plain directory that persists, so there is no
//!   memory disk and nothing to pack or restore.
//! - Harness environment values are never sent: the job reads the names from
//!   the login environment of the host at the time it runs.
//! - Calls share one SSH connection (see [`SshCli`]).

use std::collections::BTreeMap;
use std::process::Command;
use std::time::Duration;

use super::config::PoolConfig;
use super::docker::slot_files;
use super::docker_cli::{CALL_TIMEOUT, RunOutput, Runner};
use super::docker_tar as tar;
use super::doctor::Check;
use super::job::sh_quote;
use super::provider::{CreateSpec, ExecOutput, Hold, MachineRecord, PoolProvider};
use super::slot::{META_MAILSPACE, META_POOL, META_SLOT};
use super::ssh_cli::SshCli;
use super::ssh_doctor::Probe;
use super::ssh_script::command_line;
use super::ssh_state::{self as state, is_slot_id, parse_listing};
use crate::error::VivariumError;

/// The slot root as the pool's scripts and tasks spell it; a slot maps it to
/// its own directory.
const LOGICAL_ROOT: &str = "/work/";
/// ssh's own exit code for "could not connect".
const SSH_FAILED: i32 = 255;

/// Pool provider backed by the `ssh` binary.
pub struct SshProvider {
    runner: Box<dyn Runner>,
    host: String,
    root: String,
}

/// The version line of the local `ssh` client.
///
/// # Errors
/// Returns the reason `ssh` could not be run.
pub fn client_version() -> Result<String, String> {
    // `ssh -V` prints its version on standard error.
    let out = Command::new("ssh")
        .arg("-V")
        .output()
        .map_err(|e| format!("cannot run `ssh`: {e}"))?;
    let text =
        String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    Ok(text.lines().next().unwrap_or_default().trim().to_string())
}

impl SshProvider {
    #[must_use]
    pub fn new(config: &PoolConfig) -> Self {
        let host = config.ssh.host.clone().unwrap_or_default();
        let runner = SshCli::new(&host);
        Self::with_runner(config, Box::new(runner))
    }

    /// A provider that talks through `runner` (tests). The runner gets one
    /// argument: the remote command line.
    #[must_use]
    pub fn with_runner(config: &PoolConfig, runner: Box<dyn Runner>) -> Self {
        Self {
            runner,
            host: config.ssh.host.clone().unwrap_or_default(),
            root: config.ssh.root_or_default().to_string(),
        }
    }

    pub(super) fn host(&self) -> &str {
        &self.host
    }

    pub(super) fn root(&self) -> &str {
        &self.root
    }

    /// One call of the remote script; a failing exit code is returned, not
    /// raised.
    fn call(
        &self,
        (verb, args, login): (&str, &[&str], bool),
        stdin: Option<&[u8]>,
        timeout: Duration,
    ) -> Result<RunOutput, VivariumError> {
        let line = command_line(&self.root, verb, args, login);
        self.runner.run(&[line], stdin, timeout)
    }

    /// One call that must succeed.
    fn require(
        &self,
        verb: &str,
        args: &[&str],
        stdin: Option<&[u8]>,
    ) -> Result<RunOutput, VivariumError> {
        let out = self.call((verb, args, false), stdin, CALL_TIMEOUT)?;
        if out.succeeded() {
            Ok(out)
        } else {
            Err(self.failure(verb, &out))
        }
    }

    /// The error for a failed call: where it ran and what the host said.
    fn failure(&self, verb: &str, out: &RunOutput) -> VivariumError {
        let said = out.stderr.trim().replace("vivi-ssh: ", "");
        if out.code == SSH_FAILED {
            VivariumError::Message(format!("cannot reach {} over ssh: {said}", self.host))
        } else {
            VivariumError::Message(format!("`{verb}` failed on {}: {said}", self.host))
        }
    }

    /// The probe of the doctor: one login-shell call.
    pub(super) fn probe(&self) -> Result<Probe, String> {
        let out = self
            .call(("probe", &[], true), None, CALL_TIMEOUT)
            .map_err(|e| e.to_string())?;
        if out.succeeded() {
            Ok(Probe::parse(&out.stdout_text()))
        } else {
            Err(self.failure("probe", &out).to_string())
        }
    }

    /// The names among `names` that the login environment of the host lacks.
    pub(super) fn unset_harness_env(&self, names: &[String]) -> Result<Vec<String>, VivariumError> {
        if names.is_empty() {
            return Ok(Vec::new());
        }
        let list = names.join(" ");
        let out = self.call(("envgaps", &[&list], true), None, CALL_TIMEOUT)?;
        if !out.succeeded() {
            return Err(self.failure("envgaps", &out));
        }
        // A login profile may print; only the names asked about count.
        let said = out.stdout_text();
        Ok(names
            .iter()
            .filter(|n| said.lines().any(|l| l.trim() == n.as_str()))
            .cloned()
            .collect())
    }

    /// The new slot as a tar stream: marks, the environment file, the files
    /// of the spec and the one-time secrets, all below the slot directory.
    fn new_slot_archive(spec: &CreateSpec) -> Result<Vec<u8>, VivariumError> {
        let mut files: Vec<(String, Vec<u8>)> = Vec::new();
        for (key, value) in &spec.metadata {
            one_line(value)?;
            files.push((format!("marks/{key}"), format!("{value}\n").into_bytes()));
        }
        let mut env = String::new();
        for (key, value) in &spec.env {
            env.push_str(key);
            env.push('=');
            env.push_str(&sh_quote(value));
            env.push('\n');
        }
        files.push(("slot.env".to_string(), env.into_bytes()));
        for (path, content) in slot_files(spec) {
            files.push((relative(&path)?, content));
        }
        tar::build(&files)
    }
}

/// A path as the pool spells it (under `/work`), relative to the slot
/// directory.
pub(super) fn relative(path: &str) -> Result<String, VivariumError> {
    match path.strip_prefix(LOGICAL_ROOT) {
        Some(rel) if !rel.is_empty() && !rel.split('/').any(|p| p == ".." || p.is_empty()) => {
            Ok(rel.to_string())
        }
        _ => Err(VivariumError::Message(format!(
            "path {path} is not inside the slot (expected {LOGICAL_ROOT}...)"
        ))),
    }
}

/// Values that become one line of a file: no line break.
pub(super) fn one_line(value: &str) -> Result<(), VivariumError> {
    if value.contains(['\n', '\r', '\0']) {
        return Err(VivariumError::Message(
            "a slot value must be a single line".into(),
        ));
    }
    Ok(())
}

fn slot_id_of(id: &str) -> Result<&str, VivariumError> {
    if is_slot_id(id) {
        Ok(id)
    } else {
        Err(VivariumError::Message(format!("not a slot id: {id}")))
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
    slot_id_of(&id).map(str::to_string)
}

impl PoolProvider for SshProvider {
    /// The root is made by the first `create`; nothing to make beforehand.
    fn ensure_home(&self) -> Result<Option<String>, VivariumError> {
        Ok(None)
    }

    fn doctor(&self, harness_env: &[String]) -> Vec<Check> {
        self.doctor_checks(harness_env)
    }

    fn harness_env_gaps(&self, names: &[String]) -> Result<Vec<String>, VivariumError> {
        self.unset_harness_env(names)
    }

    /// The values are read when the job runs, so a dispatch can still be told.
    fn env_gaps_at_dispatch(&self, names: &[String]) -> Result<Vec<String>, VivariumError> {
        self.unset_harness_env(names)
    }

    fn env_gap_hint(&self, missing: &[String]) -> String {
        format!(
            "warning: {} not set in the login environment of {}. Jobs on ssh slots read the harness environment from a login shell of the login user when they run; no value passes through Vivi. Export {} in ~/.profile on that host (a non-interactive login skips ~/.bashrc).",
            missing.join(", "),
            self.host,
            missing.join(", ")
        )
    }

    fn live_test_notice(&self) -> String {
        format!(
            "it makes two slot directories under {} on {}, runs stand-in jobs there as the login user, and removes them afterwards (Ctrl-C stops it and cleans up)",
            self.root, self.host
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
        let out = self.require("list", &[], None)?;
        parse_listing(&String::from_utf8_lossy(&out.stdout), &self.host)
    }

    fn create(&self, spec: &CreateSpec) -> Result<String, VivariumError> {
        let id = new_slot_id(spec)?;
        let archive = Self::new_slot_archive(spec)?;
        self.require("create", &[&id], Some(&archive))?;
        if let Err(e) = self.start(&id) {
            if let Err(undo) = self.destroy(&id) {
                eprintln!("warning: could not remove {}: {undo}", spec.name);
            }
            return Err(e);
        }
        Ok(id)
    }

    fn destroy(&self, id: &str) -> Result<(), VivariumError> {
        self.require("destroy", &[slot_id_of(id)?], None).map(drop)
    }

    fn destroy_command(&self, id: &str) -> String {
        format!(
            "ssh {} rm -rf {}/{id}",
            self.host,
            self.root.trim_end_matches('/')
        )
    }

    fn machine(&self, id: &str) -> Result<MachineRecord, VivariumError> {
        let out = self.require("machine", &[slot_id_of(id)?], None)?;
        let found = parse_listing(&String::from_utf8_lossy(&out.stdout), &self.host)?;
        Ok(found
            .into_iter()
            .next()
            .unwrap_or_else(|| MachineRecord::gone(id)))
    }

    /// The claim is a hard link of a complete file, made on the host.
    fn claim(&self, id: &str, task: &str, now: u64) -> Result<Option<Hold>, VivariumError> {
        one_line(task)?;
        let out = self.require("claim", &[slot_id_of(id)?, task, &now.to_string()], None)?;
        let said = out.stdout_text();
        match said.split_whitespace().collect::<Vec<_>>().as_slice() {
            ["claimed", nonce] => Ok(Some(Hold {
                nonce: (*nonce).to_string(),
            })),
            ["taken" | "busy"] => Ok(None),
            ["gone"] => Err(VivariumError::Message(format!("slot {id} is gone"))),
            _ => Err(VivariumError::Message(format!(
                "unexpected answer to a claim of {id}: {said}"
            ))),
        }
    }

    fn unclaim(&self, id: &str) -> Result<(), VivariumError> {
        self.require("unclaim", &[slot_id_of(id)?], None).map(drop)
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
        let rel = relative(path)?;
        self.require("put", &[slot_id_of(id)?, &rel, &hold.nonce], Some(content))
            .map(drop)
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
        one_line(value)?;
        self.require("seal", &[slot_id_of(id)?, key, value], None)
            .map(drop)
    }

    fn start(&self, id: &str) -> Result<(), VivariumError> {
        self.require("start", &[slot_id_of(id)?], None).map(drop)
    }

    fn stop(&self, id: &str) -> Result<(), VivariumError> {
        self.require("stop", &[slot_id_of(id)?], None).map(drop)
    }

    fn exec(&self, id: &str, script: &str, timeout_secs: u32) -> Result<ExecOutput, VivariumError> {
        let wait = Duration::from_secs(u64::from(timeout_secs) + 15);
        let out = self.call(("exec", &[slot_id_of(id)?, script], false), None, wait)?;
        if out.code == SSH_FAILED {
            return Err(self.failure("exec", &out));
        }
        Ok(ExecOutput {
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: out.stderr,
            exit_code: i64::from(out.code),
        })
    }

    /// Any slot can be read without starting it: it is a directory.
    fn read_stopped(
        &self,
        id: &str,
        paths: &[&str],
    ) -> Result<Option<BTreeMap<String, Vec<u8>>>, VivariumError> {
        let rels: Vec<String> = paths
            .iter()
            .map(|p| relative(p))
            .collect::<Result<_, _>>()?;
        let out = self.require("read", &[slot_id_of(id)?, &rels.join(" ")], None)?;
        let mut files = BTreeMap::new();
        for file in tar::read(&out.stdout)? {
            let name = file.path.trim_start_matches("./");
            if let Some((path, _)) = paths.iter().zip(&rels).find(|(_, r)| r.as_str() == name) {
                files.insert((*path).to_string(), file.content);
            }
        }
        Ok(Some(files))
    }
}

#[cfg(test)]
#[path = "ssh_test.rs"]
mod tests;
