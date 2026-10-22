//! Offline pool configuration and ownership checks. Never opens a provider.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use super::Finding;
use crate::mailspace::MailspaceConfig;
use crate::pool::config::PoolConfig;
use crate::pool::slot::PoolId;

const SECTION: &str = "pools";
const CONFIG_FIX: &str = "check [pools.<name>] in .vivi/mailspace.toml";
const SCOPE_FIX: &str = "inspect pool roots and ownership with vivi pool doctor --pool <name>";
const SCAN_LIMIT: usize = 4096;

/// Inspect configured pools without network calls, subprocesses, or writes.
#[must_use]
pub fn findings(project: Option<&Path>) -> Vec<Finding> {
    let Ok(cwd) = std::env::current_dir() else {
        return vec![Finding::warn(
            SECTION,
            "config",
            "current directory unavailable",
            CONFIG_FIX,
        )];
    };
    let root = project.map(Path::to_path_buf).or_else(|| {
        cwd.ancestors()
            .find(|root| root.join(".vivi/mailspace.toml").is_file())
            .map(Path::to_path_buf)
    });
    let Some(root) = root else {
        return vec![Finding::skipped(
            SECTION,
            "config",
            "no mailspace discovered",
        )];
    };
    let Ok(mut config) = read_config(&root) else {
        return vec![Finding::fail(
            SECTION,
            "config",
            "pool configuration unreadable or invalid",
            CONFIG_FIX,
        )];
    };
    if let Err(reason) = read_identity(&root, &mut config) {
        return vec![Finding::warn(SECTION, "ownership", reason, CONFIG_FIX)];
    }
    inspect(&config, &cwd)
}

fn read_config(root: &Path) -> Result<MailspaceConfig, ()> {
    let path = root.join(".vivi/mailspace.toml");
    let raw = fs::read_to_string(&path).map_err(|_| ())?;
    crate::mailspace::inherit::load_config(&path, &raw).map_err(|_| ())
}

/// Match normal mailspace identity resolution, without restoring or minting it.
fn read_identity(root: &Path, config: &mut MailspaceConfig) -> Result<(), &'static str> {
    let sidecar = match read_mark(&root.join(".vivi/mailspace.id")) {
        Ok(id) => Some(id),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return Err("mailspace identity unreadable; slot ownership unknown"),
    };
    if config.id.as_ref().is_some_and(|id| !canonical_id(id))
        || sidecar.as_ref().is_some_and(|id| !canonical_id(id))
    {
        return Err("mailspace identity invalid; slot ownership unknown");
    }
    match (&config.id, sidecar) {
        (Some(id), Some(other)) if *id != other => {
            Err("mailspace config and sidecar identities disagree; slot ownership unknown")
        }
        (None, Some(id)) => {
            config.id = Some(id);
            Ok(())
        }
        _ => Ok(()),
    }
}

fn canonical_id(id: &str) -> bool {
    id.len() == 36
        && id.char_indices().all(|(index, ch)| match index {
            8 | 13 | 18 | 23 => ch == '-',
            _ => ch.is_ascii_digit() || ('a'..='f').contains(&ch),
        })
}

fn inspect(config: &MailspaceConfig, cwd: &Path) -> Vec<Finding> {
    if config.retired_pool.is_some() {
        return vec![Finding::fail(
            SECTION,
            "config",
            "retired [pool] table is present; use named pools",
            CONFIG_FIX,
        )];
    }
    if config.pools.is_empty() {
        return vec![Finding::skipped(SECTION, "config", "no pools configured")];
    }
    let detail = format!(
        "{} configured pool(s); offline checks only",
        config.pools.len()
    );
    let mut result = vec![Finding::ok(SECTION, "config", &detail)];
    if config.id.is_none() {
        result.push(Finding::warn(SECTION, "identity", "mailspace has no stable identity; name-only slot ownership can overlap other mailspaces", CONFIG_FIX));
    }
    let mut scopes = BTreeMap::<String, Vec<&str>>::new();
    for (name, pool) in &config.pools {
        let id = PoolId::new(&config.name, config.id.as_deref().unwrap_or_default(), name);
        let label = format!("pool {name}");
        if pool.validate(&id).is_err() {
            result.push(Finding::fail(
                SECTION,
                &label,
                "pool settings are invalid",
                CONFIG_FIX,
            ));
            continue;
        }
        result.push(Finding::ok(
            SECTION,
            &label,
            &format!(
                "provider {}; configured capacity {}",
                pool.provider, pool.slots
            ),
        ));
        scopes.entry(scope(pool, cwd)).or_default().push(name);
        result.push(slot_finding(pool, &id, config.id.is_some(), cwd));
    }
    result.extend(scope_findings(&scopes));
    result.extend(prefix_findings(
        config.pools.keys().map(String::as_str).collect(),
    ));
    result
}

fn slot_finding(pool: &PoolConfig, id: &PoolId, stable_id: bool, cwd: &Path) -> Finding {
    let name = format!("ownership {}", id.pool);
    if pool.provider != "local" {
        Finding::skipped(
            SECTION,
            &name,
            "remote slot ownership and availability not checked (offline)",
        )
    } else if !stable_id {
        Finding::warn(
            SECTION,
            &name,
            "stable mailspace identity missing; ownership unknown",
            CONFIG_FIX,
        )
    } else {
        ownership(id, &local_root(pool, cwd))
    }
}

