//! The failures page's data: runs collected in a window, counted by kind,
//! with the window before for the trend, and what the landings recorded.
//!
//! Everything comes from indexed reads of `slot_runs` (a range on
//! `collected_at`). The kind of each run is decided in SQL (see
//! `queries::FAILURE_KINDS`); this module groups, splits and ranks.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use rusqlite::Connection;
use vivi_error::VivariumError;

use super::config;
use super::store::db_err;
use super::{RunsState, with_runs};
use crate::queries::{FAILURE_KINDS, LANDING_REASONS, NEED_KINDS};

/// Rows kept per list on the page.
pub const LIST_ROWS: usize = 15;
/// Needs with at least this many runs and no landing are listed.
pub const REPEAT_RUNS: i64 = 3;
/// Groups read from the landing and need queries.
const LANDING_GROUPS: i64 = 5000;
const NEED_GROUPS: i64 = 20_000;

/// How far back the page looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Window {
    Hour,
    Day,
    Week,
}

impl Window {
    /// Every window, in menu order.
    pub const ALL: [Window; 3] = [Window::Hour, Window::Day, Window::Week];

    /// The query-string word: `1h`, `24h` or `7d`.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Hour => "1h",
            Self::Day => "24h",
            Self::Week => "7d",
        }
    }

    /// The window a query-string word names; anything else is 24 h.
    #[must_use]
    pub fn parse(word: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|window| window.word() == word)
            .unwrap_or(Self::Day)
    }

    /// Length in seconds.
    #[must_use]
    pub fn secs(self) -> i64 {
        match self {
            Self::Hour => 3600,
            Self::Day => 24 * 3600,
            Self::Week => 7 * 24 * 3600,
        }
    }
}

/// The failure kinds, in display order: the query's code and its label.
pub const KINDS: [(&str, &str); 9] = [
    ("timeout", "timeout"),
    ("lost", "lost"),
    ("agent", "agent failed"),
    ("check", "check red"),
    ("not_pushed", "not pushed"),
    ("landing_red", "landing red"),
    ("conflict", "conflict"),
    ("raced", "raced"),
    ("other", "other"),
];

/// Which way a count moved against the window before.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trend {
    /// Nothing in the window before.
    New,
    Up,
    Down,
    /// Unchanged, or within a tenth.
    Flat,
}

/// One failure kind's counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KindCount {
    pub code: &'static str,
    pub label: &'static str,
    pub count: i64,
    /// The same kind in the window just before.
    pub previous: i64,
}

impl KindCount {
    /// The trend of `count` against `previous`.
    #[must_use]
    pub fn trend(&self) -> Trend {
        if self.previous == 0 {
            return if self.count == 0 {
                Trend::Flat
            } else {
                Trend::New
            };
        }
        let delta = self.count - self.previous;
        if delta.abs() * 10 <= self.previous {
            Trend::Flat
        } else if delta > 0 {
            Trend::Up
        } else {
            Trend::Down
        }
    }
}

/// A path prefix and how many conflicting runs touched it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotPath {
    pub prefix: String,
    pub count: i64,
}

/// A failing command and how many landings it turned red.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedCommand {
    pub command: String,
    pub count: i64,
}

/// A need whose runs keep coming back without a landing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepeatNeed {
    pub need: String,
    pub runs: i64,
    /// Failure kind label and count, biggest first; `ok` runs show as
    /// "not landed".
    pub mix: Vec<(String, i64)>,
}

/// Everything the failures page shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailuresData {
    pub window: Window,
    /// The pool filter; empty for every pool.
    pub pool: String,
    /// The pools config declares, for the filter.
    pub pools: Vec<String>,
    /// Runs collected in the window, failed or not.
    pub collected: i64,
    pub kinds: Vec<KindCount>,
    pub hot_paths: Vec<HotPath>,
    pub red_commands: Vec<RedCommand>,
    pub repeat_needs: Vec<RepeatNeed>,
}

impl FailuresData {
    /// Failures in the window.
    #[must_use]
    pub fn failed(&self) -> i64 {
        self.kinds.iter().map(|kind| kind.count).sum()
    }
}

/// Read the failures of the project at `root` as of `now`.
///
/// # Errors
/// Returns an error when the config or the store cannot be read.
pub fn load(
    root: &Path,
    now: i64,
    window: Window,
    pool: &str,
) -> Result<RunsState<FailuresData>, VivariumError> {
    let pools = config::declared_pools(&config::load(root)?)
        .into_iter()
        .map(|declared| declared.name)
        .collect();
    with_runs(root, |conn| {
        let since = now - window.secs();
        let (kinds, collected) = kind_counts(conn, now, window, pool)?;
        let (hot_paths, red_commands) = landing_reasons(conn, since, pool)?;
        Ok(FailuresData {
            window,
            pool: pool.to_string(),
            pools,
            collected,
            kinds,
            hot_paths,
            red_commands,
            repeat_needs: repeat_needs(conn, since, pool)?,
        })
    })
}

