//! Settings a provider needs before its pool can create machines, and that
//! the pool table does not set yet.

use super::PoolConfig;

const FLY_APP_REQUIRED: &str = "fly.app is required for a fly pool and has no default (Fly app names are unique across all Fly accounts); set `app = \"<your-unique-app-name>\"` in [fly]";
const SSH_HOST_REQUIRED: &str =
    "ssh.host is required (user@server, or a Host alias from ~/.ssh/config)";
const RUNPOD_KEY_REQUIRED: &str = "runpod.api_key_file is required (the file that holds the RunPod API key; Vivi hands its path to runpodctl and never reads it)";

/// A setting a pool's provider needs before it can create machines. See
/// [`PoolConfig::missing_settings`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingSetting {
    /// The config key, `pools.<pool>.<key>`.
    pub item: String,
    pub message: String,
}

impl PoolConfig {
    /// The settings this pool's provider needs before it can create machines
    /// and that the table does not set yet. A pool without them can still be
    /// added, listed, checked and destroyed, so `vivi pool add` can make the
    /// pool first and `vivi pool set` fill them in.
    #[must_use]
    pub fn missing_settings(&self, pool: &str) -> Vec<MissingSetting> {
        let mut missing = Vec::new();
        let mut need = |key: &str, message: &str| {
            missing.push(MissingSetting {
                item: format!("pools.{pool}.{key}"),
                message: message.to_string(),
            });
        };
        if matches!(self.provider.as_str(), "docker" | "fly" | "runpod")
            && self.image.trim().is_empty()
        {
            need(
                "image",
                "image is required (a container image name, no spaces)",
            );
        }
        match self.provider.as_str() {
            "fly" if self.fly.app.is_none() => need("fly.app", FLY_APP_REQUIRED),
            "ssh" if self.ssh.host.is_none() => need("ssh.host", SSH_HOST_REQUIRED),
            "runpod" if self.runpod.api_key_file.is_none() => {
                need("runpod.api_key_file", RUNPOD_KEY_REQUIRED);
            }
            "worker" => self.worker.missing(&mut need),
            _ => {}
        }
        missing
    }
}
