//! `vivi boot`: the project frame in one read.
//!
//! Boot answers the questions a Mind asks when it gains or regains head
//! context: what is live, what is unabsorbed, what is claimed but already
//! landed, what is claimed and still open, what is blocked, and what should run
//! next. It is read-only, stateless, and idempotent, so every run is equally
//! authoritative and two runs are comparable.
//!
//! The digest is sectioned by decision, bounded per section, and closed by a
//! truncation manifest naming every cap that bit. Vivi contributes the facts it
//! owns: the board, mail, memos, roles, goals, and the handle inventory. Facts
//! it cannot own — git ancestry, lane state, the live seat count of a subagent
//! harness — arrive through [probes](probe) the project declares, so the same
//! command serves every project and Vivi never learns what a repository is.

use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::error::VivariumError;
use crate::mailspace::{
    DumpFilters, MailAbsorbFilter, MailDumpRequest, Mailspace, RoleView, TaskDumpRequest,
    TaskDumpStatus,
};
use crate::role_schedule::{self, ScheduleState};
use crate::role_status::{self, ProcessState};
use crate::storage::{Storage, StoredMessageView};

pub mod format;
pub mod goals;
pub mod handles;
pub mod probe;
pub mod pump;
pub mod pump_slots;
pub mod render;
pub mod truncate;

use format::{age_label, one_line};
use goals::GoalRead;
use handles::{HandleRow, TriageSlice, Verdict};
use probe::{ProbeOutput, ProbeRun, ProbeSection};
pub use pump::{BootOptions, PumpBoot, PumpSnapshot, PumpView};
pub use truncate::Truncation;
use truncate::{
    CAP_CHARTER_LINES, CAP_CHARTERS, CAP_GOALS, CAP_LOOPS, CAP_MEMOS, CAP_RULES, CAP_SEATS,
    CAP_UNABSORBED, CAP_UNKNOWN_ROLES,
};

/// One role's seat binding against its observed process state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SeatRow {
    pub role: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    pub binding: String,
    pub verdict: Verdict,
}

/// One role that declares a cadence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LoopRow {
    pub role: String,
    pub cadence: String,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age: Option<String>,
    pub action_required: bool,
}

/// One unabsorbed inbox record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MailRow {
    pub handle: String,
    pub from: String,
    pub subject: String,
    pub age: String,
}

/// A role name letters are held under that this mailspace does not have.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnknownRoleRow {
    pub role: String,
    pub letters: usize,
}

/// One registered goal document with its derived completion evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GoalRow {
    pub handle: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bucket: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub register: Option<String>,
    /// Present only when the Status line's completion claim disagrees with the
    /// register's own done count.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claim_mismatch: Option<String>,
    pub frontier: Vec<goals::FrontierUnit>,
}

/// One active project rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuleRow {
    pub handle: String,
    pub age: String,
    pub scope: String,
    pub subject: String,
}

/// One durable role memo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MemoRow {
    pub handle: String,
    pub age: String,
    pub subject: String,
    pub superseded: bool,
}

/// The opening lines of one role charter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CharterHead {
    pub role: String,
    pub lines: Vec<String>,
}

/// Counts that answer "how big is this" without scrolling the sections.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct BootTotals {
    pub roles: usize,
    pub seats_live: usize,
    pub seats_unverified: usize,
    pub seats_unbound: usize,
    pub seats_gone: usize,
    pub open_tasks: usize,
    pub open_needs: usize,
    pub open_wants: usize,
    pub handles_stale: usize,
    pub handles_blocked: usize,
    pub unabsorbed_mail: usize,
    /// Letters held for role names this mailspace does not have.
    pub held_unknown_role: usize,
    pub goals_registered: usize,
    pub goals_claim_mismatch: usize,
    pub frontier_remaining: usize,
    pub loops_action_required: usize,
}

