//! Read-only size checks for project growth paths.

use std::path::Path;

use super::{Finding, Project};
use crate::pump::config::PumpConfig;

const SECTION: &str = "disk";
const GIB: u64 = 1024 * 1024 * 1024;
const POOL_ROOT_WARN_GIB: u64 = 20;
const PUMP_HOME_WARN_GIB: u64 = 2;
const CLONE_WARN_GIB: u64 = 10;

/// Measure a directory without following symbolic links. Any unreadable path
/// makes the measurement unavailable rather than looking like zero bytes.
#[must_use]
pub fn dir_size_bytes(path: &Path) -> Option<u64> {
    match size_entry(path, u64::MAX) {
        Ok(bytes) => Some(bytes),
        Err(Stop::Unreadable) => None,
        Err(Stop::Over(bytes)) => Some(bytes),
    }
}

/// Why a measurement stopped early.
enum Stop {
    Unreadable,
    /// The walk passed the cap; the bytes counted so far.
    Over(u64),
}

/// Size `path`, stopping as soon as the total passes `cap`. A pump workspace
/// can hold hundreds of GiB of build trees; a threshold check never needs the
/// exact figure once the threshold is crossed.
fn size_entry(path: &Path, cap: u64) -> Result<u64, Stop> {
    let metadata = std::fs::symlink_metadata(path).map_err(|_| Stop::Unreadable)?;
    let kind = metadata.file_type();
    if kind.is_file() {
        Ok(metadata.len())
    } else if kind.is_dir() && !kind.is_symlink() {
        size_directory(path, cap)
    } else {
        Ok(0)
    }
}

fn size_directory(path: &Path, cap: u64) -> Result<u64, Stop> {
    let entries = std::fs::read_dir(path).map_err(|_| Stop::Unreadable)?;
    let mut sum = 0_u64;
    for entry in entries {
        let entry = entry.map_err(|_| Stop::Unreadable)?;
        let remaining = cap.saturating_sub(sum);
        let bytes = match size_entry(&entry.path(), remaining) {
            Ok(bytes) => bytes,
            Err(Stop::Over(bytes)) => return Err(Stop::Over(sum.saturating_add(bytes))),
            Err(stop) => return Err(stop),
        };
        sum = sum.saturating_add(bytes);
        if sum > cap {
            return Err(Stop::Over(sum));
        }
    }
    Ok(sum)
}

/// Report one directory's measured size, or skip when it cannot be measured.
#[must_use]
pub fn size_finding(section: &str, name: &str, path: &Path, warn_bytes: u64) -> Finding {
    let bytes = match size_entry(path, warn_bytes) {
        Ok(bytes) => bytes,
        Err(Stop::Over(_)) => {
            return Finding::warn(
                section,
                name,
                &format!(
                    "{} is over {} GiB (warns above {} GiB)",
                    path.display(),
                    gib(warn_bytes),
                    gib(warn_bytes)
                ),
                "reclaim space (vivi pool clean)",
            );
        }
        Err(Stop::Unreadable) => {
            return Finding::skipped(
                section,
                name,
                &format!("{} absent or unreadable", path.display()),
            );
        }
    };
    Finding::ok(
        section,
        name,
        &format!("{} is {} GiB", path.display(), gib(bytes)),
    )
}

/// Measure configured local pool roots, pump state, merge lanes, integration
/// clones.
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

/// Add the per-target clone and merge-lane measurements.
fn target_findings(project: &Project, config: &PumpConfig, home: &Path) -> Vec<Finding> {
    let mut out = Vec::new();
    let targets = config.target_configs();
    if targets.is_empty() {
        out.push(Finding::skipped(
            SECTION,
            "integration clone",
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
            continue;
        };
        out.push(size_finding(
            SECTION,
            &format!("integration clone {target}"),
            &repo,
            CLONE_WARN_GIB * GIB,
        ));
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
    for name in ["pump home", "integration clone"] {
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

fn gib(bytes: u64) -> String {
    #[allow(clippy::cast_precision_loss)]
    let value = bytes as f64 / GIB as f64;
    format!("{value:.1}")
}

#[cfg(test)]
#[path = "disk_section_test.rs"]
mod tests;
