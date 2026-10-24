//! `vivi config show`, as a value. The caller prints.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::VivariumError;
use crate::mailspace::Mailspace;
use crate::mailspace::inherit::ConfigOrigins;
use crate::routing::{self, ConfigOverview};

use super::origins::origins_of;

/// What `vivi config show` renders: the overview, plus where merged values came from.
#[derive(Debug, Clone)]
pub struct ShowReport {
    pub overview: ConfigOverview,
    pub origins: ConfigOrigins,
}

/// Build the show report for `project` (`None` discovers from the current directory).
///
/// `resolved` fills live free-slot counts by running `pool status --porcelain`
/// of the current executable. This function does not print.
///
/// # Errors
/// Returns an error when the mailspace cannot be opened or its config cannot be read.
pub fn show(project: Option<&Path>, resolved: bool) -> Result<ShowReport, VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    let free = resolved.then(|| live_free(&mailspace));
    let overview = routing::overview(&mailspace.config, free.as_ref());
    let origins = origins_of(&mailspace)?;
    Ok(ShowReport { overview, origins })
}

fn live_free(mailspace: &Mailspace) -> BTreeMap<String, u32> {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("vivi"));
    let Ok(out) = Command::new(exe)
        .args(["pool", "status", "--porcelain"])
        .current_dir(&mailspace.root)
        .output()
    else {
        return BTreeMap::new();
    };
    count_free(&String::from_utf8_lossy(&out.stdout))
}

fn count_free(text: &str) -> BTreeMap<String, u32> {
    let mut free = BTreeMap::new();
    for line in text.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.len() >= 5 && tokens[0] == "slot" && tokens[4] == "free" {
            *free.entry(tokens[1].to_owned()).or_insert(0) += 1;
        }
    }
    free
}

#[cfg(test)]
#[path = "show_test.rs"]
mod tests;
