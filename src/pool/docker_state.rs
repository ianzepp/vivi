//! What `docker inspect` says about a container, and how it becomes the
//! pool's [`MachineRecord`].
//!
//! A slot's facts come from three places, because container labels cannot
//! change after creation: fixed facts (mailspace, pool, index, base, architecture) are
//! labels; the seed mark is a file on the slot's own disk (see
//! [`MARKS_DIR`]); the task and claim time are the labels of a separate claim
//! container whose existence is the claim (see `docker.rs`).

use std::collections::BTreeMap;

use serde::Deserialize;

use super::provider::MachineRecord;
use super::slot::{META_CLAIMED, META_TASK};
use crate::error::VivariumError;

/// Label naming what a container is to the pool.
pub const LABEL_ROLE: &str = "vivi_pool_role";
pub const ROLE_SLOT: &str = "slot";
pub const ROLE_CLAIM: &str = "claim";
/// Label of a claim container: the slot it claims.
pub const LABEL_CLAIM_OF: &str = "vivi_pool_claim_of";
/// Directory on a slot's disk holding its mutable marks, one file per mark.
pub const MARKS_DIR: &str = "/work/vivi-marks";
/// Length of the container ids the pool uses.
const SHORT_ID: usize = 12;

/// The id the pool uses for a container: the first twelve hex digits.
#[must_use]
pub fn short(id: &str) -> &str {
    &id[..SHORT_ID.min(id.len())]
}

/// Name of the claim container of slot `id`.
#[must_use]
pub fn claim_name(id: &str) -> String {
    format!("vivi-claim-{}", short(id))
}

#[derive(Debug, Deserialize)]
struct RawContainer {
    #[serde(default, rename = "Id")]
    id: String,
    #[serde(default, rename = "Name")]
    name: String,
    #[serde(default, rename = "Created")]
    created: String,
    #[serde(default, rename = "State")]
    state: RawState,
    #[serde(default, rename = "Config")]
    config: RawConfig,
    #[serde(default, rename = "ImageManifestDescriptor")]
    manifest: Option<RawManifest>,
}

/// The manifest of the image the container really runs (newer daemons).
#[derive(Debug, Default, Deserialize)]
struct RawManifest {
    #[serde(default)]
    platform: RawPlatform,
}

#[derive(Debug, Default, Deserialize)]
struct RawPlatform {
    #[serde(default)]
    architecture: String,
}

#[derive(Debug, Default, Deserialize)]
struct RawState {
    #[serde(default, rename = "Status")]
    status: String,
    #[serde(default, rename = "ExitCode")]
    exit_code: i64,
}

// The container's environment is deliberately not declared: it can hold the
// harness keys, and nothing here has a use for it.
#[derive(Debug, Default, Deserialize)]
struct RawConfig {
    #[serde(default, rename = "Labels")]
    labels: Option<BTreeMap<String, String>>,
    #[serde(default, rename = "Image")]
    image: String,
}

/// One inspected container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Container {
    pub id: String,
    pub name: String,
    pub created: String,
    /// Docker's status word: `created`, `running`, `exited`, ...
    pub status: String,
    pub exit_code: i64,
    pub labels: BTreeMap<String, String>,
    pub image: String,
    /// Architecture of the image the container runs, when the daemon says.
    pub arch: Option<String>,
}

impl Container {
    #[must_use]
    pub fn role(&self) -> &str {
        self.labels.get(LABEL_ROLE).map_or("", String::as_str)
    }
}

/// Parse `docker inspect` output (a JSON array).
///
/// # Errors
/// Returns an error if the text is not the expected JSON array.
pub fn parse_inspect(json: &str) -> Result<Vec<Container>, VivariumError> {
    let raw: Vec<RawContainer> = serde_json::from_str(json)
        .map_err(|e| VivariumError::Parse(format!("unexpected `docker inspect` output: {e}")))?;
    Ok(raw
        .into_iter()
        .map(|c| Container {
            id: short(&c.id).to_string(),
            name: c.name.trim_start_matches('/').to_string(),
            created: c.created,
            status: c.state.status,
            exit_code: c.state.exit_code,
            labels: c.config.labels.unwrap_or_default(),
            image: c.config.image,
            arch: c
                .manifest
                .map(|m| m.platform.architecture)
                .filter(|a| !a.is_empty()),
        })
        .collect())
}

/// The pool's state word for a Docker status, in the words the pool engine
/// already uses (`started`, `stopped`, `destroyed`, ...).
#[must_use]
pub fn state_word(status: &str) -> &'static str {
    match status {
        "running" | "paused" => "started",
        "restarting" => "starting",
        "removing" => "stopping",
        "dead" => "failed",
        // `created` is a container that never ran: stopped, with no exit.
        _ => "stopped",
    }
}

/// The exit code worth reporting: only a container that ran and ended has one.
#[must_use]
pub fn exit_code(c: &Container) -> Option<i64> {
    matches!(c.status.as_str(), "exited" | "dead").then_some(c.exit_code)
}

/// The record of a slot container, with `marks` (files on its disk) and the
/// claim container that claims it, if any, folded into its metadata.
#[must_use]
pub fn record(
    slot: &Container,
    marks: &BTreeMap<String, String>,
    claim: Option<&Container>,
    host: &str,
) -> MachineRecord {
    let mut metadata: BTreeMap<String, String> = slot
        .labels
        .iter()
        .filter(|(k, _)| k.starts_with("vivi_pool") && k.as_str() != LABEL_ROLE)
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    metadata.extend(marks.iter().map(|(k, v)| (k.clone(), v.clone())));
    if let Some(claim) = claim {
        for key in [META_TASK, META_CLAIMED] {
            if let Some(value) = claim.labels.get(key) {
                metadata.insert(key.to_string(), value.clone());
            }
        }
    }
    MachineRecord {
        id: slot.id.clone(),
        name: slot.name.clone(),
        state: state_word(&slot.status).to_string(),
        region: host.to_string(),
        created_at: slot.created.clone(),
        metadata,
        exit_code: exit_code(slot),
    }
}

/// The claim container of slot `id` among `containers`.
#[must_use]
pub fn claim_for<'a>(containers: &'a [Container], id: &str) -> Option<&'a Container> {
    containers
        .iter()
        .find(|c| c.role() == ROLE_CLAIM && c.labels.get(LABEL_CLAIM_OF).is_some_and(|of| of == id))
}

#[cfg(test)]
#[path = "docker_state_test.rs"]
mod tests;
