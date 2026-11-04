//! A scenario: one directory under `scenarios/` holding a `scenario.toml`.
//!
//! Unknown keys are refused, so a misspelled expectation fails loudly instead
//! of silently passing.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The file every scenario directory holds.
pub const FILE: &str = "scenario.toml";

/// One parsed scenario.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub world: World,
    pub run: Run,
    #[serde(default)]
    pub expect: Expect,
    /// The directory the scenario was read from.
    #[serde(skip)]
    pub dir: PathBuf,
}

/// The simulated machines the pump dispatches to.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct World {
    /// Worker pools, each a simulated machine.
    #[serde(default = "one")]
    pub workers: u32,
    /// Slots per worker pool.
    #[serde(default = "one")]
    pub slots: u32,
}

impl Default for World {
    fn default() -> Self {
        World {
            workers: 1,
            slots: 1,
        }
    }
}

/// How long to run, in virtual time.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Run {
    /// Virtual hours to simulate.
    pub virtual_hours: u64,
    /// Virtual seconds between pump ticks.
    #[serde(default = "twenty")]
    pub tick_secs: u64,
}

/// What must hold when the run ends. Every bound is a maximum.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expect {
    /// Exceptions the pump may record.
    #[serde(default)]
    pub exceptions: usize,
    /// Ticks that may return an error or panic.
    #[serde(default)]
    pub tick_errors: usize,
    /// Real seconds the whole run may take.
    #[serde(default = "thirty")]
    pub max_wall_secs: u64,
    /// Commands the pump may send to the world; unbounded when absent.
    #[serde(default)]
    pub commands: Option<usize>,
}

fn one() -> u32 {
    1
}

fn twenty() -> u64 {
    20
}

fn thirty() -> u64 {
    30
}

/// Every scenario under `root`, sorted by directory name.
pub fn discover(root: &Path) -> Result<Vec<Scenario>, String> {
    let entries = fs::read_dir(root).map_err(|e| format!("{}: {e}", root.display()))?;
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.join(FILE).is_file())
        .collect();
    dirs.sort();
    dirs.iter().map(|dir| load(dir)).collect()
}

/// Parse `dir/scenario.toml`.
pub fn load(dir: &Path) -> Result<Scenario, String> {
    let path = dir.join(FILE);
    let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut scenario: Scenario =
        toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    scenario.dir = dir.to_path_buf();
    Ok(scenario)
}
