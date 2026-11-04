//! The machine-host half of `vivi pool doctor` for the `ssh` host.

use std::collections::BTreeMap;

use super::doctor::Check;
use super::ssh::SshProvider;

pub const GROUP: &str = "Provider (SSH)";
/// Free space below which a warning is given, in GiB (same floor as the
/// local dispatch reclaim guard).
const LOW_DISK_GIB: u64 = super::clean_rule::DISK_RECLAIM_GIB;
const KIB_PER_GIB: u64 = 1024 * 1024;

/// What the host said about itself in one call.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Probe {
    pub user: String,
    pub host_name: String,
    /// `Linux x86_64`
    pub system: String,
    /// Tools and facilities the slot scripts need that the host lacks.
    pub missing: Vec<String>,
    pub root: String,
    pub root_exists: bool,
    pub root_writable: bool,
    /// The nearest directory of the root's path that exists.
    pub anchor: String,
    pub free_kb: Option<u64>,
}

impl Probe {
    /// Read the `key=value` lines of the remote `probe` verb.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut probe = Self::default();
        let fields: BTreeMap<&str, Vec<&str>> = text
            .lines()
            .filter_map(|l| l.split_once('='))
            .fold(BTreeMap::new(), |mut acc, (k, v)| {
                acc.entry(k).or_default().push(v.trim());
                acc
            });
        let one = |key: &str| {
            fields
                .get(key)
                .and_then(|v| v.first())
                .map_or(String::new(), ToString::to_string)
        };
        probe.user = one("user");
        probe.host_name = one("host");
        probe.system = one("system");
        probe.missing = fields
            .get("missing")
            .map(|v| v.iter().map(ToString::to_string).collect())
            .unwrap_or_default();
        probe.root = one("root");
        probe.root_exists = one("root_exists") == "1";
        probe.root_writable = one("root_writable") == "1";
        probe.anchor = one("root_anchor");
        probe.free_kb = one("free_kb").parse::<u64>().into_iter().next();
        probe
    }
}

/// What the probes saw. `None` means a probe was not attempted because the
/// one before it failed.
#[derive(Debug, Clone)]
pub struct SshFacts {
    pub host: String,
    pub root: String,
    pub harness_env: usize,
    pub unset_env: Vec<String>,
    /// The local `ssh` client's version line.
    pub client: Result<String, String>,
    pub probe: Option<Result<Probe, String>>,
}

impl SshProvider {
    /// The provider's checks, as the pool doctor lists them. Reads only: it
    /// creates nothing on the host, not even the root.
    #[must_use]
    pub fn doctor_checks(&self, harness_env: &[String]) -> Vec<Check> {
        let client = super::ssh::client_version();
        let probe = client.is_ok().then(|| self.probe());
        let reachable = matches!(probe, Some(Ok(_)));
        let unset_env = if reachable {
            self.unset_harness_env(harness_env).unwrap_or_default()
        } else {
            Vec::new()
        };
        checks(&SshFacts {
            host: self.host().to_string(),
            root: self.root().to_string(),
            harness_env: harness_env.len(),
            unset_env,
            client,
            probe,
        })
    }
}

/// Turn what was seen into checks.
#[must_use]
pub fn checks(f: &SshFacts) -> Vec<Check> {
    let mut list = vec![client_check(f), host_check(f)];
    let probe = match &f.probe {
        Some(Ok(p)) => Some(p),
        _ => None,
    };
    list.push(system_check(probe));
    list.push(tools_check(probe));
    list.push(root_check(f, probe));
    list.push(disk_check(probe));
    list.push(arch_check(probe));
    list.push(env_check(f, probe.is_some()));
    list
}

fn client_check(f: &SshFacts) -> Check {
    match &f.client {
        Ok(version) => Check::ok(GROUP, "ssh", version.clone()),
        Err(e) => Check::fail(
            GROUP,
            "ssh",
            e.clone(),
            "install OpenSSH and put `ssh` on PATH",
        ),
    }
}

