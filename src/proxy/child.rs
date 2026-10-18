//! Spawn, health-check, and restart `kind = "child"` backends.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::process::{Child, Command};

use super::access::{child_stderr_path, child_stdout_path};
use super::config::{BackendKind, ProviderConfig};
use super::forward;
use super::health::HealthTable;
use super::paths::ensure_private_dir;
use crate::VivariumError;

const START_DELAY: Duration = Duration::from_millis(200);
const BACKOFF_CAP: Duration = Duration::from_secs(60);

/// One supervised child process.
struct ChildSlot {
    child: Child,
}

/// Live children keyed by provider name.
#[derive(Default)]
pub struct Children {
    slots: BTreeMap<String, ChildSlot>,
    failures: BTreeMap<String, u32>,
    retry_after: BTreeMap<String, Instant>,
}

impl Children {
    /// Spawn every `kind = child` provider that is not already running.
    pub async fn reconcile(
        &mut self,
        providers: &BTreeMap<String, ProviderConfig>,
        log_dir: &Path,
        client: &reqwest::Client,
        health: &HealthTable,
    ) {
        self.reap();
        for (name, provider) in providers {
            if provider.kind != BackendKind::Child {
                continue;
            }
            if self
                .retry_after
                .get(name)
                .is_some_and(|until| *until > Instant::now())
            {
                continue;
            }
            if self.slots.contains_key(name) {
                probe_running(name, provider, client, health).await;
                continue;
            }
            self.start_one(name, provider, log_dir, health).await;
        }
    }

    async fn start_one(
        &mut self,
        name: &str,
        provider: &ProviderConfig,
        log_dir: &Path,
        health: &HealthTable,
    ) {
        match spawn(name, provider, log_dir).await {
            Ok(child) => {
                health.record(name, "child", true, None);
                self.failures.insert(name.to_string(), 0);
                self.retry_after.remove(name);
                self.slots.insert(name.to_string(), ChildSlot { child });
            }
            Err(error) => {
                tracing::warn!(provider = %name, %error, "could not start child");
                health.record(name, "child", false, Some(redact(&error)));
                self.mark_failure(name);
            }
        }
    }

    fn reap(&mut self) {
        let names: Vec<String> = self.slots.keys().cloned().collect();
        for name in names {
            let exited = self
                .slots
                .get_mut(&name)
                .is_some_and(|slot| matches!(slot.child.try_wait(), Ok(Some(_))));
            if exited {
                self.slots.remove(&name);
                tracing::warn!(provider = %name, "child exited; restart delayed");
                self.mark_failure(&name);
            }
        }
    }

    fn mark_failure(&mut self, name: &str) {
        let failures = self
            .failures
            .get(name)
            .copied()
            .unwrap_or(0)
            .saturating_add(1);
        self.failures.insert(name.to_string(), failures);
        self.retry_after
            .insert(name.to_string(), Instant::now() + backoff(failures));
    }

    /// Kill every child. Used on supervisor shutdown.
    pub async fn stop_all(&mut self) {
        for (name, mut slot) in std::mem::take(&mut self.slots) {
            if let Err(error) = slot.child.kill().await {
                tracing::warn!(provider = %name, %error, "could not stop child");
            }
            drop(slot.child.wait().await);
        }
    }
}

async fn probe_running(
    name: &str,
    provider: &ProviderConfig,
    client: &reqwest::Client,
    health: &HealthTable,
) {
    let Some(backend) = provider.backend.as_deref() else {
        return;
    };
    let url = forward::join_url(backend, provider.health_path(), None);
    match forward::probe(client, &url).await {
        Ok(()) => health.record(name, "child", true, None),
        Err(error) => health.record(name, "child", false, Some(error)),
    }
}

async fn spawn(
    name: &str,
    provider: &ProviderConfig,
    log_dir: &Path,
) -> Result<Child, VivariumError> {
    ensure_private_dir(log_dir)?;
    let command = provider.command.as_ref().ok_or_else(|| {
        VivariumError::Config(format!("providers.{name} child command is missing"))
    })?;
    let program = command
        .first()
        .ok_or_else(|| VivariumError::Config(format!("providers.{name} child command is empty")))?;
    let stdout = std::fs::File::create(child_stdout_path(log_dir, name))?;
    let stderr = std::fs::File::create(child_stderr_path(log_dir, name))?;
    let mut cmd = Command::new(program);
    cmd.args(&command[1..])
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr);
    if let Some(dir) = provider.working_directory.as_deref() {
        cmd.current_dir(dir);
    }
    for (key, value) in &provider.env {
        cmd.env(key, value);
    }
    let child = cmd.spawn()?;
    tokio::time::sleep(START_DELAY).await;
    Ok(child)
}

fn backoff(failures: u32) -> Duration {
    let secs = 2u64.saturating_pow(failures.min(5));
    Duration::from_secs(secs).min(BACKOFF_CAP)
}

fn redact(error: &VivariumError) -> String {
    let text = error.to_string();
    if text.contains("token") || text.contains("Bearer") {
        "child start failed".into()
    } else {
        text
    }
}
