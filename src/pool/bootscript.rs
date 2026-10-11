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

/// The files a slot is created with: boot, setup, and seed scripts.
#[must_use]
pub fn slot_files(config: &PoolConfig) -> Vec<(String, String)> {
    vec![
        (BOOT_PATH.to_string(), BOOT_SCRIPT.to_string()),
        (SETUP_PATH.to_string(), render_commands(&config.setup)),
        (SEED_PATH.to_string(), render_commands(&config.seed)),
    ]
}

/// The environment a slot is created with.
#[must_use]
pub fn slot_env(config: &PoolConfig, token: &Secret, base: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("REPO".to_string(), config.repo.clone()),
        ("BASE".to_string(), base.to_string()),
        ("GH_TOKEN".to_string(), token.expose().to_string()),
        ("BUILD_DIR".to_string(), config.build_dir.clone()),
        ("TMPFS_SIZE".to_string(), config.tmpfs.clone()),
    ])
}

/// The command a slot runs on every boot.
#[must_use]
pub fn slot_command() -> Vec<String> {
    vec!["bash".to_string(), BOOT_PATH.to_string()]
}

#[cfg(test)]
#[path = "bootscript_test.rs"]
mod tests;
