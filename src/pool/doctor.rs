//! `vivi pool doctor`: a read-only report on what is wrong with a pool and the
//! exact command that fixes each finding. Nothing here creates, starts, stops,
//! marks or pushes anything.
//!
//! Findings fall under three headings so a reader can tell which side a
//! problem is on: the machine host (the provider supplies its own checks), the
//! source repository host, and Vivi's own pool state. A check that cannot run
//! because an earlier one failed is `skipped`, not `fail`.

use std::collections::BTreeMap;

use serde::Serialize;

use super::config::{PoolConfig, RepoAccess};
use super::doctor_pool::{self, MarkView, PoolFacts, RequiresView};
use super::doctor_repo::{self, GithubView};
use super::provider::PoolProvider;
use super::record::OpenDispatch;
use super::requires;
use super::slot::{self, PoolId, SlotStatus};
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
    /// The status as the report prints it.
    #[must_use]
    pub fn word(self) -> &'static str {
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
    /// The pool the check is about (empty until a report stamps it).
    pub pool: String,
    pub group: String,
    pub name: String,
    pub status: Status,
    pub detail: String,
    /// The command (or step) that fixes a `warn` or `fail`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
    /// How long the check took in milliseconds, for the steps of the live test.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub millis: Option<u64>,
}

impl Check {
    fn new(group: &str, name: &str, status: Status, detail: String, fix: Option<String>) -> Self {
        Self {
            pool: String::new(),
            group: group.to_string(),
            name: name.to_string(),
            status,
            detail,
            fix,
            millis: None,
        }
    }

    /// The same check, about the pool `pool`.
    #[must_use]
    pub fn in_pool(mut self, pool: &str) -> Self {
        self.pool = pool.to_string();
        self
    }

    /// A timed step of the live test.
    #[must_use]
    pub fn timed(mut self, took: std::time::Duration) -> Self {
        self.millis = Some(u64::try_from(took.as_millis()).unwrap_or(u64::MAX));
        self
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
    /// The checks about one pool, as a report of their own.
    #[must_use]
    pub fn of_pool(&self, pool: &str) -> Report {
        Report {
            checks: self
                .checks
                .iter()
                .filter(|c| c.pool == pool)
                .cloned()
                .collect(),
        }
    }

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

    /// True when a check of the machine host failed: any failure outside the
    /// repository and Vivi's own pool groups.
    #[must_use]
    pub fn provider_failed(&self) -> bool {
        self.checks.iter().any(|c| {
            c.status == Status::Fail
                && c.group != doctor_repo::GROUP
                && c.group != doctor_pool::GROUP
        })
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
        let mut heading = String::new();
        for check in &self.checks {
            let this = if check.pool.is_empty() {
                check.group.clone()
            } else {
                format!("{}  (pool {})", check.group, check.pool)
            };
            if this != heading {
                if !lines.is_empty() {
                    lines.push(String::new());
                }
                lines.push(this.clone());
                heading = this;
            }
            #[allow(clippy::cast_precision_loss)]
            let took = check
                .millis
                .map_or(String::new(), |ms| format!(" ({:.1}s)", ms as f64 / 1000.0));
            lines.push(format!(
                "  {:<7}  {:<width$}  {}{took}",
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
    pub id: &'a PoolId,
    pub open: &'a [OpenDispatch],
    pub now: u64,
    pub github: &'a dyn Fn(&RepoAccess, &[String]) -> GithubView,
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
    let view = if let Some(access) = config.repo_access() {
        let view = (inputs.github)(&access, &bases);
        checks.extend(doctor_repo::checks(access.repo, &view));
        view
    } else {
        checks.push(doctor_repo::absent_check());
        GithubView {
            absent: true,
            ..GithubView::default()
        }
    };
    let requires = RequiresView {
        configured: &config.requires,
        marks: read_marks(inputs, slots.as_ref()),
    };
    checks.extend(doctor_pool::checks(&PoolFacts {
        pool: &inputs.id.pool,
        space: &inputs.id.mailspace,
        config: config.validate(inputs.id).map_err(|e| e.to_string()),
        max: config.slots,
        slots,
        open: inputs.open,
        now: inputs.now,
        github: &view,
        requires: &requires,
        lifetime: inputs.provider.slot_lifetime(),
        grace: inputs.provider.setup_grace_secs(),
        billing: &inputs.provider.billing_note(),
    }));
    let checks = checks.into_iter().map(|c| c.in_pool(&inputs.id.pool));
    Report {
        checks: checks.collect(),
    }
}

/// What each slot's disk says about the pool's `requires`: read without
/// starting anything, so a running slot (or a host that cannot read a stopped
/// disk) is reported as not read. Nothing is read when nothing is configured.
fn read_marks(
    inputs: &Inputs,
    slots: Option<&Result<Vec<SlotStatus>, String>>,
) -> BTreeMap<String, MarkView> {
    let Some(Ok(slots)) = slots.filter(|_| !inputs.config.requires.is_empty()) else {
        return BTreeMap::new();
    };
    slots
        .iter()
        .map(|slot| {
            let view = if slot.provider_state == "stopped" {
                read_mark(inputs.provider, &slot.id)
            } else {
                MarkView::Unread("the slot is running; doctor never starts or touches one".into())
            };
            (slot.name.clone(), view)
        })
        .collect()
}

fn read_mark(provider: &dyn PoolProvider, id: &str) -> MarkView {
    match provider.read_stopped(id, &[requires::MARK_PATH]) {
        Ok(Some(files)) => files
            .get(requires::MARK_PATH)
            .map_or(MarkView::Absent, |b| {
                MarkView::Mark(String::from_utf8_lossy(b).into_owned())
            }),
        Ok(None) => MarkView::Unread("this host cannot read a stopped slot's disk".into()),
        Err(e) => MarkView::Unread(e.to_string()),
    }
}

fn list_slots(inputs: &Inputs) -> Result<Vec<SlotStatus>, String> {
    inputs
        .provider
        .list()
        .map(|machines| slot::pool_slots(inputs.id, &machines))
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
