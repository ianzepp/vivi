//! What `runpodctl` says about pods, the account and the GPU market, and the
//! argument lists that ask. Shapes are those the CLI prints with `--output
//! json` (the default); fields not listed here are ignored.

use serde::Deserialize;

use super::runpod_cli::CREATE_WAIT;
use super::runpod_config::PoolRunpod;
use crate::error::VivariumError;

/// One pod as the CLI prints it (a `pod list` row, or `pod get` / `pod
/// create` output, which hold more).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pod {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub desired_status: String,
    #[serde(default)]
    pub runtime_status: String,
    #[serde(default)]
    pub cost_per_hr: Option<f64>,
    #[serde(default)]
    pub gpu_count: u32,
    #[serde(default)]
    pub image_name: String,
    /// `2026-08-27 13:29:30.172 +0000 UTC` (`pod get` and `pod create` only).
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub machine: Option<Machine>,
    #[serde(default)]
    pub ssh: Option<Ssh>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Machine {
    #[serde(default)]
    pub gpu_id: String,
    #[serde(default)]
    pub secure_cloud: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Ssh {
    #[serde(default)]
    pub ip: String,
    #[serde(default)]
    pub port: u16,
}

impl Pod {
    /// The state word the pool engine reads: `started`, `starting`,
    /// `stopped`, `destroyed` or `unknown`. A stopped pod keeps nothing (its
    /// container disk is gone), so `stopped` is as final as `destroyed` for a
    /// result.
    #[must_use]
    pub fn state(&self) -> &'static str {
        match (self.runtime_status.as_str(), self.desired_status.as_str()) {
            ("running", _) => "started",
            ("initializing", _) | ("unknown" | "", "RUNNING") => "starting",
            ("stopped", _) | ("unknown" | "", "EXITED") => "stopped",
            ("terminated", _) | ("unknown" | "", "TERMINATED") => "destroyed",
            _ => "unknown",
        }
    }

    /// The SSH endpoint, once the provider has mapped the port.
    #[must_use]
    pub fn endpoint(&self) -> Option<(&str, u16)> {
        let ssh = self
            .ssh
            .as_ref()
            .filter(|s| !s.ip.is_empty() && s.port != 0)?;
        Some((ssh.ip.as_str(), ssh.port))
    }

    /// The hardware as one line: `NVIDIA A100-SXM4-80GB x1, $1.59/h`.
    #[must_use]
    pub fn hardware(&self) -> String {
        let gpu = self
            .machine
            .as_ref()
            .map(|m| m.gpu_id.as_str())
            .filter(|g| !g.is_empty())
            .unwrap_or("unknown GPU");
        let cost = self
            .cost_per_hr
            .map_or_else(String::new, |c| format!(", ${c:.2}/h"));
        format!("{gpu} x{}{cost}", self.gpu_count)
    }

    /// When the provider made the pod, in epoch milliseconds, if it said.
    #[must_use]
    pub fn created_ms(&self) -> Option<i64> {
        let text = self.created_at.trim().trim_end_matches(" UTC");
        chrono::DateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S%.f %z")
            .into_iter()
            .next()
            .map(|t| t.timestamp_millis())
    }
}

/// True for the id shape the provider hands out (and the only ids put in an
/// argument list).
#[must_use]
pub fn is_pod_id(id: &str) -> bool {
    (6..=40).contains(&id.len())
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

fn cli_error(what: &str, json: &str) -> VivariumError {
    // The CLI prints failures as `{"error": "...", "code": "..."}`.
    let said = serde_json::from_str::<serde_json::Value>(json)
        .into_iter()
        .find_map(|v| v.get("error")?.as_str().map(str::to_string))
        .unwrap_or_else(|| json.chars().take(200).collect());
    VivariumError::Parse(format!("unexpected `runpodctl {what}` output: {said}"))
}

/// Parse `pod list` into pods.
///
/// # Errors
/// Returns an error if the text is not a JSON array of pods.
pub fn parse_pods(json: &str) -> Result<Vec<Pod>, VivariumError> {
    serde_json::from_str(json).map_err(|_| cli_error("pod list", json))
}

/// Parse `pod get` / `pod create` output into one pod.
///
/// # Errors
/// Returns an error if the text is not a JSON pod with an id.
pub fn parse_pod(json: &str) -> Result<Pod, VivariumError> {
    let pod: Pod = serde_json::from_str(json).map_err(|_| cli_error("pod", json))?;
    if pod.id.is_empty() {
        return Err(cli_error("pod", json));
    }
    Ok(pod)
}

/// The account, as far as the doctor reads it: who, and what is owed or
/// being spent.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct User {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub client_balance: Option<f64>,
    #[serde(default)]
    pub current_spend_per_hr: Option<f64>,
}

/// Parse `user` output.
///
/// # Errors
/// Returns an error if the text is not a JSON account with an id.
pub fn parse_user(json: &str) -> Result<User, VivariumError> {
    let user: User = serde_json::from_str(json).map_err(|_| cli_error("user", json))?;
    if user.id.is_empty() {
        return Err(cli_error("user", json));
    }
    Ok(user)
}

