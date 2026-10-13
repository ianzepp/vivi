//! Pull: read the project's letters after the stored cursor and file the ones
//! this mailspace does not hold, byte for byte.
//!
//! Every record is verified (`sha256(bytes) == content_sha256`) before it is
//! looked at, its envelope recipients must agree with the letter's own To and
//! Cc, and a From address that claims a mailspace prefix must carry the one
//! the server stamped for the sender (see [`crate::sync::filer`]). Ingest is
//! idempotent and the cursor moves only after a whole page is handled, so a
//! failure replays the page and changes nothing twice.
//!
//! Filing follows the letter's addresses, never its author, and is
//! additive. One row per To or Cc address that names this mailspace, under
//! that role. If the From address names this mailspace too, also a `sent` row
//! under the From role: exactly what a local send writes, so a restore from
//! an empty store reproduces both. A letter with neither is held under its
//! first To address. A letter addressed to the reserved `postmaster` is
//! project governance: the pull counts it and files it under no role.

use std::collections::BTreeMap;

use crate::error::VivariumError;
use crate::mailspace::Mailspace;
use crate::storage::Storage;
use crate::sync::filer::Filer;
use crate::sync::link::{self, LinkOutcome};
use crate::sync::state::{Floor, SyncState};
use crate::sync::transport::{ListQuery, PAGE_LIMIT, Page, Record, Transport};
use crate::sync::{SyncSettings, UIDVALIDITY, sync_dir};

/// A record this pass would not file, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusedRecord {
    pub message_id: String,
    pub reason: String,
}

/// What one pull did, for the operator.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PullReport {
    pub fetched: usize,
    /// New letters filed, by local role.
    pub filed: BTreeMap<String, usize>,
    /// Letters this mailspace already held; left untouched.
    pub existing: usize,
    /// Reply links recorded.
    pub linked: usize,
    pub refused: Vec<RefusedRecord>,
    /// Problems that did not stop the pull (a binding that could not be
    /// written).
    pub warnings: Vec<String>,
    pub cursor: u64,
    /// Pages read.
    pub pages: usize,
    /// Evicted letters the pull brought back (also counted in `filed`).
    pub restored: usize,
    /// Message ids of one row of each letter written this run, for the
    /// reference follow.
    pub filed_ids: Vec<String>,
    /// Letters filed this run under a role name this mailspace does not have,
    /// by name. They are held, not refused.
    pub unknown_roles: BTreeMap<String, usize>,
    /// Governance letters: envelope or To/Cc names the reserved
    /// `postmaster`. They are counted and filed under no role.
    pub governance: usize,
    /// The server log's epoch changed mid-pull: `(old, new)`. The page that
    /// carried the new epoch was read with a stale cursor and discarded
    /// unfiled; the cursor and floor were reset and the pull re-read from
    /// sequence 0.
    pub epoch_reset: Option<(Option<u32>, u32)>,
    /// Content ids of the event letters and work items handled since the
    /// last settle, new or already held. Drained by [`settle_events`].
    pub touched: Vec<String>,
    /// Items whose rows or node moved to match their latest event.
    pub events_applied: usize,
    /// Events not applied: not from the item's owner, or for a sealed item.
    pub events_ignored: usize,
}

