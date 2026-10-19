//! `flyctl` as the pool provider.
//!
//! One subprocess per call. The secret-bearing create call never echoes its
//! command line, and any error text is scrubbed of the environment values.

use std::process::Command;

use serde::Deserialize;

use super::claim::CLAIM_TTL_SECS;
use super::config::PoolFly;
use super::doctor::Check;
use super::fly_api::FlyApi;
use super::provider::{CreateSpec, ExecOutput, Hold, MachineRecord, PoolProvider};
use super::slot::{self, META_CLAIMED, META_TASK};
use crate::error::VivariumError;

/// Pool provider backed by the `fly` binary for list, create and destroy, and
/// by the Machines API ([`FlyApi`]) for everything that acts on one machine.
#[derive(Debug, Clone)]
pub struct FlyctlProvider {
    bin: String,
    pub(super) app: String,
    /// Where new machines go and how big they are, from `[pool.fly]`.
    host: PoolFly,
    api: FlyApi,
}

impl FlyctlProvider {
    #[must_use]
    pub fn new(app: &str, host: &PoolFly) -> Self {
        Self {
            bin: "fly".to_string(),
            app: app.to_string(),
            host: host.clone(),
            api: FlyApi::new(app),
        }
    }

    /// The names (never the values) of the application's secrets.
    pub(super) fn secret_names(&self) -> Result<Vec<String>, VivariumError> {
        let args = ["secrets", "list", "--json", "-a", &self.app].map(str::to_string);
        let (stdout, _) = self.run(&args, &[])?;
        parse_secret_names(&stdout)
    }

    /// Run one `fly` call and report stdout, stderr and whether it succeeded,
    /// so a caller can recover from a partial success without losing the text.
    pub(super) fn spawn(&self, args: &[String]) -> Result<(String, String, bool), VivariumError> {
        let output = Command::new(&self.bin).args(args).output().map_err(|e| {
            VivariumError::Message(format!(
                "failed to run `{}` (is flyctl installed and on PATH?): {e}",
                self.bin
            ))
        })?;
        Ok((
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
            output.status.success(),
        ))
    }

    pub(super) fn run(
        &self,
        args: &[String],
        secrets: &[&str],
    ) -> Result<(String, String), VivariumError> {
        let (stdout, stderr, ok) = self.spawn(args)?;
        if !ok {
            return Err(self.failure(args, &stderr, secrets));
        }
        Ok((stdout, stderr))
    }

    /// The error one failed `fly` call says: the verb, then scrubbed stderr.
    fn failure(&self, args: &[String], stderr: &str, secrets: &[&str]) -> VivariumError {
        let verb = args.iter().take(2).cloned().collect::<Vec<_>>().join(" ");
        VivariumError::Message(format!(
            "`{} {verb}` failed: {}",
            self.bin,
            scrub(stderr.trim(), secrets)
        ))
    }
}

impl PoolProvider for FlyctlProvider {
    fn ensure_home(&self) -> Result<Option<String>, VivariumError> {
        let args = ["apps", "list", "--json"].map(str::to_string);
        let (stdout, _) = self.run(&args, &[])?;
        if parse_app_names(&stdout)?.contains(&self.app) {
            return Ok(None);
        }
        let org = self.host.organization_name();
        let args = ["apps", "create", &self.app, "--org", org].map(str::to_string);
        self.run(&args, &[]).map_err(|e| name_taken(&self.app, e))?;
        Ok(Some(format!(
            "created app {} in organization {org}",
            self.app
        )))
    }

    fn doctor(&self, harness_env: &[String]) -> Vec<Check> {
        self.doctor_checks(harness_env)
    }

    fn harness_env_gaps(&self, names: &[String]) -> Result<Vec<String>, VivariumError> {
        let have = self.secret_names()?;
        Ok(names
            .iter()
            .filter(|n| !have.contains(n))
            .cloned()
            .collect())
    }

    fn env_gaps_at_dispatch(&self, names: &[String]) -> Result<Vec<String>, VivariumError> {
        self.harness_env_gaps(names)
    }