/// One GPU type of `gpu list`: price and stock.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GpuType {
    pub gpu_id: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub secure_cloud: bool,
    #[serde(default)]
    pub community_cloud: bool,
    #[serde(default)]
    pub secure_price_per_hr: Option<f64>,
    #[serde(default)]
    pub community_price_per_hr: Option<f64>,
    #[serde(default)]
    pub stock_status: String,
    #[serde(default)]
    pub data_center_availability: Vec<DataCenterStock>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DataCenterStock {
    pub data_center_id: String,
    #[serde(default)]
    pub stock_status: String,
}

impl GpuType {
    /// The price per hour of one GPU in `cloud` (`SECURE` or `COMMUNITY`).
    #[must_use]
    pub fn price(&self, cloud: &str) -> Option<f64> {
        if cloud == "COMMUNITY" {
            self.community_price_per_hr
        } else {
            self.secure_price_per_hr
        }
    }

    /// True unless the listing says this type cannot be had in `cloud` (in
    /// one of `centers` when any are named) right now. It is a hint: stock
    /// changes between the listing and the create.
    #[must_use]
    pub fn in_stock(&self, cloud: &str, centers: &[String]) -> bool {
        let offered = if cloud == "COMMUNITY" {
            self.community_cloud
        } else {
            self.secure_cloud
        };
        let stocked = |s: &str| !s.is_empty() && !s.eq_ignore_ascii_case("none");
        let here = self
            .data_center_availability
            .iter()
            .filter(|d| centers.is_empty() || centers.contains(&d.data_center_id))
            .any(|d| stocked(&d.stock_status));
        offered && here
    }
}

/// Parse `gpu list` output.
///
/// # Errors
/// Returns an error if the text is not a JSON array of GPU types.
pub fn parse_gpus(json: &str) -> Result<Vec<GpuType>, VivariumError> {
    serde_json::from_str(json).map_err(|_| cli_error("gpu list", json))
}

/// Parse `ssh list-keys` output: how many keys the account holds. The CLI
/// prints `{"keys": [...]}`; a bare array is accepted too.
///
/// # Errors
/// Returns an error if the text is neither.
pub fn count_ssh_keys(json: &str) -> Result<usize, VivariumError> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|_| cli_error("ssh list-keys", json))?;
    let keys = value.get("keys").unwrap_or(&value);
    keys.as_array()
        .map(Vec::len)
        .ok_or_else(|| cli_error("ssh list-keys", "the answer holds no list of keys"))
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|a| (*a).to_string()).collect()
}

#[must_use]
pub fn list_args() -> Vec<String> {
    args(&["pod", "list", "--all"])
}

/// # Errors
/// Returns an error if `id` is not shaped like a pod id.
pub fn get_args(id: &str) -> Result<Vec<String>, VivariumError> {
    Ok(vec!["pod".into(), "get".into(), pod_id(id)?])
}

/// # Errors
/// Returns an error if `id` is not shaped like a pod id.
pub fn delete_args(id: &str) -> Result<Vec<String>, VivariumError> {
    Ok(vec!["pod".into(), "delete".into(), pod_id(id)?])
}

fn pod_id(id: &str) -> Result<String, VivariumError> {
    if is_pod_id(id) {
        Ok(id.to_string())
    } else {
        Err(VivariumError::Message(format!("not a RunPod pod id: {id}")))
    }
}

#[must_use]
pub fn user_args() -> Vec<String> {
    args(&["user"])
}

#[must_use]
pub fn gpus_args() -> Vec<String> {
    args(&["gpu", "list"])
}

#[must_use]
pub fn ssh_keys_args() -> Vec<String> {
    args(&["ssh", "list-keys"])
}

/// The arguments of one `pod create`: a fixed list, never a shell line. It
/// waits (up to [`CREATE_WAIT`]) until the pod's SSH port answers, and sets a
/// provider-side deadline `terminate_after` (an ISO time) after which the
/// provider is asked to terminate the pod.
#[must_use]
pub fn create_args(
    config: &PoolRunpod,
    image: &str,
    name: &str,
    gpu_id: &str,
    terminate_after: &str,
) -> Vec<String> {
    let mut list = args(&[
        "pod", "create", "--name", name, "--image", image, "--gpu-id", gpu_id,
    ]);
    list.extend(["--gpu-count".to_string(), config.gpu_count.to_string()]);
    list.extend(["--cloud-type".to_string(), config.cloud_type().to_string()]);
    list.extend([
        "--container-disk-in-gb".to_string(),
        config.container_disk_gb.to_string(),
    ]);
    if let Some(version) = &config.min_cuda_version {
        list.extend(["--min-cuda-version".to_string(), version.clone()]);
    }
    if !config.data_centers.is_empty() {
        list.extend([
            "--data-center-ids".to_string(),
            config.data_centers.join(","),
        ]);
    }
    list.extend(args(&["--ports", "22/tcp", "--ssh"]));
    if config.cloud_type() == "COMMUNITY" {
        // The community cloud maps a public port only on hosts with a public IP.
        list.push("--public-ip".to_string());
    }
    list.extend(["--terminate-after".to_string(), terminate_after.to_string()]);
    list.extend(args(&["--wait", "--wait-timeout", CREATE_WAIT]));
    list
}

#[cfg(test)]
#[path = "runpod_pod_test.rs"]
mod tests;