/// The full boot digest.
#[derive(Debug, Clone, Serialize)]
pub struct BootReport {
    pub mailspace: String,
    pub root: PathBuf,
    pub vivi: String,
    pub generated_at: String,
    pub totals: BootTotals,
    /// Seat bindings against observed state. Read this first.
    pub seats: Vec<SeatRow>,
    /// Declared cadences and their silence.
    pub loops: Vec<LoopRow>,
    /// Inbox records no reader has absorbed.
    pub unabsorbed: Vec<MailRow>,
    /// Role names letters wait under that no seat here carries.
    pub held_unknown: Vec<UnknownRoleRow>,
    /// Local pump execution state, when `pump.db` or configured pools exist.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pump: Option<PumpBoot>,
    /// Open tasks, needs, and wants with verdicts.
    pub handles: Vec<HandleRow>,
    /// Registered goal documents with register tallies.
    pub goals: Vec<GoalRow>,
    /// Active project rules, shared by every seat.
    pub rules: Vec<RuleRow>,
    /// Durable role memos.
    pub memos: Vec<MemoRow>,
    /// Opening lines of the charters for roles holding open work.
    pub charters: Vec<CharterHead>,
    /// The backlog sliced into seat-sized groups.
    pub triage: Vec<TriageSlice>,
    /// Flat facts contributed by probes.
    pub probe_facts: Vec<String>,
    /// Named blocks contributed by probes.
    pub probe_sections: Vec<ProbeSection>,
    /// Probes that contributed nothing, with the reason.
    pub probes_skipped: Vec<String>,
    /// Every cap that bit, so the reader knows the digest is complete in
    /// coverage and bounded in length.
    pub truncations: Vec<Truncation>,
}

impl BootReport {
    /// Collect the digest for one mailspace.
    ///
    /// # Errors
    /// Returns an error when the mailspace store cannot be opened. A declared
    /// probe that is missing, fails, or prints unparseable JSON is recorded as
    /// skipped and never fails the run.
    pub fn collect(mailspace: &Mailspace) -> Result<Self, VivariumError> {
        Self::collect_with(mailspace, &BootOptions::default())
    }

    /// Collect the digest, gating the pump snapshot with `opts`.
    ///
    /// # Errors
    /// Returns an error when the mailspace store cannot be opened or `--role`
    /// names an unknown identity. A declared probe that is missing, fails, or
    /// prints unparseable JSON is recorded as skipped and never fails the run.
    pub fn collect_with(mailspace: &Mailspace, opts: &BootOptions) -> Result<Self, VivariumError> {
        let now = Utc::now();
        let storage = mailspace.storage()?;
        let views = mailspace.list_role_views()?;
        let mut truncations = Vec::new();

        let (mut rows, blocked) = collect_handles(mailspace, now)?;
        let runs = probe::run_all(&mailspace.root, &mailspace.config.probes);
        handles::apply_verdicts(&mut rows, &probe_verdicts(&runs), &blocked);
        let open_needs: BTreeSet<String> = rows
            .iter()
            .filter(|row| row.kind == "need")
            .map(|row| row.handle.clone())
            .collect();
        let pump = pump::collect(mailspace, &open_needs, opts, now, &mut truncations)?;
        let handles = cap_handles(rows, &mut truncations);
        let (all_seats, seats) = collect_seats(&views, &handles, &mut truncations);
        let loops = collect_loops(mailspace, &storage, &views, &mut truncations);
        let unabsorbed = collect_unabsorbed(mailspace, now, &mut truncations)?;
        let goals = collect_goals(mailspace, &mut truncations);
        let rules = collect_rule_rows(mailspace, now, &mut truncations)?;
        let memos = collect_memos(mailspace, now, &mut truncations)?;
        let charters = collect_charters(&views, &handles);
        let triage = handles::triage_slices(&handles);
        let (held_unknown, held_letters) = collect_held_unknown(mailspace, &mut truncations)?;
        let totals = totals(
            &all_seats,
            &loops,
            &unabsorbed,
            &handles,
            &goals,
            held_letters,
        );

        Ok(Self {
            mailspace: mailspace.config.name.clone(),
            root: mailspace.root.clone(),
            vivi: env!("CARGO_PKG_VERSION").to_string(),
            generated_at: now.to_rfc3339(),
            totals,
            seats,
            loops,
            unabsorbed,
            held_unknown,
            pump,
            handles,
            goals,
            rules,
            memos,
            charters,
            triage,
            probe_facts: probe_facts(&runs),
            probe_sections: probe_sections(&runs),
            probes_skipped: probe_skips(&runs),
            truncations,
        })
    }

    /// Render the digest as text.
    #[must_use]
    pub fn render(&self) -> String {
        render::to_text(self)
    }
}

