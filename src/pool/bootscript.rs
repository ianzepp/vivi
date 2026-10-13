//! The in-machine boot script and the files and environment that drive it.
//!
//! The script itself is generic and embedded; everything specific to a
//! project reaches it as environment values and as the rendered `setup.sh`
//! and `seed.sh` files.

use std::collections::BTreeMap;

use super::config::PoolConfig;
use super::github::Secret;
use super::requires;

/// The generic boot script, embedded in the binary.
pub const BOOT_SCRIPT: &str = include_str!("boot-slot.sh");

/// Where the boot script lands inside the machine.
pub const BOOT_PATH: &str = "/work/boot.sh";
const SETUP_PATH: &str = "/work/setup.sh";
const SEED_PATH: &str = "/work/seed.sh";
/// The checkout inside the machine (`RD` in the boot script); the build
/// directory sits beneath it. Every path below `/work` is the slot's root: a
/// host whose slot is a directory maps `/work` to that directory (the scripts
/// do the same for the addresses they are given, see `WORK` in the boot script).
pub const CHECKOUT_DIR: &str = "/work/repo";
/// Where an embedded source tree lands inside the machine.
pub const SOURCE_DIR: &str = "/work/source";

/// Render a command list as a script that stops at the first failure.
#[must_use]
pub fn render_commands(commands: &[String]) -> String {
    let mut script = String::from("#!/bin/bash\nset -e\n");
    for command in commands {
        script.push_str(command.trim_end());
        script.push('\n');
    }
    script
}

/// The files a slot is created with: boot, setup, and seed scripts, the list of
/// requirements when the pool has any, then the embedded source files when the
/// pool has any.
#[must_use]
pub fn slot_files(config: &PoolConfig) -> Vec<(String, String)> {
    let mut files = vec![
        (BOOT_PATH.to_string(), BOOT_SCRIPT.to_string()),
        (SETUP_PATH.to_string(), render_commands(&config.setup)),
        (SEED_PATH.to_string(), render_commands(&config.seed)),
    ];
    if !config.requires.is_empty() {
        files.push((
            requires::LIST_PATH.to_string(),
            requires::render_list(&config.requires),
        ));
    }
    if let Some(source) = &config.embedded {
        files.extend(source.files.iter().cloned());
    }
    files
}

/// What the machine host does for its slots, as the boot script is told.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SlotMode {
    /// The host already mounts the memory disk over the build directory, so
    /// the boot script must not (`BUILD_MOUNTED`).
    pub mounted: bool,
    /// The build directory is a plain directory that persists (`BUILD_PERSISTS`).
    pub persists: bool,
    /// The scripts run as an ordinary user (`UNPRIVILEGED`).
    pub unprivileged: bool,
}

/// The environment a slot is created with. `GH_TOKEN` is set only when there
/// is a token; an embedded source needs none. `BUILD_DIR` and `TMPFS_SIZE`
/// are set only for a pool with a build directory. `WORKTREE_OF` is set only
/// for a pool whose slots are Git worktrees of a local repository: the
/// provider makes the checkout from it before the boot script runs. `DEPTH`
/// is the pool's fetch depth. `mode` is what the machine host
/// does for the slot (see [`SlotMode`]).
#[must_use]
pub fn slot_env(
    config: &PoolConfig,
    token: &Secret,
    base: &str,
    mode: SlotMode,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::from([
        ("REMOTE".to_string(), config.remote_address()),
        ("BASE".to_string(), base.to_string()),
        ("DEPTH".to_string(), config.fetch_depth().to_string()),
    ]);
    if let Some(dir) = &config.build_dir {
        env.insert("BUILD_DIR".to_string(), dir.clone());
        env.insert("TMPFS_SIZE".to_string(), config.tmpfs_size().to_string());
    }
    if let Some(repo) = config.worktree_source() {
        env.insert("WORKTREE_OF".to_string(), repo.display().to_string());
    }
    for (name, on) in [
        ("BUILD_MOUNTED", mode.mounted),
        ("BUILD_PERSISTS", mode.persists),
        ("UNPRIVILEGED", mode.unprivileged),
    ] {
        if on {
            env.insert(name.to_string(), "1".to_string());
        }
    }
    if !token.expose().is_empty() {
        env.insert("GH_TOKEN".to_string(), token.expose().to_string());
    }
    if let Some(source) = &config.embedded {
        env.insert("SOURCE_DIR".to_string(), SOURCE_DIR.to_string());
        env.extend(source.env.clone());
    }
    env
}

/// The command a slot runs on every boot.
#[must_use]
pub fn slot_command() -> Vec<String> {
    vec!["bash".to_string(), BOOT_PATH.to_string()]
}

#[cfg(test)]
#[path = "bootscript_test.rs"]
mod tests;

#[cfg(test)]
#[path = "bootscript_unprivileged_test.rs"]
mod unprivileged_tests;