impl PullReport {
    /// One-line operator summary.
    #[must_use]
    pub fn summary(&self) -> String {
        let filed: usize = self.filed.values().sum();
        let by_role = self
            .filed
            .iter()
            .map(|(role, count)| format!("{count} {role}"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut new = if by_role.is_empty() {
            format!("{filed} new")
        } else {
            format!("{filed} new ({by_role})")
        };
        if self.restored > 0 {
            new = format!("{new}, {} restored", self.restored);
        }
        let refused = if self.refused.is_empty() {
            "0 refused".to_string()
        } else {
            let shown = self.refused.iter().take(5).map(|r| r.message_id.as_str());
            let ids = shown.collect::<Vec<_>>().join(", ");
            let more = self.refused.len().saturating_sub(5);
            let tail = if more > 0 {
                format!(", ... and {more} more")
            } else {
                String::new()
            };
            format!("{} refused: {ids}{tail}", self.refused.len())
        };
        let governance = (self.governance > 0)
            .then(|| format!(", {} governance letter(s)", self.governance))
            .unwrap_or_default();
        let epoch = self
            .epoch_reset
            .as_ref()
            // A reset always names the epoch it replaced.
            .and_then(|(old, new)| old.map(|old| (old, *new)))
            .map_or(String::new(), |(old, new)| {
                format!(
                    ", server epoch changed ({old} -> {new}): cursor and floor reset, \
                     re-read from sequence 0"
                )
            });
        let held = self
            .held_note()
            .map(|note| format!(", {note}"))
            .unwrap_or_default();
        format!(
            "sync pull: {} fetched, {new}, {} already present, {} reply link(s), \
             {refused}{governance}{held}{}{}, cursor {}",
            self.fetched,
            self.existing,
            self.linked,
            self.event_note(),
            epoch,
            self.cursor
        )
    }

    /// `, N event(s) applied, M event(s) ignored: not from the item's owner`
    /// when item-state events were seen, else nothing.
    #[must_use]
    pub fn event_note(&self) -> String {
        let applied = (self.events_applied > 0)
            .then(|| format!(", {} event(s) applied", self.events_applied));
        let ignored = (self.events_ignored > 0).then(|| {
            format!(
                ", {} event(s) ignored: not from the item's owner",
                self.events_ignored
            )
        });
        [applied, ignored].into_iter().flatten().collect()
    }

    /// `N held for unknown role(s): ghost, ...` when letters were filed for
    /// roles this mailspace does not have.
    #[must_use]
    pub fn held_note(&self) -> Option<String> {
        if self.unknown_roles.is_empty() {
            return None;
        }
        let total: usize = self.unknown_roles.values().sum();
        let names: Vec<&str> = self.unknown_roles.keys().map(String::as_str).collect();
        Some(format!(
            "{total} held for unknown role(s): {}",
            names.join(", ")
        ))
    }

    /// Why the command should exit non-zero, if it should.
    #[must_use]
    pub fn failure(&self) -> Option<String> {
        (!self.refused.is_empty()).then(|| {
            format!(
                "{} record(s) refused (hash, envelope or From mismatch, id clash, or bad id); see above",
                self.refused.len()
            )
        })
    }
}

/// What to do differently from continuing at the stored cursor.
#[derive(Debug, Clone, Default)]
pub struct PullPlan<'a> {
    /// Start at this time (RFC 3339 UTC) instead of a cursor. Only valid on a
    /// mailspace with no pull cursor for the project.
    pub window: Option<&'a str>,
    /// Reset the cursor and floor to zero and replay everything.
    pub resync: bool,
}

/// One page, as it was handled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageProgress {
    /// 1-based page number within this pull.
    pub page: usize,
    pub records: usize,
    /// The saved cursor after the page.
    pub cursor: u64,
    /// Whether another page follows.
    pub more: bool,
}

/// Pull everything after the stored cursor, page by page.
///
/// # Errors
/// Returns an error when the read fails or storage or the state file cannot
/// be written.
pub fn pull(
    mailspace: &Mailspace,
    settings: &SyncSettings,
    transport: &dyn Transport,
) -> Result<PullReport, VivariumError> {
    pull_with(
        mailspace,
        settings,
        transport,
        &PullPlan::default(),
        &mut |_| {},
    )
}