/// The pump state line: durable control mode, writer process, and how old
/// the status snapshot is.
#[must_use]
pub fn pump_state_line(control: &str, writer: &str, age: Option<u64>) -> String {
    let age = match age {
        Some(seconds) => format!("{seconds}s"),
        None => String::from("never"),
    };
    format!("  state   control {control}, writer {writer}, status age {age}")
}

fn probe_verdicts(runs: &[ProbeRun]) -> Vec<probe::ProbeVerdict> {
    outputs(runs)
        .flat_map(|output| output.verdicts.clone())
        .collect()
}

fn probe_facts(runs: &[ProbeRun]) -> Vec<String> {
    outputs(runs)
        .flat_map(|output| output.facts.clone())
        .collect()
}

fn probe_sections(runs: &[ProbeRun]) -> Vec<ProbeSection> {
    outputs(runs)
        .flat_map(|output| output.sections.clone())
        .collect()
}

fn probe_skips(runs: &[ProbeRun]) -> Vec<String> {
    runs.iter()
        .filter_map(|run| {
            run.skipped
                .as_ref()
                .map(|reason| format!("{}: {reason}", run.name))
        })
        .collect()
}

fn outputs(runs: &[ProbeRun]) -> impl Iterator<Item = &ProbeOutput> {
    runs.iter().filter_map(|run| run.output.as_ref())
}

/// Seats the reader must act on: a bound process the OS can classify, or an
/// open handle addressed to the role. The rest of the roster is routine — a
/// subagent-harness seat with no work and no probeable process — and the totals
/// block already states how many there are. Printing all 137 buried the frame.
///
/// Returns every seat row for the totals, and the subset worth printing.
fn collect_seats(
    views: &[RoleView],
    handles: &[HandleRow],
    truncations: &mut Vec<Truncation>,
) -> (Vec<SeatRow>, Vec<SeatRow>) {
    let all: Vec<SeatRow> = views.iter().map(seat_row).collect();
    let mut shown: Vec<SeatRow> = views
        .iter()
        .zip(&all)
        .filter(|(view, row)| seat_needs_attention(view, row, handles))
        .map(|(_, row)| row.clone())
        .collect();
    shown.truncate(CAP_SEATS);
    truncate::record(truncations, "seats: routine roster", shown.len(), all.len());
    (all, shown)
}

/// A seat is routine when its harness is a subagent or it is unbound, and no
/// open handle addresses it.
fn seat_needs_attention(view: &RoleView, row: &SeatRow, handles: &[HandleRow]) -> bool {
    !matches!(row.verdict, Verdict::Unverified | Verdict::Unbound)
        || handles.iter().any(|handle| addresses_role(handle, view))
}

fn addresses_role(row: &HandleRow, view: &RoleView) -> bool {
    row.to.contains(&view.address) || row.to.contains(&format!("{}@", view.name))
}

fn seat_row(view: &RoleView) -> SeatRow {
    let report = role_status::probe_quick(view.pid, view.host.as_deref(), view.harness.as_deref());
    SeatRow {
        role: view.name.clone(),
        kind: view.kind.clone().unwrap_or_else(|| "role".to_string()),
        harness: view.harness.clone(),
        binding: binding_label(view),
        verdict: seat_verdict(&report.state),
    }
}

fn binding_label(view: &RoleView) -> String {
    if view.harness.as_deref() == Some(crate::mailspace::ROLE_HARNESS_SUBAGENT) {
        return match view.pid {
            Some(pid) => format!("subagent harness, pid {pid} unprobeable"),
            None => "subagent harness, unbound".to_string(),
        };
    }
    match (view.pid, view.host.as_deref()) {
        (Some(pid), Some(host)) => format!("pid {pid} @ {host}"),
        (Some(pid), None) => format!("pid {pid}"),
        (None, _) => "unbound".to_string(),
    }
}

fn seat_verdict(state: &ProcessState) -> Verdict {
    match state {
        ProcessState::Alive => Verdict::Live,
        ProcessState::Subagent => Verdict::Unverified,
        ProcessState::NotSet => Verdict::Unbound,
        ProcessState::Dead => Verdict::Dead,
        ProcessState::Zombie => Verdict::Zombie,
        ProcessState::Remote => Verdict::Remote,
        ProcessState::Unknown => Verdict::Unknown,
    }
}

