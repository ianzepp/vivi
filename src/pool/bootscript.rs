//! The in-machine boot script and the files and environment that drive it.
//!
//! The script itself is generic and embedded; everything specific to a
//! project reaches it as environment values and as the rendered `setup.sh`
//! and `seed.sh` files.

use std::collections::BTreeMap;

use super::config::PoolConfig;
use super::github::Secret;

/// The generic boot script, embedded in the binary.
pub const BOOT_SCRIPT: &str = include_str!("boot-slot.sh");

/// Where the boot script lands inside the machine.
pub const BOOT_PATH: &str = "/work/boot.sh";
const SETUP_PATH: &str = "/work/setup.sh";
const SEED_PATH: &str = "/work/seed.sh";
/// The checkout inside the machine (`RD` in the boot script); the build
/// directory sits beneath it.
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

/// The files a slot is created with: boot, setup, and seed scripts, then the
/// embedded source files when the pool has any.
#[must_use]
pub fn slot_files(config: &PoolConfig) -> Vec<(String, String)> {
    let mut files = vec![
        (BOOT_PATH.to_string(), BOOT_SCRIPT.to_string()),
        (SETUP_PATH.to_string(), render_commands(&config.setup)),
        (SEED_PATH.to_string(), render_commands(&config.seed)),
    ];
    if let Some(source) = &config.embedded {
        files.extend(source.files.iter().cloned());
    }
    files
}

/// The environment a slot is created with. `GH_TOKEN` is set only when there
/// is a token; an embedded source needs none. `mounted` says the machine host
/// already mounts the memory disk over the build directory, so the boot script
/// must not (`BUILD_MOUNTED`).
#[must_use]
pub fn slot_env(
    config: &PoolConfig,
    token: &Secret,
    base: &str,
    mounted: bool,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::from([
        ("REMOTE".to_string(), config.remote_address()),
        ("BASE".to_string(), base.to_string()),
        ("BUILD_DIR".to_string(), config.build_dir.clone()),
        ("TMPFS_SIZE".to_string(), config.tmpfs_size().to_string()),
    ]);
    if mounted {
        env.insert("BUILD_MOUNTED".to_string(), "1".to_string());
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
