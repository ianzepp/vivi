//! The overview page's data: pump health, slots per pool, and the last day
//! of runs.

use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::Connection;
use vivi_core::mailspace::MailspaceConfig;
use vivi_core::pump::health::{HealthOptions, health};
use vivi_error::VivariumError;

use super::config::{self, DeclaredPool};
use super::runs::{self, HourCount, OutcomeCount, RunRow};
use super::{RunsState, with_runs};

/// The window the overview reports on.
pub const DAY_SECS: i64 = 24 * 3600;
/// Hourly bars shown.
pub const BAR_HOURS: i64 = 24;
/// Failures listed.
pub const FAILURE_ROWS: i64 = 10;
/// Open runs read when counting busy slots.
pub const OPEN_RUN_LIMIT: i64 = 1000;

/// What the pump health read found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PumpView {
    /// The project has no `[pump]` table.
    NoPump,
    Health(PumpHealthView),
    /// The health read failed; the message is not secret.
    Unreadable(String),
}

/// The parts of the pump health verdict the overview shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PumpHealthView {
    pub verdict: &'static str,
    pub control: Option<String>,
    pub writer: &'static str,
    pub last_tick_age_secs: Option<u64>,
    pub last_tick_outcome: Option<String>,
    pub reasons: Vec<String>,
}

/// One pool's slots: declared in config, busy from open runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolSlots {
    pub pool: String,
    pub declared: u32,
    /// `None` when there is no slot index to count from.
    pub busy: Option<u32>,
}

impl PoolSlots {
    /// Declared slots not busy; never below zero.
    #[must_use]
    pub fn free(&self) -> Option<u32> {
        self.busy.map(|busy| self.declared.saturating_sub(busy))
    }
}

/// One hourly bar: runs collected in that epoch hour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HourBar {
    pub hour: i64,
    pub ok: i64,
    pub failed: i64,
}

/// The last 24 hours of runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayRuns {
    pub dispatched: i64,
    pub outcomes: Vec<OutcomeCount>,
    pub hours: Vec<HourBar>,
    pub failures: Vec<RunRow>,
    pub busy_by_pool: BTreeMap<String, u32>,
}

impl DayRuns {
    /// Collected runs that were not failures.
    #[must_use]
    pub fn collected_ok(&self) -> i64 {
        self.sum(false)
    }

    /// Collected runs that failed.
    #[must_use]
    pub fn collected_failed(&self) -> i64 {
        self.sum(true)
    }

    fn sum(&self, failed: bool) -> i64 {
        self.outcomes
            .iter()
            .filter(|row| row.failed == failed)
            .map(|row| row.count)
            .sum()
    }
}

/// Everything the overview page shows for one project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overview {
    pub now: i64,
    pub pump: PumpView,
    pub pools: Vec<PoolSlots>,
    pub runs: RunsState<DayRuns>,
}

/// Read the overview of the project at `root`, as of `now` (epoch seconds).
///
/// # Errors
/// Returns an error when the config or the store cannot be read.
pub fn load(root: &Path, now: i64) -> Result<Overview, VivariumError> {
    let config = config::load(root)?;
    let pump = pump_view(root, &config);
    let runs = with_runs(root, |conn| day_runs(conn, now))?;
    let pools = pool_slots(&config::declared_pools(&config), &runs);
    Ok(Overview {
        now,
        pump,
        pools,
        runs,
    })
}

fn pump_view(root: &Path, config: &MailspaceConfig) -> PumpView {
    if config.pump.is_none() {
        return PumpView::NoPump;
    }
    match health(root, &HealthOptions::default()) {
        Ok(answer) => PumpView::Health(PumpHealthView {
            verdict: answer.verdict.as_str(),
            control: answer.control,
            writer: answer.writer.state,
            last_tick_age_secs: answer.last_tick.as_ref().map(|tick| tick.age_secs),
            last_tick_outcome: answer.last_tick.and_then(|tick| tick.outcome),
            reasons: answer.reasons,
        }),
        Err(error) => PumpView::Unreadable(error.to_string()),
    }
}

fn day_runs(conn: &Connection, now: i64) -> Result<DayRuns, VivariumError> {
    let since = now - DAY_SECS;
    let first_hour = now / 3600 - (BAR_HOURS - 1);
    let hourly = runs::hourly_since(conn, first_hour * 3600)?;
    let open = runs::open_runs(conn, OPEN_RUN_LIMIT)?;
    let mut busy_by_pool: BTreeMap<String, u32> = BTreeMap::new();
    for run in &open {
        *busy_by_pool.entry(run.pool.clone()).or_default() += 1;
    }
    Ok(DayRuns {
        dispatched: runs::dispatched_since(conn, since)?,
        outcomes: runs::outcomes_since(conn, since)?,
        hours: hour_bars(&hourly, first_hour),
        failures: runs::failures_since(conn, since, FAILURE_ROWS)?,
        busy_by_pool,
    })
}

/// 24 bars, oldest first, starting at epoch hour `first_hour`.
fn hour_bars(counts: &[HourCount], first_hour: i64) -> Vec<HourBar> {
    (first_hour..first_hour + BAR_HOURS)
        .map(|hour| {
            let sum = |failed: bool| -> i64 {
                counts
                    .iter()
                    .filter(|row| row.hour == hour && row.failed == failed)
                    .map(|row| row.count)
                    .sum()
            };
            HourBar {
                hour,
                ok: sum(false),
                failed: sum(true),
            }
        })
        .collect()
}

/// Declared pools first, then any pool with open runs that config no longer
/// declares (shown with zero declared slots).
fn pool_slots(declared: &[DeclaredPool], runs: &RunsState<DayRuns>) -> Vec<PoolSlots> {
    let busy_of = |pool: &str| match runs {
        RunsState::Ready(day) => Some(day.busy_by_pool.get(pool).copied().unwrap_or(0)),
        _ => None,
    };
    let mut out: Vec<PoolSlots> = declared
        .iter()
        .map(|pool| PoolSlots {
            pool: pool.name.clone(),
            declared: pool.slots,
            busy: busy_of(&pool.name),
        })
        .collect();
    if let RunsState::Ready(day) = runs {
        for (pool, busy) in &day.busy_by_pool {
            if !declared.iter().any(|d| &d.name == pool) {
                out.push(PoolSlots {
                    pool: pool.clone(),
                    declared: 0,
                    busy: Some(*busy),
                });
            }
        }
    }
    out
}
