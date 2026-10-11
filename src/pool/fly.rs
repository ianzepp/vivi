//! `flyctl` as the pool provider.
//!
//! One subprocess per call. The secret-bearing create call never echoes its
//! command line, and any error text is scrubbed of the environment values.

use std::process::Command;

use serde::Deserialize;

use super::provider::{CreateSpec, MachineRecord, PoolProvider};
use crate::error::VivariumError;

/// Pool provider backed by the `fly` binary.
#[derive(Debug, Clone)]
pub struct FlyctlProvider {
    bin: String,
    app: String,
}

impl FlyctlProvider {
    #[must_use]
    pub fn new(app: &str) -> Self {
        Self {
            bin: "fly".to_string(),
            app: app.to_string(),
        }
    }

    fn run(&self, args: &[String], secrets: &[&str]) -> Result<(String, String), VivariumError> {
        let output = Command::new(&self.bin).args(args).output().map_err(|e| {
            VivariumError::Message(format!(
                "failed to run `{}` (is flyctl installed and on PATH?): {e}",
                self.bin
            ))
        })?;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        if !output.status.success() {
            let verb = args.iter().take(2).cloned().collect::<Vec<_>>().join(" ");
            return Err(VivariumError::Message(format!(
                "`{} {verb}` failed: {}",
                self.bin,
                scrub(stderr.trim(), secrets)
            )));
        }
        Ok((stdout, stderr))
    }
}

impl PoolProvider for FlyctlProvider {
    fn list(&self) -> Result<Vec<MachineRecord>, VivariumError> {
        let args = ["machine", "list", "--json", "-a", &self.app].map(str::to_string);
        let (stdout, _) = self.run(&args, &[])?;
        parse_machine_list(&stdout)
    }

    fn create(&self, spec: &CreateSpec) -> Result<String, VivariumError> {
        let secrets: Vec<&str> = spec.env.values().map(String::as_str).collect();
        let (stdout, stderr) = self.run(&run_args(&self.app, spec), &secrets)?;
        parse_run_output(&format!("{stdout}\n{stderr}")).ok_or_else(|| {
            VivariumError::Message(format!(
                "could not find the machine id in flyctl output for {}",
                spec.name
            ))
        })
    }

    fn destroy(&self, id: &str) -> Result<(), VivariumError> {
        let args = ["machine", "destroy", "--force", id, "-a", &self.app].map(str::to_string);
        self.run(&args, &[]).map(|_| ())
    }
}

/// Arguments for `fly machine run`. `--file-literal` takes the raw text (its
/// help says base64, but flyctl encodes the value itself).
/// Root disk persists and the machine never
/// restarts on its own, so a finished machine is a stopped (free) slot.
#[must_use]
pub fn run_args(app: &str, spec: &CreateSpec) -> Vec<String> {
    let mut args: Vec<String> = [
        "machine",
        "run",
        spec.image.as_str(),
        "--detach",
        "--app",
        app,
        "--region",
        &spec.region,
        "--vm-size",
        &spec.vm_size,
        "--vm-memory",
        &spec.memory_mb.to_string(),
        "--restart",
        "no",
        "--rootfs-persist",
        "always",
        "--name",
        &spec.name,
    ]
    .map(str::to_string)
    .to_vec();
    for (key, value) in &spec.metadata {
        args.extend(["--metadata".to_string(), format!("{key}={value}")]);
    }
    for (key, value) in &spec.env {
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

fn scrub(text: &str, secrets: &[&str]) -> String {
    secrets
        .iter()
        .filter(|s| s.len() >= 8)
        .fold(text.to_string(), |acc, s| acc.replace(s, "<redacted>"))
}

#[derive(Debug, Deserialize)]
struct RawMachine {
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
    Ok(raw
        .into_iter()
        .map(|m| MachineRecord {
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
        })
        .collect())
}

#[cfg(test)]
#[path = "fly_test.rs"]
mod tests;