/// [`pull`] with a plan and a per-page progress hook.
///
/// A window starts a fresh mailspace at a time and records the **floor**;
/// a resync replays everything and records full history. The cursor is saved
/// after every page, so an interrupted pull resumes where it stopped.
///
/// # Errors
/// Returns an error when a window is asked of a mailspace that already pulled
/// this project, the read fails, or storage or the state file cannot be
/// written.
pub fn pull_with(
    mailspace: &Mailspace,
    settings: &SyncSettings,
    transport: &dyn Transport,
    plan: &PullPlan<'_>,
    progress: &mut dyn FnMut(&PageProgress),
) -> Result<PullReport, VivariumError> {
    let dir = sync_dir(&mailspace.dir);
    let mut state = load_state(&dir, settings, plan)?;
    let before_plan = state.clone();
    apply_plan(&mut state, plan, settings)?;
    // The plan's changes are saved with the first page that is read, so a
    // failed read leaves the state file exactly as it was.
    let mut unsaved = state != before_plan;
    let mut storage = mailspace.storage()?;
    let mut filer = Filer::new(mailspace, settings);
    let mut report = PullReport::default();
    let mut epoch_resets = 0usize;
    loop {
        let page = transport.fetch(&next_query(&state))?;
        report.pages += 1;
        if react_epoch(&mut state, &page, &mut report, &mut epoch_resets, &dir)? {
            // The page was read with a cursor into a log that no longer
            // exists: discarded unfiled, and the re-read starts at once.
            unsaved = false;
            continue;
        }
        // Letters bind under the epoch their page was read under.
        filer.set_validity(state.epoch.unwrap_or(UIDVALIDITY));
        report.fetched += page.records.len();
        let before = state.clone();
        refine_floor(&mut state, page.records.first());
        let mut pending = Vec::new();
        for record in &page.records {
            filer.file_record(&mut storage, record, &mut report, &mut pending)?;
        }
        report.linked += settle_links(&storage, pending, &mut state)?;
        settle_events(mailspace, &mut report)?;
        let advanced = !page.records.is_empty() && page.next_cursor > state.since;
        if advanced {
            state.since = page.next_cursor;
        }
        if unsaved || state != before {
            state.save(&dir)?;
            unsaved = false;
        }
        let more = advanced && page.has_more.unwrap_or(true);
        progress(&PageProgress {
            page: report.pages,
            records: page.records.len(),
            cursor: state.since,
            more,
        });
        if !more {
            break;
        }
    }
    report.cursor = state.since;
    Ok(report)
}

/// The state a pull starts from. A resync replaces the cursor and floor, so
/// it also replaces an unreadable file instead of failing on it.
fn load_state(
    dir: &std::path::Path,
    settings: &SyncSettings,
    plan: &PullPlan<'_>,
) -> Result<SyncState, VivariumError> {
    if plan.resync {
        Ok(SyncState::load_for_resync(dir, &settings.project))
    } else {
        SyncState::load(dir, &settings.project)
    }
}

/// Apply a plan to the state before the first request: refuse a window on a
/// mailspace that already has a cursor, reset for a resync, and give a first
/// plain pull the floor "full history".
fn apply_plan(
    state: &mut SyncState,
    plan: &PullPlan<'_>,
    settings: &SyncSettings,
) -> Result<(), VivariumError> {
    if plan.resync {
        state.since = 0;
        state.floor = Some(Floor::full());
        return Ok(());
    }
    if let Some(window) = plan.window {
        if state.since > 0 {
            return Err(VivariumError::Message(format!(
                "this mailspace already pulled project {} ({}, cursor {}); --since only starts a \
                 mailspace that has no cursor. Run `vivi sync resync` to replay everything",
                settings.project,
                state.history_line(),
                state.since
            )));
        }
        // The outcome of an empty window; the first record refines it.
        state.floor = Some(Floor {
            sequence: 0,
            time: Some(window.to_string()),
        });
    } else if state.since == 0 && state.floor.is_none() {
        state.floor = Some(Floor::full());
    }
    Ok(())
}

/// The next list request: records after the cursor, or after the window
/// floor's time while the cursor is still zero.
fn next_query(state: &SyncState) -> ListQuery<'_> {
    let after = (state.since == 0)
        .then(|| state.floor.as_ref().and_then(|floor| floor.time.as_deref()))
        .flatten();
    ListQuery {
        since: state.since,
        limit: PAGE_LIMIT,
        after,
    }
}

/// The most epoch resets one pull tolerates: a server that changes its
/// epoch again and again has no stable log to read from.
const MAX_EPOCH_RESETS: usize = 2;

/// The error a pull gives up with when the epoch keeps changing.
fn unstable_epoch(changes: usize) -> VivariumError {
    VivariumError::Message(format!(
        "server epoch changed {changes} times during one pull; the server log is unstable; try \
         again later"
    ))
}