    fn supplies_build_mount(&self) -> bool {
        false
    }

    fn build_persists(&self) -> bool {
        false
    }

    fn runs_unprivileged(&self) -> bool {
        false
    }

    /// A stopped machine's disk is only reachable by starting it.
    fn read_stopped(
        &self,
        _id: &str,
        _paths: &[&str],
    ) -> Result<Option<std::collections::BTreeMap<String, Vec<u8>>>, VivariumError> {
        Ok(None)
    }

    fn env_gap_hint(&self, missing: &[String]) -> String {
        let assignments = missing
            .iter()
            .map(|name| format!("{name}=<value>"))
            .collect::<Vec<_>>()
            .join(" ");
        format!(
            "warning: app {} has no secret named {}. The harness gets these from the machine's environment, so a job will run without them unless the image sets them. Set them yourself (vivi never reads or sets secret values):\n  fly secrets set -a {} {assignments}",
            self.app,
            missing.join(", "),
            self.app
        )
    }

    fn live_test_notice(&self) -> String {
        "it creates two small billable machines for a few minutes (Ctrl-C stops it and cleans up)"
            .to_string()
    }

    fn list(&self) -> Result<Vec<MachineRecord>, VivariumError> {
        let args = ["machine", "list", "--json", "-a", &self.app].map(str::to_string);
        let (stdout, _) = self.run(&args, &[])?;
        parse_machine_list(&stdout)
    }

    /// `fly machine run --detach` can fail *after* the machine exists: with a
    /// large image flyctl gives up waiting around 60s though the machine keeps
    /// unpacking and starts on its own. `machine run` has no wait-timeout flag,
    /// so a start-wait failure is read as "still starting": the machine id is
    /// recovered and the existing slot wait loop watches it to a clean stop.
    fn create(&self, spec: &CreateSpec) -> Result<String, VivariumError> {
        let secrets: Vec<&str> = spec.once_env.values().map(String::as_str).collect();
        let args = run_args(&self.app, &self.host, spec);
        let (stdout, stderr, ok) = self.spawn(&args)?;
        let output = format!("{stdout}\n{stderr}");
        let missing = || {
            VivariumError::Message(format!(
                "could not find the machine id in flyctl output for {}",
                spec.name
            ))
        };
        if ok {
            return parse_run_output(&output).ok_or_else(missing);
        }
        if is_start_wait_failure(&output) {
            let machines = self.list().unwrap_or_default();
            if let Some(id) = salvaged_id(&spec.name, &output, &machines) {
                eprintln!(
                    "warning: flyctl gave up waiting for {} to start; watching machine {id} (a large image takes minutes to unpack)",
                    spec.name
                );
                return Ok(id);
            }
        }
        Err(self.failure(&args, &stderr, &secrets))
    }

    fn destroy(&self, id: &str) -> Result<(), VivariumError> {
        let args = ["machine", "destroy", "--force", id, "-a", &self.app].map(str::to_string);
        self.run(&args, &[]).map(|_| ())
    }

    fn destroy_command(&self, id: &str) -> String {
        format!("fly machine destroy --force {id} -a {}", self.app)
    }

    fn machine(&self, id: &str) -> Result<MachineRecord, VivariumError> {
        self.api.machine(id)
    }

    /// A machine lease serialises the check-and-mark; the marks are machine
    /// metadata and outlive the lease.
    fn claim(&self, id: &str, task: &str, now: u64) -> Result<Option<Hold>, VivariumError> {
        let Some(hold) = self.api.lease(id, CLAIM_TTL_SECS)? else {
            return Ok(None);
        };
        // The list may be stale: re-read under the lease before marking.
        let fresh = self.api.machine(id);
        if !fresh.as_ref().is_ok_and(slot::is_free) {
            self.api.release(id, &hold)?;
            fresh?;
            return Ok(None);
        }
        let marked = self
            .api
            .set_metadata(id, META_TASK, task)
            .and_then(|()| self.api.set_metadata(id, META_CLAIMED, &now.to_string()));
        if let Err(e) = marked {
            // Every step is attempted; a mark left behind is a stale claim.
            let (unmarked, freed) = (self.unclaim(id), self.api.release(id, &hold));
            if let Err(undo) = unmarked.and(freed) {
                eprintln!("warning: could not fully undo the claim: {undo}");
            }
            return Err(e);
        }
        Ok(Some(hold))
    }

