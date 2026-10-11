//! What the live test of `vivi pool doctor --test` carries with it: a tiny
//! embedded source tree, the stand-in harness, and the isolated pool config.
//!
//! The test needs no project repository, no GitHub App, no model key and no
//! agent harness. The source tree becomes a bare Git remote on each slot's own
//! disk; the stand-in harness is a shell command.

use std::collections::BTreeMap;

use super::bootscript::SOURCE_DIR;
use super::config::{EmbeddedSource, PoolCommitter, PoolConfig, PoolHarness};

/// Suffix that keeps the test pool apart from the real pool.
pub const POOL_SUFFIX: &str = "-doctor";
/// The model slug the stand-in harness accepts.
pub const MODEL: &str = "standin";
/// The branch the embedded source starts on; also the seed base.
pub const MAIN_BRANCH: &str = "main";
/// The bare remote the seed builds on each slot.
pub const REMOTE: &str = "file:///work/remote.git";
/// Seconds a finished job's machine stays up for `wait` to collect it.
pub const LINGER_SECS: &str = "30";
/// Seconds an idle machine stays up before stopping itself.
pub const IDLE_SECS: &str = "60";
/// The file the stand-in harness appends to in the checkout.
pub const HARNESS_FILE: &str = "standin.txt";

/// The embedded source tree: `(file name, contents)`.
pub const SOURCE_FILES: [(&str, &str); 3] = [
    (
        "README.md",
        "# Vivi pool test source\n\nThis tree is carried inside the vivi binary. `vivi pool doctor --test`\nturns it into a bare Git remote on a slot's own disk and runs stand-in jobs\nagainst it. It is not a real project.\n",
    ),
    ("notes.txt", "one line of text\n"),
    // The build directory is a memory mount the job runner must not clean.
    (".gitignore", "build/\n"),
];

/// The test pool's name for a real pool.
#[must_use]
pub fn pool_name(real_pool: &str) -> String {
    format!("{real_pool}{POOL_SUFFIX}")
}

/// The stand-in harness: append a line to a file in the checkout and print a
/// one-line receipt, after sleeping `sleep_secs`.
#[must_use]
pub fn harness_command(sleep_secs: u64) -> String {
    let nap = if sleep_secs == 0 {
        String::new()
    } else {
        format!("sleep {sleep_secs}; ")
    };
    format!(
        "{nap}echo \"stand-in {{model}} $(date +%s)\" >> {HARNESS_FILE} && echo \"stand-in receipt: appended one line to {HARNESS_FILE}\""
    )
}

/// The config the test runs under: the real pool's machine host, region and
/// image, with everything else replaced so no project setting is used.
#[must_use]
pub fn test_config(real: &PoolConfig, pool: &str) -> PoolConfig {
    let mut config = real.clone();
    config.name = Some(pool.to_string());
    config.slots = 2;
    config.setup = Vec::new();
    config.seed = vec!["true".to_string()];
    config.build_dir = "build".to_string();
    config.fly.size = "shared-cpu-1x".to_string();
    config.fly.memory = 256;
    config.fly.tmpfs = "32m".to_string();
    config.harness = Some(PoolHarness {
        name: "builtin".to_string(),
        run: harness_command(0),
        models: vec![MODEL.to_string()],
        env: Vec::new(),
        rules: None,
        unknown: BTreeMap::new(),
    });
    config.committer = Some(PoolCommitter {
        name: "Vivi pool test".to_string(),
        email: "pool-test@localhost".to_string(),
        unknown: BTreeMap::new(),
    });
    config.embedded = Some(EmbeddedSource {
        remote: REMOTE.to_string(),
        files: SOURCE_FILES
            .iter()
            .map(|(name, text)| (format!("{SOURCE_DIR}/{name}"), (*text).to_string()))
            .collect(),
        env: BTreeMap::from([
            ("LINGER".to_string(), LINGER_SECS.to_string()),
            ("IDLE_LIMIT".to_string(), IDLE_SECS.to_string()),
        ]),
    });
    config
}

#[cfg(test)]
#[path = "selftest_source_test.rs"]
mod tests;