fn host_check(f: &SshFacts) -> Check {
    match &f.probe {
        Some(Ok(p)) => Check::ok(
            GROUP,
            "host",
            format!(
                "{}@{} reachable without a prompt (login shell runs bash)",
                p.user, p.host_name
            ),
        ),
        Some(Err(e)) => Check::fail(
            GROUP,
            "host",
            format!("cannot run commands on {}: {e}", f.host),
            format!(
                "check that `ssh -o BatchMode=yes {} true` succeeds: a key that needs no passphrase prompt (ssh-agent), a known host key, and a POSIX login shell",
                f.host
            ),
        ),
        None => Check::skipped(GROUP, "host", "needs the ssh client"),
    }
}

fn system_check(probe: Option<&Probe>) -> Check {
    let Some(p) = probe else {
        return Check::skipped(GROUP, "system", "needs the host");
    };
    if p.system.starts_with("Linux") {
        Check::ok(GROUP, "system", p.system.clone())
    } else {
        Check::fail(
            GROUP,
            "system",
            format!("{} is not Linux", p.system),
            "the ssh provider needs a Linux host (it finds a slot's processes through /proc)",
        )
    }
}

fn tools_check(probe: Option<&Probe>) -> Check {
    let Some(p) = probe else {
        return Check::skipped(GROUP, "tools", "needs the host");
    };
    if p.missing.is_empty() {
        return Check::ok(GROUP, "tools", "bash, git, tar and the rest are present");
    }
    Check::fail(
        GROUP,
        "tools",
        format!("missing on the host: {}", p.missing.join(", ")),
        "install them on the host (the ssh provider installs nothing); git and tar are needed, and bash, setsid, timeout, sha256sum, ps from the usual base packages",
    )
}

fn root_check(f: &SshFacts, probe: Option<&Probe>) -> Check {
    let Some(p) = probe else {
        return Check::skipped(GROUP, "root", "needs the host");
    };
    match (p.root_exists, p.root_writable) {
        (true, true) => Check::ok(GROUP, "root", format!("{} exists and is writable", p.root)),
        (false, true) => Check::ok(
            GROUP,
            "root",
            format!("{} will be created by `vivi pool create`", p.root),
        ),
        _ => Check::fail(
            GROUP,
            "root",
            format!("{} ({}) is not writable by {}", f.root, p.anchor, p.user),
            "choose a root the login user can write (`root` in [pool.ssh]), or fix its permissions",
        ),
    }
}

fn disk_check(probe: Option<&Probe>) -> Check {
    let Some(p) = probe else {
        return Check::skipped(GROUP, "disk", "needs the host");
    };
    let Some(kb) = p.free_kb else {
        return Check::warn(
            GROUP,
            "disk",
            "free space could not be read",
            "check `df` on the host",
        );
    };
    let gib = kb / KIB_PER_GIB;
    let tenths = (kb % KIB_PER_GIB) * 10 / KIB_PER_GIB;
    let detail = format!(
        "{gib}.{tenths} GiB free where the root lives ({})",
        p.anchor
    );
    if gib < LOW_DISK_GIB {
        Check::warn(
            GROUP,
            "disk",
            detail,
            "each slot holds a checkout and its build output; free space or choose a root on a larger disk",
        )
    } else {
        Check::ok(GROUP, "disk", detail)
    }
}

fn arch_check(probe: Option<&Probe>) -> Check {
    match probe {
        Some(p) => Check::ok(GROUP, "arch", p.system.clone()),
        None => Check::skipped(GROUP, "arch", "needs the host"),
    }
}

fn env_check(f: &SshFacts, reachable: bool) -> Check {
    let name = "harness env";
    if !reachable {
        return Check::skipped(GROUP, name, "needs the host");
    }
    if f.harness_env == 0 {
        return Check::ok(GROUP, name, "no harness env names configured");
    }
    if f.unset_env.is_empty() {
        return Check::ok(
            GROUP,
            name,
            format!(
                "all {} harness env names are set in the login environment of the host",
                f.harness_env
            ),
        );
    }
    Check::warn(
        GROUP,
        name,
        format!(
            "not set in the login environment on {}: {}",
            f.host,
            f.unset_env.join(", ")
        ),
        "export them in ~/.profile of the login user (a login shell reads it; ~/.bashrc is skipped when non-interactive); values stay on the host",
    )
}

#[cfg(test)]
#[path = "ssh_doctor_test.rs"]
mod tests;