    fn unclaim(&self, id: &str) -> Result<(), VivariumError> {
        let task = self.api.clear_metadata(id, META_TASK);
        task.and(self.api.clear_metadata(id, META_CLAIMED))
    }

    fn release(&self, id: &str, hold: &Hold) -> Result<(), VivariumError> {
        self.api.release(id, hold)
    }

    fn put_file(
        &self,
        id: &str,
        hold: &Hold,
        path: &str,
        content: &[u8],
    ) -> Result<(), VivariumError> {
        self.api.put_file(id, hold, path, content)
    }

    fn seal(
        &self,
        id: &str,
        drop_env: &[&str],
        key: &str,
        value: &str,
    ) -> Result<(), VivariumError> {
        self.api.seal(id, drop_env, key, value)
    }

    fn start(&self, id: &str) -> Result<(), VivariumError> {
        self.api.start(id)
    }

    fn stop(&self, id: &str) -> Result<(), VivariumError> {
        self.api.stop(id)
    }

    fn exec(&self, id: &str, script: &str, timeout_secs: u32) -> Result<ExecOutput, VivariumError> {
        self.api.exec(id, script, timeout_secs)
    }
}

/// The error for a failed `fly apps create`, in plain words when the cause is
/// that the name is taken (Fly app names are unique across all accounts).
fn name_taken(app: &str, error: VivariumError) -> VivariumError {
    if !is_name_taken(&error.to_string()) {
        return error;
    }
    VivariumError::Message(format!(
        "the Fly app name \"{app}\" is already taken (Fly app names are unique across all Fly accounts). Choose another name in .vivi/mailspace.toml: set `app = \"<your-unique-app-name>\"` in the pool's `[pools.<pool>.fly]` table"
    ))
}

/// True when `flyctl` text says the app name is already in use.
#[must_use]
pub fn is_name_taken(text: &str) -> bool {
    let lower = text.to_lowercase();
    ["already been taken", "already taken", "name is taken"]
        .iter()
        .any(|needle| lower.contains(needle))
}

/// Arguments for `fly machine run`. `--file-literal` takes the raw text (its
/// help says base64, but flyctl encodes the value itself).
/// Root disk persists and the machine never
/// restarts on its own, so a finished machine is a stopped (free) slot.
#[must_use]
pub fn run_args(app: &str, host: &PoolFly, spec: &CreateSpec) -> Vec<String> {
    let mut args: Vec<String> = [
        "machine",
        "run",
        spec.image.as_str(),
        "--detach",
        "--app",
        app,
        "--region",
        &host.region,
        "--vm-size",
        &host.size,
        "--vm-memory",
        &host.memory.to_string(),
        "--restart",
        "no",
        "--rootfs-persist",
        "always",
        "--name",
        &spec.name,
    ]
    .map(str::to_string)
    .to_vec();
    if let Some(size) = host.rootfs_gb {
        args.extend(["--rootfs-size".to_string(), size.to_string()]);
    }
    for (key, value) in &spec.metadata {
        args.extend(["--metadata".to_string(), format!("{key}={value}")]);
    }
    for (key, value) in spec.env.iter().chain(&spec.once_env) {
        args.extend(["--env".to_string(), format!("{key}={value}")]);
    }
    for (path, content) in &spec.files {
        args.extend(["--file-literal".to_string(), format!("{path}={content}")]);
    }
    args.push("--".to_string());
    args.extend(spec.command.iter().cloned());
    args
}