fn collect_loops(
    mailspace: &Mailspace,
    storage: &Storage,
    views: &[RoleView],
    truncations: &mut Vec<Truncation>,
) -> Vec<LoopRow> {
    let mut rows: Vec<LoopRow> = views
        .iter()
        .filter(|view| view.cadence.is_some())
        .filter_map(|view| loop_row(mailspace, storage, view))
        .collect();
    rows.sort_by(|a, b| b.action_required.cmp(&a.action_required));
    truncate::capped(rows, CAP_LOOPS, "loops", truncations)
}

fn loop_row(mailspace: &Mailspace, storage: &Storage, view: &RoleView) -> Option<LoopRow> {
    let Ok(report) = mailspace.schedule_report_with(storage, &view.name) else {
        return None;
    };
    if matches!(report.state, ScheduleState::None) {
        return None;
    }
    Some(LoopRow {
        role: view.name.clone(),
        cadence: view.cadence.clone().unwrap_or_default(),
        state: role_schedule::state_label(report.state).to_string(),
        age: report.age_seconds.map(human_seconds),
        action_required: report.action_required,
    })
}

fn collect_unabsorbed(
    mailspace: &Mailspace,
    now: DateTime<Utc>,
    truncations: &mut Vec<Truncation>,
) -> Result<Vec<MailRow>, VivariumError> {
    let records = mailspace.dump_mail(MailDumpRequest {
        folder: "inbox".into(),
        kind: None,
        filters: DumpFilters {
            absorb_status: MailAbsorbFilter::Unabsorbed,
            ..Default::default()
        },
    })?;
    // Mail held under another mailspace's address (other members' traffic)
    // or for a role name this mailspace does not have is nobody's here to
    // absorb.
    let records: Vec<_> = records
        .into_iter()
        .filter(|record| mailspace.is_local_account(&record.account))
        .collect();
    let rows = truncate::capped(records, CAP_UNABSORBED, "unabsorbed mail", truncations);
    Ok(rows
        .into_iter()
        .map(|record| MailRow {
            handle: record.handle,
            from: record.from,
            subject: one_line(&record.subject, 88),
            age: age_label(&record.date, now),
        })
        .collect())
}

/// The rows of role names letters are held for, and the letter total.
fn collect_held_unknown(
    mailspace: &Mailspace,
    truncations: &mut Vec<Truncation>,
) -> Result<(Vec<UnknownRoleRow>, usize), VivariumError> {
    let rows: Vec<UnknownRoleRow> = mailspace
        .held_for_unknown_roles()?
        .into_iter()
        .map(|(role, letters)| UnknownRoleRow { role, letters })
        .collect();
    let total = rows.iter().map(|row| row.letters).sum();
    let shown = truncate::capped(rows, CAP_UNKNOWN_ROLES, "unknown roles", truncations);
    Ok((shown, total))
}

fn collect_handles(
    mailspace: &Mailspace,
    now: DateTime<Utc>,
) -> Result<(Vec<HandleRow>, Vec<String>), VivariumError> {
    let mut rows = Vec::new();
    collect_open(mailspace, "tasks", "task", now, &mut rows)?;
    collect_open(mailspace, "needs", "need", now, &mut rows)?;
    collect_wants(mailspace, now, &mut rows)?;
    let blocked = blocked_handles(mailspace);
    Ok((rows, blocked))
}

fn collect_open(
    mailspace: &Mailspace,
    role: &str,
    kind: &str,
    now: DateTime<Utc>,
    rows: &mut Vec<HandleRow>,
) -> Result<(), VivariumError> {
    let records = mailspace.dump_tasks(TaskDumpRequest {
        status: TaskDumpStatus::Open,
        open_role: role.into(),
        kind: kind.into(),
        include_outbound: false,
        filters: DumpFilters::default(),
    })?;
    // Unassigned work held for another mailspace's role, and work held for
    // a role name this mailspace does not have, is nobody's handle here.
    // Active delegated needs are added below as local work.
    let mut seen = HashSet::new();
    for record in records
        .into_iter()
        .filter(|record| mailspace.is_local_account(&record.account))
    {
        seen.insert(record.handle.clone());
        rows.push(HandleRow::new(
            &record.handle,
            kind,
            &record.date,
            &record.from,
            &record.to,
            &record.subject,
            now,
        ));
    }
    for assigned in mailspace.assigned_work_for_role(None, role, kind)? {
        if seen.insert(assigned.handle.clone()) {
            rows.push(HandleRow::new(
                &assigned.handle,
                kind,
                &assigned.date,
                &assigned.from_addr,
                &assigned.to_addr,
                &assigned.subject,
                now,
            ));
        }
    }
    Ok(())
}

