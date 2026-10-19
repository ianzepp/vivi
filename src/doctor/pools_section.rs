//! The `pools` section of `vivi doctor`: an offline health summary of the
//! configured pools, plus the pool-scoping risks of need 87af6100.
//!
//! Provider-side checks (Fly/RunPod APIs, GitHub reachability) are never run
//! here: each per-pool finding's fix points at
//! `vivi pool doctor --pool <name>` for the full report.

use std::collections::{BTreeMap, BTreeSet};
use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use super::Finding;
use crate::mailspace::Mailspace;
use crate::pool::config::PoolConfig;
use crate::pool::local_slots as slots;
use crate::pool::slot::{PoolId, space_slug};

/// The section name every finding here carries.
pub const SECTION: &str = "pools";

/// The providers whose slots are directories on this machine, so their root
/// and slot facts can be gathered offline.
const LOCAL: &str = "local";

/// Summarize every configured pool offline and flag scoping risks.
#[must_use]
pub fn findings(mailspace: &Mailspace) -> Vec<Finding> {
    match crate::pool::pools::configured(mailspace) {
        Ok(pools) => {
            let mut out = pool_findings(mailspace, pools);
            out.extend(scoping_findings(mailspace, pools));
            out
        }
        Err(error) => vec![config_finding(mailspace, error.to_string())],
    }
}

/// The section gate: no pools at all only skips; anything else that stops the
/// pools from being read fails.
fn config_finding(mailspace: &Mailspace, error: String) -> Finding {
    if mailspace.config.retired_pool.is_none() && mailspace.config.pools.is_empty() {
        Finding::skipped(SECTION, "config", "no pools configured")
    } else {
        Finding::fail(
            SECTION,
            "config",
            &error,
            "fix [pools.*] in .vivi/mailspace.toml",
        )
    }
}

/// One finding per configured pool.
fn pool_findings(mailspace: &Mailspace, pools: &BTreeMap<String, PoolConfig>) -> Vec<Finding> {
    let key = mailspace_key(mailspace);
    pools
        .iter()
        .map(|(name, config)| pool_finding(mailspace, &key, name, config))
        .collect()
}

/// The mailspace key that names the slot directories on disk.
fn mailspace_key(mailspace: &Mailspace) -> String {
    mailspace
        .config
        .id
        .clone()
        .unwrap_or_else(|| space_slug(&mailspace.config.name))
}

/// One pool's offline facts, aggregated into a single finding.
fn pool_finding(
    mailspace: &Mailspace,
    key: &str,
    name: &str,
    config: &PoolConfig,
) -> Finding {
    let fix = format!("vivi pool doctor --pool {name}");
    if let Err(error) = config.validate(&PoolId::keyed(key, name)) {
        return Finding::fail(SECTION, name, &format!("{name}: {error}"), fix);
    }
    if config.provider != LOCAL {
        return Finding::warn(
            SECTION,
            name,
            &format!(
                "{name}: {} pool: slot facts need the provider (not probed offline)",
                config.provider
            ),
            fix,
        );
    }
    local_pool_finding(mailspace, key, name, config)
}

/// A `local` pool's slots are directories here, so root, writability and slot
/// counts are all checkable without a provider.
fn local_pool_finding(
    mailspace: &Mailspace,
    key: &str,
    name: &str,
    config: &PoolConfig,
) -> Finding {
    let fix = format!("vivi pool doctor --pool {name}");
    let root = local_root(mailspace, config);
    if !root.is_dir() {
        return Finding::warn(
            SECTION,
            name,
            &format!("{name}: root missing ({})", root.display()),
            fix,
        );
    }
    let mut problems = Vec::new();
    if !writable(&root) {
        problems.push("root is not writable".to_string());
    }
    let have = slot_dir_count(&root.join(key).join(name));
    let max = config.slots;
    if let Some(summary) = slot_count_summary(have, max) {
        problems.push(summary);
    }
    if problems.is_empty() {
        Finding::ok(
            SECTION,
            name,
            &format!("{name}: root ok, slot dirs {have} of {max}"),
        )
    } else {
        Finding::warn(SECTION, name, &format!("{name}: {}", problems.join("; ")), fix)
    }
}

/// How the slot-dir count compares with the configured maximum.
fn slot_count_summary(have: u32, max: u32) -> Option<String> {
    match have.cmp(&max) {
        Ordering::Equal => None,
        Ordering::Less => Some(format!("slot dirs {have} of {max}")),
        Ordering::Greater => Some(format!(
            "slot dirs {have} of {max} (more than the configured maximum)"
        )),
    }
}

/// The pool root as a path: `~/` expands to the home directory, a relative
/// path resolves against the project root.
fn local_root(mailspace: &Mailspace, config: &PoolConfig) -> PathBuf {
    let root = slots::expand_root(config.local.root_or_default());
    if root.is_absolute() {
        root
    } else {
        mailspace.root.join(root)
    }
}

/// The pool root compared for sharing: resolved, and canonical when it
/// exists so the same directory under two spellings still matches.
fn root_key(mailspace: &Mailspace, config: &PoolConfig) -> PathBuf {
    let root = local_root(mailspace, config);
    root.canonicalize().unwrap_or(root)
}