/// The warning an unusable epoch adds once per pull.
const UNUSABLE_EPOCH: &str = "server sent an unusable epoch; ignoring it";

/// React to the page's `epoch`, per the wire contract on [`Page::epoch`].
/// First sight records the epoch with no reset. A changed epoch means the
/// page was read with a cursor into a log that no longer exists: the page is
/// discarded unfiled, the cursor and floor reset to a full re-read, and the
/// state saved at once, so an interrupted pull resumes from sequence 0. An
/// unusable epoch value warns once and changes nothing. A third change in
/// one pull is an error: the server log is unstable. The count so far
/// arrives in `resets` and leaves bumped when a reset was handled.
///
/// Returns whether the pull must skip the page and start over.
fn react_epoch(
    state: &mut SyncState,
    page: &Page,
    report: &mut PullReport,
    resets: &mut usize,
    dir: &std::path::Path,
) -> Result<bool, VivariumError> {
    if page.epoch_unusable && !report.warnings.iter().any(|w| w == UNUSABLE_EPOCH) {
        report.warnings.push(UNUSABLE_EPOCH.into());
    }
    let Some(new) = page.epoch else {
        return Ok(false);
    };
    let Some(old) = state.epoch else {
        // First sight: the checkout learns the epoch, the cursor stays.
        state.epoch = Some(new);
        return Ok(false);
    };
    if old == new {
        return Ok(false);
    }
    if state
        .floor
        .as_ref()
        .is_some_and(|floor| floor.time.is_some() && !floor.is_full())
    {
        report.warnings.push(format!(
            "server epoch changed ({old} -> {new}): the --since window floor was dropped; \
             the re-read covers full history"
        ));
    }
    report.epoch_reset = Some((Some(old), new));
    state.since = 0;
    state.floor = Some(Floor::full());
    state.epoch = Some(new);
    state.save(dir)?;
    *resets += 1;
    if *resets > MAX_EPOCH_RESETS {
        // The state stays as saved: cursor 0 and the latest epoch.
        return Err(unstable_epoch(*resets));
    }
    Ok(true)
}

/// A window's floor is its first record: its sequence and `appended_at`.
/// Applies while the cursor is zero and the floor is a bare requested time.
fn refine_floor(state: &mut SyncState, first: Option<&Record>) {
    let Some(first) = first else {
        return;
    };
    let Some(floor) = &state.floor else {
        return;
    };
    if state.since != 0 || floor.sequence != 0 || floor.time.is_none() {
        return;
    }
    state.floor = Some(Floor {
        sequence: first.sequence,
        time: first.appended_at.clone().or_else(|| floor.time.clone()),
    });
}

/// Apply the item-state events this page brought, and the events held for
/// the work items it brought, now that the page's rows and reply links are
/// all there. Idempotent: an item already at its latest event changes
/// nothing.
pub(crate) fn settle_events(
    mailspace: &Mailspace,
    report: &mut PullReport,
) -> Result<(), VivariumError> {
    let touched = std::mem::take(&mut report.touched);
    let tally = mailspace.apply_arrived_events(&touched)?;
    report.events_applied += tally.applied;
    report.events_ignored += tally.ignored;
    Ok(())
}

/// Try every reply this page produced, and every reply still waiting from
/// earlier pulls, against the rows now held. A reply whose parent is still
/// absent stays in the state for the next pull.
pub(crate) fn settle_links(
    storage: &Storage,
    pending: Vec<(String, String)>,
    state: &mut SyncState,
) -> Result<usize, VivariumError> {
    let mut work = std::mem::take(&mut state.dangling);
    work.extend(pending);
    let mut linked = 0;
    for (child, parent) in work {
        match link::link_reply(storage, &child, &parent)? {
            LinkOutcome::Linked => linked += 1,
            LinkOutcome::Settled => {}
            LinkOutcome::ParentMissing => {
                state.dangling.insert(child, parent);
            }
        }
    }
    Ok(linked)
}

#[cfg(test)]
#[path = "pull_test.rs"]
mod tests;