/// Pull the machine id from `fly machine run` output (`Machine ID: <id>`).
#[must_use]
pub fn parse_run_output(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let rest = line.trim().strip_prefix("Machine ID:")?;
        let id = rest.trim();
        (!id.is_empty()).then(|| id.to_string())
    })
}

/// True when `flyctl` text says it stopped waiting for the machine to start,
/// rather than that the machine could not be made.
#[must_use]
pub fn is_start_wait_failure(text: &str) -> bool {
    text.contains("failed to reach desired start state")
}

/// The id of a machine `fly machine run` made but gave up waiting on: from the
/// run output when it got that far, else from the listing by name (`--name`
/// makes that deterministic).
#[must_use]
pub fn salvaged_id(spec_name: &str, output: &str, machines: &[MachineRecord]) -> Option<String> {
    parse_run_output(output).or_else(|| {
        machines
            .iter()
            .find(|m| m.name == spec_name)
            .map(|m| m.id.clone())
    })
}

fn scrub(text: &str, secrets: &[&str]) -> String {
    secrets
        .iter()
        .filter(|s| s.len() >= 8)
        .fold(text.to_string(), |acc, s| acc.replace(s, "<redacted>"))
}

#[derive(Debug, Deserialize)]
struct RawApp {
    #[serde(default, rename = "Name")]
    name: String,
}

/// Parse `fly apps list --json` into app names.
///
/// # Errors
/// Returns an error if the text is not the expected JSON array.
pub fn parse_app_names(json: &str) -> Result<Vec<String>, VivariumError> {
    let raw: Vec<RawApp> = serde_json::from_str(json)
        .map_err(|e| VivariumError::Parse(format!("unexpected `fly apps list` output: {e}")))?;
    Ok(raw.into_iter().map(|a| a.name).collect())
}

#[derive(Debug, Deserialize)]
struct RawSecret {
    #[serde(default)]
    name: String,
}

/// Parse `fly secrets list --json` into secret names. Only the name field is
/// read; no value is ever part of that output or of this result.
///
/// # Errors
/// Returns an error if the text is not the expected JSON array.
pub fn parse_secret_names(json: &str) -> Result<Vec<String>, VivariumError> {
    let raw: Vec<RawSecret> = serde_json::from_str(json)
        .map_err(|e| VivariumError::Parse(format!("unexpected `fly secrets list` output: {e}")))?;
    Ok(raw.into_iter().map(|s| s.name).collect())
}

#[derive(Debug, Deserialize)]
pub(super) struct RawMachine {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    region: String,
    #[serde(default)]
    created_at: String,
    #[serde(default)]
    config: RawConfig,
    #[serde(default)]
    events: Vec<RawEvent>,
}

#[derive(Debug, Default, Deserialize)]
struct RawConfig {
    #[serde(default)]
    metadata: Option<std::collections::BTreeMap<String, String>>,
}

#[derive(Debug, Deserialize)]
struct RawEvent {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    request: serde_json::Value,
}

/// Parse `fly machine list --json` into records. Only the fields the pool
/// uses are read, so the environment (which can hold a token) is never kept.
///
/// # Errors
/// Returns an error if the text is not the expected JSON array.
pub fn parse_machine_list(json: &str) -> Result<Vec<MachineRecord>, VivariumError> {
    let raw: Vec<RawMachine> = serde_json::from_str(json)
        .map_err(|e| VivariumError::Parse(format!("unexpected `fly machine list` output: {e}")))?;
    Ok(raw.into_iter().map(record_from_raw).collect())
}

/// The pool's view of one raw machine record.
pub(super) fn record_from_raw(m: RawMachine) -> MachineRecord {
    MachineRecord {
        exit_code: m
            .events
            .iter()
            .find(|e| e.kind == "exit")
            .and_then(|e| e.request.pointer("/exit_event/exit_code"))
            .and_then(serde_json::Value::as_i64),
        id: m.id,
        name: m.name,
        state: m.state,
        region: m.region,
        created_at: m.created_at,
        metadata: m.config.metadata.unwrap_or_default(),
    }
}

#[cfg(test)]
#[path = "fly_test.rs"]
mod tests;