fn local_root(pool: &PoolConfig, cwd: &Path) -> PathBuf {
    let path = crate::pool::local_slots::expand_root(pool.local.root_or_default());
    let path = if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    };
    path.canonicalize().unwrap_or(path)
}

/// Scope keys are compared, never printed: hosts may carry sensitive details.
fn scope(pool: &PoolConfig, cwd: &Path) -> String {
    match pool.provider.as_str() {
        "local" => format!("local:{}", local_root(pool, cwd).display()),
        "ssh" => format!("ssh:{:?}:{}", pool.ssh.host, pool.ssh.root_or_default()),
        "fly" => format!("fly:{:?}", pool.fly.app),
        "docker" => format!("docker:{:?}", pool.docker.host),
        _ => format!("runpod:{:?}", pool.runpod.api_key_file),
    }
}

fn scope_findings(scopes: &BTreeMap<String, Vec<&str>>) -> Vec<Finding> {
    scopes
        .iter()
        .filter(|(_, names)| names.len() > 1)
        .map(|(scope, names)| {
            let what = if scope.starts_with("local:") || scope.starts_with("ssh:") {
                "root"
            } else {
                "provider scope"
            };
            Finding::warn(
                SECTION,
                "shared scope",
                &format!(
                    "pools {} share a {what}; ownership depends on mailspace and pool marks",
                    names.join(", ")
                ),
                SCOPE_FIX,
            )
        })
        .collect()
}

fn prefix_findings(names: Vec<&str>) -> Vec<Finding> {
    let mut findings = Vec::new();
    for (index, name) in names.iter().enumerate() {
        for other in names.iter().skip(index + 1) {
            if name.starts_with(*other) || other.starts_with(*name) {
                findings.push(Finding::warn(SECTION, "overlapping prefixes", &format!("pool name prefixes overlap: {name}, {other}; use exact pool names and ownership marks"), SCOPE_FIX));
            }
        }
    }
    findings
}

#[derive(Debug, Default)]
struct Ownership {
    own: usize,
    foreign: usize,
    unknown: usize,
}

fn ownership(id: &PoolId, root: &Path) -> Finding {
    let name = format!("ownership {}", id.pool);
    let facts = match scan_root(root, id) {
        Ok(facts) => facts,
        Err(_) => {
            return Finding::warn(
                SECTION,
                &name,
                "local slot root missing, unreadable, or scan limit reached; ownership unknown",
                SCOPE_FIX,
            );
        }
    };
    ownership_finding(&name, &facts)
}

fn ownership_finding(name: &str, facts: &Ownership) -> Finding {
    let detail = format!(
        "local slots: {} owned, {} owned by another mailspace, {} unknown",
        facts.own, facts.foreign, facts.unknown
    );
    if facts.foreign > 0 || facts.unknown > 0 {
        Finding::warn(SECTION, name, &detail, SCOPE_FIX)
    } else {
        Finding::ok(SECTION, name, &detail)
    }
}

/// Only three directory levels and the two ownership marks are read.
fn scan_root(root: &Path, id: &PoolId) -> std::io::Result<Ownership> {
    let mut facts = Ownership::default();
    let mut remaining = SCAN_LIMIT;
    for space in directories(root, &mut remaining)? {
        for pool in directories(&space, &mut remaining)? {
            for slot in directories(&pool, &mut remaining)? {
                let name = slot
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default();
                if name
                    .strip_prefix("slot-")
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
                {
                    record_ownership(&slot, id, &mut facts);
                }
            }
        }
    }
    Ok(facts)
}

fn directories(path: &Path, remaining: &mut usize) -> std::io::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(path)? {
        *remaining = remaining
            .checked_sub(1)
            .ok_or_else(|| std::io::Error::other("scan limit"))?;
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Err(std::io::Error::other("linked directory; ownership unknown"));
        }
        if kind.is_dir() && !entry.file_name().to_string_lossy().starts_with('.') {
            paths.push(entry.path());
        }
    }
    Ok(paths)
}

fn record_ownership(slot: &Path, id: &PoolId, facts: &mut Ownership) {
    if !matches!(fs::symlink_metadata(slot.join("marks")), Ok(m) if m.is_dir()) {
        facts.unknown += 1;
        return;
    }
    let Ok(pool) = read_mark(&slot.join("marks/vivi_pool")) else {
        facts.unknown += 1;
        return;
    };
    if pool != id.pool {
        return;
    }
    match read_mark(&slot.join("marks/vivi_pool_mailspace")) {
        Ok(mark) if mark == id.mailspace => facts.own += 1,
        Ok(mark) if !mark.is_empty() => facts.foreign += 1,
        _ => facts.unknown += 1,
    }
}

fn read_mark(path: &Path) -> std::io::Result<String> {
    if !fs::symlink_metadata(path)?.is_file() {
        return Err(std::io::Error::other("mark is not a plain file"));
    }
    let mut value = String::new();
    fs::File::open(path)?.take(513).read_to_string(&mut value)?;
    if value.len() > 512 {
        return Err(std::io::Error::other("oversized mark"));
    }
    Ok(value.trim().to_string())
}

#[cfg(test)]
#[path = "pools_section_test.rs"]
mod tests;