fn collect_wants(
    mailspace: &Mailspace,
    now: DateTime<Utc>,
    rows: &mut Vec<HandleRow>,
) -> Result<(), VivariumError> {
    // The plain role listing carries every field the inventory needs, so boot
    // does not go through `list_wants_with_metadata` for metadata it never
    // shows.
    for want in mailspace.list_kind(None, "wants", "want")? {
        if want.absorbed_at.is_some() || !mailspace.is_local_account(&want.account) {
            continue;
        }
        rows.push(HandleRow::new(
            &want.handle,
            "want",
            &want.date,
            &want.from_addr,
            &want.to_addr,
            &want.subject,
            now,
        ));
    }
    Ok(())
}

/// Handles the backlog graph lists as blocked, by item handle.
fn blocked_handles(mailspace: &Mailspace) -> Vec<String> {
    match mailspace.graph_board_summaries() {
        Ok(graphs) => graphs
            .iter()
            .flat_map(|show| show.blocked.iter())
            .map(|node| node.source_id.clone())
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// Cap the handle rows by kind, so one large class cannot crowd out the others.
fn cap_handles(rows: Vec<HandleRow>, truncations: &mut Vec<Truncation>) -> Vec<HandleRow> {
    let mut kept = Vec::new();
    for (kind, cap) in [
        ("task", truncate::CAP_TASKS),
        ("need", truncate::CAP_NEEDS),
        ("want", truncate::CAP_WANTS),
    ] {
        let of_kind: Vec<HandleRow> = rows
            .iter()
            .filter(|row| row.kind == kind)
            .cloned()
            .collect();
        kept.extend(truncate::capped(
            of_kind,
            cap,
            &format!("open handles ({kind})"),
            truncations,
        ));
    }
    kept
}

fn collect_goals(mailspace: &Mailspace, truncations: &mut Vec<Truncation>) -> Vec<GoalRow> {
    let Ok(goals) = mailspace.goal_list() else {
        return Vec::new();
    };
    truncate::capped(goals, CAP_GOALS, "goals", truncations)
        .into_iter()
        .map(|goal| {
            let read = goal_read(mailspace, &goal.path);
            GoalRow {
                handle: goal.handle,
                path: goal.path,
                bucket: read.bucket.clone(),
                status_text: read.status_text.as_deref().map(|t| one_line(t, 120)),
                register: read.register.as_ref().map(register_summary),
                claim_mismatch: read.claim_mismatch.then(|| claim_mismatch_text(&read)),
                frontier: read.frontier.clone(),
            }
        })
        .collect()
}

fn register_summary(tally: &goals::RegisterTally) -> String {
    let counts = tally
        .counts
        .iter()
        .map(|(slug, count)| format!("{slug}={count}"))
        .collect::<Vec<_>>()
        .join(" ");
    format!("{} rows: {counts}", tally.rows)
}

fn claim_mismatch_text(read: &GoalRead) -> String {
    let claimed = read.claim.map_or(0, |(claimed, _)| claimed);
    let counted = read
        .register
        .as_ref()
        .map_or(0, |tally| tally.count_of(goals::DONE_STATUS));
    format!("Status claims {claimed}; register holds {counted} done")
}

fn goal_read(mailspace: &Mailspace, path: &str) -> GoalRead {
    match std::fs::read_to_string(mailspace.root.join(path)) {
        Ok(text) => goals::read_goal(&text),
        Err(_) => goals::read_goal(""),
    }
}

fn collect_rule_rows(
    mailspace: &Mailspace,
    now: DateTime<Utc>,
    truncations: &mut Vec<Truncation>,
) -> Result<Vec<RuleRow>, VivariumError> {
    let rules = mailspace.list_rules(None, false)?;
    let rows = rules
        .into_iter()
        .map(|rule| RuleRow {
            handle: rule.handle,
            age: age_label(&rule.date, now),
            scope: rule.scope,
            subject: one_line(&rule.subject, 72),
        })
        .collect();
    Ok(truncate::capped(rows, CAP_RULES, "rules", truncations))
}

fn collect_memos(
    mailspace: &Mailspace,
    now: DateTime<Utc>,
    truncations: &mut Vec<Truncation>,
) -> Result<Vec<MemoRow>, VivariumError> {
    let mut memos = mailspace.list_kind(None, "memos", "memo")?;
    memos.sort_by(|a, b| b.date.cmp(&a.date));
    let superseded = superseded_handles(&memos);
    let rows = truncate::capped(memos, CAP_MEMOS, "memos", truncations);
    Ok(rows
        .into_iter()
        .map(|memo| MemoRow {
            age: age_label(&memo.date, now),
            superseded: superseded.contains(&memo.handle),
            subject: one_line(&memo.subject, 88),
            handle: memo.handle,
        })
        .collect())
}

/// A memo is superseded when a newer memo names it in the subject, which is how
/// the `supersedes <handle>` convention reads.
fn superseded_handles(memos: &[StoredMessageView]) -> Vec<String> {
    memos
        .iter()
        .enumerate()
        .filter(|(index, memo)| {
            memos[..*index]
                .iter()
                .any(|newer| newer.subject.contains(&memo.handle))
        })
        .map(|(_, memo)| memo.handle.clone())
        .collect()
}

fn collect_charters(views: &[RoleView], handles: &[HandleRow]) -> Vec<CharterHead> {
    let mut rows: Vec<CharterHead> = views
        .iter()
        .filter(|view| view.has_charter)
        .filter(|view| holds_open_work(view.address.as_str(), &view.name, handles))
        .map(|view| CharterHead {
            role: view.name.clone(),
            lines: format::first_paragraph(&view.charter, CAP_CHARTER_LINES),
        })
        .collect();
    rows.sort_by(|a, b| a.role.cmp(&b.role));
    rows.truncate(CAP_CHARTERS);
    rows.retain(|head| !head.lines.is_empty());
    rows
}

/// A role holds open work when an open handle addresses it.
fn holds_open_work(address: &str, name: &str, handles: &[HandleRow]) -> bool {
    handles
        .iter()
        .any(|row| row.to.contains(address) || row.to.contains(name))
}

fn totals(
    seats: &[SeatRow],
    loops: &[LoopRow],
    unabsorbed: &[MailRow],
    handles: &[HandleRow],
    goals: &[GoalRow],
    held_unknown_role: usize,
) -> BootTotals {
    let seats_with = |verdict: Verdict| seats.iter().filter(|seat| seat.verdict == verdict).count();
    let handles_of = |kind: &str| handles.iter().filter(|row| row.kind == kind).count();
    BootTotals {
        roles: seats.len(),
        seats_live: seats_with(Verdict::Live),
        seats_unverified: seats_with(Verdict::Unverified),
        seats_unbound: seats_with(Verdict::Unbound),
        seats_gone: seats_with(Verdict::Dead) + seats_with(Verdict::Zombie),
        open_tasks: handles_of("task"),
        open_needs: handles_of("need"),
        open_wants: handles_of("want"),
        handles_stale: handles
            .iter()
            .filter(|row| row.verdict == Verdict::Stale)
            .count(),
        handles_blocked: handles
            .iter()
            .filter(|row| row.verdict == Verdict::Blocked)
            .count(),
        unabsorbed_mail: unabsorbed.len(),
        held_unknown_role,
        goals_registered: goals.len(),
        goals_claim_mismatch: goals
            .iter()
            .filter(|row| row.claim_mismatch.is_some())
            .count(),
        frontier_remaining: goals.iter().map(|row| row.frontier.len()).sum(),
        loops_action_required: loops.iter().filter(|row| row.action_required).count(),
    }
}

fn human_seconds(seconds: u64) -> String {
    if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h", seconds / 3600)
    } else {
        format!("{}d", seconds / 86_400)
    }
}

/// One seat row as a fixed-width line.
#[must_use]
pub fn seat_line(row: &SeatRow) -> String {
    format!(
        "  {:<16} {:<9} {:<32} {}",
        row.role,
        row.kind,
        row.binding,
        row.verdict.slug()
    )
}

/// A goal row's headline: handle, audit bucket, and document path.
#[must_use]
pub fn goal_line(row: &GoalRow) -> String {
    format!(
        "  {}  {:<8} {}",
        row.handle,
        row.bucket.as_deref().unwrap_or("(none)"),
        row.path
    )
}

#[cfg(test)]
#[path = "boot/boot_test.rs"]
mod tests;