/// Whether a file can be created in `dir`, probed with a file that is
/// removed again. Never errors: a failed probe just reads as unwritable.
fn writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".vivi-doctor-probe-{}", std::process::id()));
    let written = std::fs::write(&probe, b"").is_ok();
    if written {
        std::fs::remove_file(&probe).map(drop).unwrap_or_default();
    }
    written
}

/// The slot directories directly inside `pool_dir`, sorted by name.
fn slot_dirs(pool_dir: &Path) -> Vec<String> {
    list_dirs(pool_dir)
        .into_iter()
        .filter(|name| is_slot_dir(name))
        .collect()
}

/// How many slot directories a pool dir holds.
fn slot_dir_count(pool_dir: &Path) -> u32 {
    u32::try_from(slot_dirs(pool_dir).len()).unwrap_or(u32::MAX)
}

/// True for a directory named `slot-<digits>`, the layout of
/// `crate::pool::local_slots`.
fn is_slot_dir(name: &str) -> bool {
    name.strip_prefix("slot-")
        .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
}

/// The plain subdirectory names directly inside `dir`, sorted; links and dot
/// names are skipped, and a directory that cannot be read reads as empty.
fn list_dirs(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// The scoping risks of need 87af6100, each its own finding; when none
/// applies, one ok finding says so.
fn scoping_findings(mailspace: &Mailspace, pools: &BTreeMap<String, PoolConfig>) -> Vec<Finding> {
    let mut out = Vec::new();
    out.extend(shared_root_findings(mailspace, pools));
    out.extend(prefix_findings(pools.keys()));
    out.extend(foreign_dir_findings(mailspace, pools));
    if out.is_empty() {
        out.push(Finding::ok(
            SECTION,
            "pool scoping clean",
            "no shared roots, no name prefixes, no foreign slot dirs",
        ));
    }
    out
}

/// Local pools whose slots resolve to the same directory.
fn shared_root_findings(
    mailspace: &Mailspace,
    pools: &BTreeMap<String, PoolConfig>,
) -> Vec<Finding> {
    let mut by_root: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    for (name, config) in pools.iter().filter(|(_, c)| c.provider == LOCAL) {
        by_root
            .entry(root_key(mailspace, config))
            .or_default()
            .push(name.clone());
    }
    by_root
        .into_iter()
        .filter(|(_, names)| names.len() > 1)
        .map(|(root, names)| {
            Finding::warn(
                SECTION,
                "shared root",
                &format!("pools {} share root {}", names.join(", "), root.display()),
                "give each pool its own root",
            )
        })
        .collect()
}

/// One finding per pair of pool names where one is a strict prefix of the
/// other (case-sensitive), so `--pool` picks become ambiguous to read.
fn prefix_findings<'a>(names: impl Iterator<Item = &'a String>) -> Vec<Finding> {
    let names: Vec<&String> = names.collect();
    let mut out = Vec::new();
    for (position, shorter) in names.iter().enumerate() {
        for longer in names.iter().skip(position + 1) {
            if longer.starts_with(shorter.as_str()) {
                out.push(Finding::warn(
                    SECTION,
                    "overlapping name prefixes",
                    &format!("pool {shorter} is a strict prefix of pool {longer}"),
                    "rename so no pool name prefixes another (see 87af6100)",
                ));
            }
        }
    }
    out
}

/// Slot directories under a local pool's root whose owning pool name matches
/// no configured pool. Each root is scanned once, even when pools share it.
fn foreign_dir_findings(
    mailspace: &Mailspace,
    pools: &BTreeMap<String, PoolConfig>,
) -> Vec<Finding> {
    let key = mailspace_key(mailspace);
    let mut scanned: BTreeSet<PathBuf> = BTreeSet::new();
    let mut out = Vec::new();
    for (_, config) in pools.iter().filter(|(_, c)| c.provider == LOCAL) {
        let root = root_key(mailspace, config);
        if !scanned.insert(root.clone()) {
            continue;
        }
        let space = root.join(&key);
        let foreign: Vec<String> = list_dirs(&space)
            .into_iter()
            .filter(|dir| !pools.contains_key(dir))
            .collect();
        if let Some(finding) = foreign_finding(&space, &foreign) {
            out.push(finding);
        }
    }
    out
}

/// The foreign-dir finding, listing up to five names; `None` when there is
/// nothing foreign.
fn foreign_finding(space: &Path, foreign: &[String]) -> Option<Finding> {
    if foreign.is_empty() {
        return None;
    }
    let shown: Vec<String> = foreign.iter().take(5).cloned().collect();
    let mut detail = format!(
        "slot dirs under {} owned by no configured pool: {}",
        space.display(),
        shown.join(", ")
    );
    if foreign.len() > shown.len() {
        detail.push_str(&format!(", and {} more", foreign.len() - shown.len()));
    }
    Some(Finding::warn(
        SECTION,
        "foreign slot dirs",
        &detail,
        "vivi pool clean (scoped to this pool's slots)",
    ))
}

#[cfg(test)]
#[path = "pools_section_test.rs"]
mod tests;
