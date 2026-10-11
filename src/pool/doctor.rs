//! `vivi pool doctor`: a read-only report on what is wrong with a pool and the
//! exact command that fixes each finding. Nothing here creates, starts, stops,
//! marks or pushes anything.
//!
//! Findings fall under three headings so a reader can tell which side a
//! problem is on: the machine host (the provider supplies its own checks), the
//! source repository host, and Vivi's own pool state. A check that cannot run
//! because an earlier one failed is `skipped`, not `fail`.

use serde::Serialize;

use super::config::PoolConfig;
use super::doctor_pool::{self, PoolFacts};
use super::doctor_repo::{self, GithubView};
use super::provider::PoolProvider;
use super::record::OpenDispatch;
use super::slot::{self, SlotStatus};
use crate::error::VivariumError;

/// The outcome of one check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Warn,
    Fail,
    Skipped,
}

impl Status {
    fn word(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Fail => "fail",
            Self::Skipped => "skipped",
        }
    }
}

/// One line of the report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    pub group: String,
    pub name: String,
    pub status: Status,
    pub detail: String,
    /// The command (or step) that fixes a `warn` or `fail`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

impl Check {
    fn new(group: &str, name: &str, status: Status, detail: String, fix: Option<String>) -> Self {
        Self {
            group: group.to_string(),
            name: name.to_string(),
            status,
            detail,
            fix,
        }
    }

    #[must_use]
    pub fn ok(group: &str, name: &str, detail: impl Into<String>) -> Self {
        Self::new(group, name, Status::Ok, detail.into(), None)
    }

    #[must_use]
    pub fn warn(
        group: &str,
        name: &str,
        detail: impl Into<String>,
        fix: impl Into<String>,
    ) -> Self {
        Self::new(group, name, Status::Warn, detail.into(), Some(fix.into()))
    }

    #[must_use]
    pub fn fail(
        group: &str,
        name: &str,
        detail: impl Into<String>,
        fix: impl Into<String>,
    ) -> Self {
        Self::new(group, name, Status::Fail, detail.into(), Some(fix.into()))
    }

    #[must_use]
    pub fn skipped(group: &str, name: &str, reason: impl Into<String>) -> Self {
        Self::new(group, name, Status::Skipped, reason.into(), None)
    }
}

/// How many checks ended each way.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Counts {
    pub ok: usize,
    pub warn: usize,
    pub fail: usize,
    pub skipped: usize,
}

/// The whole report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub checks: Vec<Check>,
}

#[derive(Serialize)]
struct JsonReport<'a> {
    ok: bool,
    counts: Counts,
    checks: &'a [Check],
}

impl Report {
    #[must_use]
    pub fn counts(&self) -> Counts {
        let mut counts = Counts::default();
        for check in &self.checks {
            match check.status {
                Status::Ok => counts.ok += 1,
                Status::Warn => counts.warn += 1,
                Status::Fail => counts.fail += 1,
                Status::Skipped => counts.skipped += 1,
            }
        }
        counts
    }

    /// True when no check failed (warnings do not count).
    #[must_use]
    pub fn passed(&self) -> bool {
        self.counts().fail == 0
    }

    /// The report as pretty JSON: `ok`, `counts`, and every check.
    ///
    /// # Errors
    /// Returns an error if encoding fails.
    pub fn to_json(&self) -> Result<String, VivariumError> {
        serde_json::to_string_pretty(&JsonReport {
            ok: self.passed(),
            counts: self.counts(),
            checks: &self.checks,
        })
        .map_err(|e| VivariumError::Other(format!("failed to encode JSON: {e}")))
    }

    /// The report as text: one heading per group, one line per check, and a
    /// `fix:` line under each warn or fail.
    #[must_use]
    pub fn render(&self) -> String {
        let width = self.checks.iter().map(|c| c.name.len()).max().unwrap_or(0);
        let mut lines: Vec<String> = Vec::new();
        let mut group = "";
        for check in &self.checks {
            if check.group != group {
                if !lines.is_empty() {
                    lines.push(String::new());
                }
                lines.push(check.group.clone());
                group = &check.group;
            }
            lines.push(format!(
                "  {:<7}  {:<width$}  {}",
                check.status.word(),
                check.name,
                check.detail
            ));
            if let Some(fix) = &check.fix {
                lines.push(format!("           fix: {fix}"));
            }
        }
        let c = self.counts();
        lines.push(String::new());
        lines.push(format!(
            "{} ok, {} warn, {} fail, {} skipped",
            c.ok, c.warn, c.fail, c.skipped
        ));
        lines.join("\n") + "\n"
    }
}

/// Everything `run` reads. GitHub is reached through `github` so tests run
/// without a network: it gets the config and the distinct seeded bases found
/// in the pool, and returns what it saw.
pub struct Inputs<'a> {
    pub provider: &'a dyn PoolProvider,
    pub config: &'a PoolConfig,
    pub mailspace_name: &'a str,
    pub open: &'a [OpenDispatch],
    pub now: u64,
    pub github: &'a dyn Fn(&PoolConfig, &[String]) -> GithubView,
}

/// Run every check. Read-only.
#[must_use]
pub fn run(inputs: &Inputs) -> Report {
    let config = inputs.config;
    let env = config
        .harness
        .as_ref()
        .map(|h| h.env.clone())
        .unwrap_or_default();
    let mut checks = inputs.provider.doctor(&env);
    let provider_failed = checks.iter().any(|c| c.status == Status::Fail);
    let slots = (!provider_failed).then(|| list_slots(inputs));
    let bases = slots.as_ref().map(distinct_bases).unwrap_or_default();
    let view = (inputs.github)(config, &bases);
    checks.extend(doctor_repo::checks(&config.repo, &view));
    checks.extend(doctor_pool::checks(&PoolFacts {
        config: config
            .validate(inputs.mailspace_name)
            .map_err(|e| e.to_string()),
        max: config.slots,
        slots,
        open: inputs.open,
        now: inputs.now,
        github: &view,
    }));
    Report { checks }
}

fn list_slots(inputs: &Inputs) -> Result<Vec<SlotStatus>, String> {
    let pool = inputs.config.pool_name(inputs.mailspace_name);
    inputs
        .provider
        .list()
        .map(|machines| slot::pool_slots(&pool, &machines))
        .map_err(|e| e.to_string())
}

fn distinct_bases(slots: &Result<Vec<SlotStatus>, String>) -> Vec<String> {
    let mut bases: Vec<String> = slots
        .iter()
        .flatten()
        .filter_map(|s| s.base.clone())
        .collect();
    bases.sort();
    bases.dedup();
    bases
}

#[cfg(test)]
#[path = "doctor_test.rs"]
mod tests;
