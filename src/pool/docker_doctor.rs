//! The machine-host half of `vivi pool doctor` for the `docker` host.

use std::time::Duration;

use super::docker::{DockerProvider, env_is_set, unset_names};
use super::doctor::Check;

pub const GROUP: &str = "Provider (Docker)";
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// What the daemon says about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Daemon {
    pub version: String,
    /// `linux/arm64`, `linux/amd64`, ...
    pub platform: String,
    /// Memory the daemon can give containers, in bytes.
    pub memory: u64,
}

/// Where the image stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageState {
    /// Present locally, with this architecture.
    Present(String),
    /// Not present, but the registry has it.
    Pullable,
}

/// What the probes saw. `None` means the probe was not attempted because one
/// it depends on failed.
#[derive(Debug, Clone)]
pub struct DockerFacts {
    pub host: Option<String>,
    pub image: String,
    pub memory_limit: String,
    pub unset_env: Vec<String>,
    pub harness_env: usize,
    pub cli: Result<String, String>,
    pub daemon: Option<Result<Daemon, String>>,
    pub image_state: Option<Result<ImageState, String>>,
}

impl DockerProvider {
    /// Probe the `docker` binary and the daemon. Reads only; nothing is
    /// pulled, created, or changed.
    pub(super) fn gather(&self, harness_env: &[String]) -> DockerFacts {
        let cli = self.probe(&["--version"]).map(|t| first_line(&t));
        let daemon = cli.is_ok().then(|| self.daemon());
        let reachable = matches!(daemon, Some(Ok(_)));
        DockerFacts {
            host: self.host.clone(),
            image: self.image.clone(),
            memory_limit: self.limits.memory.clone(),
            unset_env: unset_names(harness_env, env_is_set),
            harness_env: harness_env.len(),
            cli,
            daemon,
            image_state: reachable.then(|| self.image_state()),
        }
    }

    fn probe(&self, args: &[&str]) -> Result<String, String> {
        let out = self
            .call(args, None, PROBE_TIMEOUT)
            .map_err(|e| e.to_string())?;
        if out.succeeded() {
            Ok(out.stdout_text())
        } else {
            Err(first_line(out.stderr.trim()))
        }
    }

    fn daemon(&self) -> Result<Daemon, String> {
        let version = self.probe(&[
            "version",
            "--format",
            "{{.Server.Version}} {{.Server.Os}}/{{.Server.Arch}}",
        ])?;
        let info = self.probe(&["info", "--format", "{{.MemTotal}}"])?;
        let mut parts = version.split_whitespace();
        Ok(Daemon {
            version: parts.next().unwrap_or("?").to_string(),
            platform: parts.next().unwrap_or("?").to_string(),
            memory: info
                .trim()
                .parse()
                .map_err(|_| format!("unexpected memory \"{info}\""))?,
        })
    }

    fn image_state(&self) -> Result<ImageState, String> {
        let local = self.probe(&[
            "image",
            "inspect",
            "--format",
            "{{.Architecture}}",
            &self.image,
        ]);
        if let Ok(arch) = local {
            return Ok(ImageState::Present(arch));
        }
        self.probe(&["manifest", "inspect", &self.image])
            .map(|_| ImageState::Pullable)
    }

    /// The provider's checks, as the pool doctor lists them.
    #[must_use]
    pub fn doctor_checks(&self, harness_env: &[String]) -> Vec<Check> {
        checks(&self.gather(harness_env))
    }
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or_default().trim().to_string()
}

/// Turn what was seen into checks.
#[must_use]
pub fn checks(f: &DockerFacts) -> Vec<Check> {
    vec![
        cli_check(f),
        daemon_check(f),
        image_check(f),
        memory_check(f),
        env_check(f),
    ]
}

fn cli_check(f: &DockerFacts) -> Check {
    match &f.cli {
        Ok(version) => Check::ok(GROUP, "docker", version.clone()),
        Err(e) => Check::fail(
            GROUP,
            "docker",
            e.clone(),
            "install Docker (https://docs.docker.com/get-docker/) and put `docker` on PATH",
        ),
    }
}

