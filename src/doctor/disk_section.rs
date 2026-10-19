//! Read-only size and unit-branch checks for project growth paths.

use std::path::Path;
use std::process::Command;

use super::{Finding, Project};
use crate::pump::config::PumpConfig;

const SECTION: &str = "disk";
const GIB: u64 = 1024 * 1024 * 1024;
const POOL_ROOT_WARN_GIB: u64 = 20;
const PUMP_HOME_WARN_GIB: u64 = 2;
const CLONE_WARN_GIB: u64 = 10;
const BRANCHES_WARN: usize = 400;

/// Measure a directory without following symbolic links. Any unreadable path
/// makes the measurement unavailable rather than looking like zero bytes.
#[must_use]
pub fn dir_size_bytes(path: &Path) -> Option<u64> {
    size_entry(path)
}

fn size_entry(path: &Path) -> Option<u64> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => return None,
    };
    let kind = metadata.file_type();
    if kind.is_symlink() {
        Some(0)
    } else if kind.is_file() {
        Some(metadata.len())
    } else if kind.is_dir() {
        size_directory(path)
    } else {
        Some(0)
    }
}

fn size_directory(path: &Path) -> Option<u64> {
    let mut entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(_) => return None,
    };
    entries.try_fold(0_u64, |sum, entry| {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => return None,
        };
        sum.checked_add(size_entry(&entry.path())?)
    })
}

/// Report one directory's measured size, or skip when it cannot be measured.
#[must_use]
pub fn size_finding(section: &str, name: &str, path: &Path, warn_bytes: u64) -> Finding {
    let Some(bytes) = dir_size_bytes(path) else {
        return Finding::skipped(
            section,
            name,
            &format!("{} absent or unreadable", path.display()),
        );
    };
    let shown = gib(bytes);
    if bytes > warn_bytes {
        Finding::warn(
            section,
            name,
            &format!(
                "{} is {shown} GiB (warns above {} GiB)",
                path.display(),
                gib(warn_bytes)
            ),
            "reclaim space (vivi pool clean; prune old merge lanes and unit branches)",
        )
    } else {
        Finding::ok(section, name, &format!("{} is {shown} GiB", path.display()))
    }
}

/// Count local `vivi/unit/*` branches without fetching or changing repository state.
#[must_use]
pub fn branch_count(repo: &Path) -> Option<usize> {
    let prefix = format!("refs/heads/{}", crate::pump::need::unit_namespace_prefix());
    let output = match Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["for-each-ref", "--format=%(refname)"])
        .arg(prefix)
        .output()
    {
        Ok(output) => output,
        Err(_) => return None,
    };
    if !output.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| !line.is_empty())
            .count(),
    )
}

/// Measure configured local pool roots, pump state, merge lanes, integration
/// clones, and local `unit/*` branches.
#[must_use]
pub fn findings(project: &Project) -> Vec<Finding> {
    let mut out = pool_findings(project);
    let Some(config) = project.config.pump.as_ref() else {
        skip_unconfigured_pump(&mut out);
        return out;
    };
    let home = config.home_dir(&project.root);
    out.push(size_finding(
        SECTION,
        "pump home",
        &home,
        PUMP_HOME_WARN_GIB * GIB,
    ));
    out.extend(target_findings(project, config, &home));
    out
}

/// Add the per-target clone, unit-branch, and merge-lane measurements.
fn target_findings(project: &Project, config: &PumpConfig, home: &Path) -> Vec<Finding> {
    let mut out = Vec::new();
    let targets = config.target_configs();
    if targets.is_empty() {
        out.push(Finding::skipped(
            SECTION,
            "integration clone",
            "no pump targets configured",
        ));
        out.push(Finding::skipped(
            SECTION,
            "unit branches",
            "no pump targets configured",
        ));
    }
    for (target, _) in targets {
        let Some(repo) = config.target_repo_dir(&project.root, &target) else {
            out.push(Finding::skipped(
                SECTION,
                "integration clone",
                &format!("target {target} path unavailable"),
            ));
            out.push(Finding::skipped(
                SECTION,
                "unit branches",
                &format!("target {target} path unavailable"),
            ));
            continue;
        };
        out.push(size_finding(
            SECTION,
            &format!("integration clone {target}"),
            &repo,
            CLONE_WARN_GIB * GIB,
        ));
        out.push(branch_finding(&target, &repo));
        out.push(size_finding(
            SECTION,
            &format!("merge lanes {target}"),
            &home.join("merge-lanes").join(&target),
            PUMP_HOME_WARN_GIB * GIB,
        ));
    }
    out
}

/// Report unavailable pump measurements when the project has no pump config.
fn skip_unconfigured_pump(out: &mut Vec<Finding>) {
    for name in ["pump home", "integration clone", "unit branches"] {
        out.push(Finding::skipped(SECTION, name, "pump not configured"));
    }
}

fn pool_findings(project: &Project) -> Vec<Finding> {
    if project.config.pools.is_empty() {
        return vec![Finding::skipped(
            SECTION,
            "pool roots",
            "no pools configured",
        )];
    }
    project
        .config
        .pools
        .iter()
        .map(|(name, config)| {
            if config.provider.eq_ignore_ascii_case("local") {
                let root = crate::pool::local_slots::expand_root(config.local.root_or_default());
                size_finding(
                    SECTION,
                    &format!("pool root {name}"),
                    &root,
                    POOL_ROOT_WARN_GIB * GIB,
                )
            } else {
                Finding::skipped(
                    SECTION,
                    &format!("pool root {name}"),
                    "pool storage is not a local directory",
                )
            }
        })
        .collect()
}

fn branch_finding(target: &str, repo: &Path) -> Finding {
    match branch_count(repo) {
        Some(count) if count > BRANCHES_WARN => Finding::warn(
            SECTION,
            &format!("unit branches {target}"),
            &format!("{count} unit/* branches"),
            "vivi pump closeout prunes merged units",
        ),
        Some(count) => Finding::ok(
            SECTION,
            &format!("unit branches {target}"),
            &format!("{count} unit/* branches"),
        ),
        None => Finding::skipped(
            SECTION,
            &format!("unit branches {target}"),
            &format!("integration clone {} is unavailable", repo.display()),
        ),
    }
}

fn gib(bytes: u64) -> String {
    #[allow(clippy::cast_precision_loss)]
    let value = bytes as f64 / GIB as f64;
    format!("{value:.1}")
}

#[cfg(test)]
#[path = "disk_section_test.rs"]
mod tests;