/// The kinds with a count in either window, in display order, and the runs
/// collected in the current window.
fn kind_counts(
    conn: &Connection,
    now: i64,
    window: Window,
    pool: &str,
) -> Result<(Vec<KindCount>, i64), VivariumError> {
    let split = now - window.secs();
    let context = "count failures by kind";
    let mut stmt = conn
        .prepare_cached(FAILURE_KINDS.sql)
        .map_err(db_err(context))?;
    let rows = stmt
        .query_map(
            rusqlite::params![split - window.secs(), split, pool],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .map_err(db_err(context))?;
    let mut now_counts: BTreeMap<String, i64> = BTreeMap::new();
    let mut before_counts: BTreeMap<String, i64> = BTreeMap::new();
    let mut collected = 0;
    for row in rows {
        let (code, period, count) = row.map_err(db_err(context))?;
        if period == 0 {
            collected += count;
            *now_counts.entry(code).or_default() += count;
        } else {
            *before_counts.entry(code).or_default() += count;
        }
    }
    let kinds = KINDS
        .iter()
        .map(|(code, label)| KindCount {
            code,
            label,
            count: now_counts.get(*code).copied().unwrap_or(0),
            previous: before_counts.get(*code).copied().unwrap_or(0),
        })
        .filter(|kind| kind.count > 0 || kind.previous > 0)
        .collect();
    Ok((kinds, collected))
}

/// Hot paths from conflict reasons, and red landings by failing command.
///
/// The contract with the landing writer: a `conflict` reason is the
/// conflicting files joined by newlines; a `red` reason is the failing
/// command. Counted in Rust because a reason is free text.
fn landing_reasons(
    conn: &Connection,
    since: i64,
    pool: &str,
) -> Result<(Vec<HotPath>, Vec<RedCommand>), VivariumError> {
    let context = "read landing reasons";
    let mut stmt = conn
        .prepare_cached(LANDING_REASONS.sql)
        .map_err(db_err(context))?;
    let rows = stmt
        .query_map(rusqlite::params![since, pool, LANDING_GROUPS], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(db_err(context))?;
    let mut paths: BTreeMap<String, i64> = BTreeMap::new();
    let mut commands: BTreeMap<String, i64> = BTreeMap::new();
    for row in rows {
        let (landing, reason, count) = row.map_err(db_err(context))?;
        if landing == "conflict" {
            for prefix in distinct_prefixes(&reason) {
                *paths.entry(prefix).or_default() += count;
            }
        } else if let Some(command) = command_name(&reason) {
            *commands.entry(command).or_default() += count;
        }
    }
    Ok((
        ranked(paths, |prefix, count| HotPath { prefix, count }),
        ranked(commands, |command, count| RedCommand { command, count }),
    ))
}

fn ranked<T>(counts: BTreeMap<String, i64>, make: impl Fn(String, i64) -> T) -> Vec<T> {
    let mut list: Vec<(String, i64)> = counts.into_iter().collect();
    list.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    list.into_iter()
        .take(LIST_ROWS)
        .map(|(name, count)| make(name, count))
        .collect()
}

/// The distinct two-component prefixes of the files a conflict names.
fn distinct_prefixes(reason: &str) -> BTreeSet<String> {
    reason.lines().filter_map(path_prefix).collect()
}

/// The directory prefix of one path, at most two components deep:
/// `radix/lower/call.rs` is `radix/lower/`, `a/b.rs` is `a/`, and a file at
/// the root is itself.
#[must_use]
pub fn path_prefix(path: &str) -> Option<String> {
    let path = path.trim().trim_start_matches("./");
    if path.is_empty() {
        return None;
    }
    let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    match parts.len() {
        0 => None,
        1 => Some(parts[0].to_string()),
        n => Some(format!("{}/", parts[..(n - 1).min(2)].join("/"))),
    }
}

/// A failing command, trimmed to its first line and a sensible length.
fn command_name(reason: &str) -> Option<String> {
    let line = reason
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    Some(line.chars().take(120).collect())
}

/// Needs with [`REPEAT_RUNS`] or more runs in the window and none landed.
fn repeat_needs(
    conn: &Connection,
    since: i64,
    pool: &str,
) -> Result<Vec<RepeatNeed>, VivariumError> {
    let context = "read needs that come back";
    let mut stmt = conn
        .prepare_cached(NEED_KINDS.sql)
        .map_err(db_err(context))?;
    let rows = stmt
        .query_map(rusqlite::params![since, pool, NEED_GROUPS], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(db_err(context))?;
    let mut needs: BTreeMap<String, Vec<(String, i64)>> = BTreeMap::new();
    for row in rows {
        let (need, kind, count) = row.map_err(db_err(context))?;
        needs.entry(need).or_default().push((kind, count));
    }
    let mut out: Vec<RepeatNeed> = needs
        .into_iter()
        .filter(|(_, mix)| mix.iter().all(|(kind, _)| kind != "landed"))
        .map(|(need, mix)| RepeatNeed {
            need,
            runs: mix.iter().map(|(_, count)| count).sum(),
            mix: label_mix(mix),
        })
        .filter(|need| need.runs >= REPEAT_RUNS)
        .collect();
    out.sort_by(|a, b| b.runs.cmp(&a.runs).then_with(|| a.need.cmp(&b.need)));
    out.truncate(LIST_ROWS);
    Ok(out)
}

fn label_mix(mut mix: Vec<(String, i64)>) -> Vec<(String, i64)> {
    mix.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    mix.into_iter()
        .map(|(kind, count)| {
            let label = KINDS
                .iter()
                .find(|(code, _)| *code == kind)
                .map_or("not landed", |(_, label)| *label);
            (label.to_string(), count)
        })
        .collect()
}

#[cfg(test)]
#[path = "failures_test.rs"]
mod tests;