fn daemon_check(f: &DockerFacts) -> Check {
    let at = f
        .host
        .as_deref()
        .map_or("local daemon".to_string(), |h| format!("daemon at {h}"));
    match &f.daemon {
        Some(Ok(d)) => Check::ok(
            GROUP,
            "daemon",
            format!("{at}, Docker {}, {}", d.version, d.platform),
        ),
        Some(Err(e)) => Check::fail(
            GROUP,
            "daemon",
            format!("{at} unreachable: {e}"),
            match &f.host {
                Some(h) => format!(
                    "check that `ssh {}` works and Docker runs there",
                    h.trim_start_matches("ssh://")
                ),
                None => "start Docker (Docker Desktop, or the docker service)".to_string(),
            },
        ),
        None => Check::skipped(GROUP, "daemon", "needs the docker binary"),
    }
}

fn image_check(f: &DockerFacts) -> Check {
    let image = &f.image;
    match &f.image_state {
        Some(Ok(ImageState::Present(arch))) if arch.is_empty() => {
            Check::ok(GROUP, "image", format!("{image} is present"))
        }
        Some(Ok(ImageState::Present(arch))) => {
            Check::ok(GROUP, "image", format!("{image} is present ({arch})"))
        }
        Some(Ok(ImageState::Pullable)) => Check::ok(
            GROUP,
            "image",
            format!("{image} is not here yet; the registry has it and `vivi pool create` pulls it"),
        ),
        Some(Err(e)) => Check::fail(
            GROUP,
            "image",
            format!("{image} is neither present nor found in a registry: {e}"),
            format!("docker pull {image} (or fix `image` in [pool])"),
        ),
        None => Check::skipped(GROUP, "image", "needs the daemon"),
    }
}

fn memory_check(f: &DockerFacts) -> Check {
    let Some(Ok(daemon)) = &f.daemon else {
        return Check::skipped(GROUP, "memory", "needs the daemon");
    };
    let limit = parse_size(&f.memory_limit);
    let have = format_bytes(daemon.memory);
    match limit {
        Some(limit) if limit > daemon.memory => Check::warn(
            GROUP,
            "memory",
            format!(
                "the daemon has {have}, but a slot may use up to {} (including its memory disk)",
                f.memory_limit
            ),
            "lower `memory` and `tmpfs` in [pool.docker], or give the daemon more memory",
        ),
        _ => Check::ok(
            GROUP,
            "memory",
            format!(
                "the daemon has {have}; a slot may use up to {}",
                f.memory_limit
            ),
        ),
    }
}

fn env_check(f: &DockerFacts) -> Check {
    let name = "harness env";
    if f.harness_env == 0 {
        return Check::ok(GROUP, name, "no harness env names configured");
    }
    if f.unset_env.is_empty() {
        return Check::ok(
            GROUP,
            name,
            format!(
                "all {} harness env names are set in this process",
                f.harness_env
            ),
        );
    }
    let names = f.unset_env.join(", ");
    let assigns = f
        .unset_env
        .iter()
        .map(|n| format!("{n}=<value>"))
        .collect::<Vec<_>>()
        .join(" ");
    Check::warn(
        GROUP,
        name,
        format!("not set in this process: {names} (slots created now would lack them)"),
        format!("export {assigns} before `vivi pool create`"),
    )
}

/// Bytes in a Docker size such as `4g`, `512m`, `4096k` (binary units).
#[must_use]
pub fn parse_size(text: &str) -> Option<u64> {
    let last = text.chars().last()?;
    let Ok(digits) = text[..text.len() - last.len_utf8()].parse::<u64>() else {
        return None;
    };
    let unit: u64 = match last.to_ascii_lowercase() {
        'k' => 1 << 10,
        'm' => 1 << 20,
        'g' => 1 << 30,
        _ => return None,
    };
    digits.checked_mul(unit)
}

fn format_bytes(bytes: u64) -> String {
    #[allow(clippy::cast_precision_loss)]
    let gib = bytes as f64 / f64::from(1u32 << 30);
    format!("{gib:.1} GiB")
}

#[cfg(test)]
#[path = "docker_doctor_test.rs"]
mod tests;
