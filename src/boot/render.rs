//! Text rendering for the boot digest.
//!
//! Sections are ordered by decision value rather than by data source: seats
//! first because claimed-versus-observed ownership changes what the reader
//! does, then the loops, then the paper that no longer matches reality, then
//! the backlog, then what to run next. Every section is bounded, and the digest
//! closes by naming each cap.
//!
//! Lines are built with `push_str` rather than `write!` so the renderer needs
//! no error-swallowing: appending to a `String` cannot fail.

use super::truncate::{CAP_CHARTER_LINES, CAP_PROBE_LINES};
use super::{BootReport, GoalRow, goal_line, seat_line};

/// Render the digest.
#[must_use]
pub fn to_text(report: &BootReport) -> String {
    let mut out = String::new();
    line(
        &mut out,
        format!("boot    {}  vivi {}", report.mailspace, report.vivi),
    );
    line(&mut out, format!("root    {}", report.root.display()));
    line(&mut out, format!("at      {}", report.generated_at));
    line(&mut out, String::new());
    totals(&mut out, report);
    seats(&mut out, report);
    loops(&mut out, report);
    unabsorbed(&mut out, report);
    held_unknown(&mut out, report);
    pump(&mut out, report);
    handles(&mut out, report);
    goals(&mut out, report);
    rules(&mut out, report);
    memos(&mut out, report);
    charters(&mut out, report);
    triage(&mut out, report);
    probes(&mut out, report);
    truncations(&mut out, report);
    out
}

fn line(out: &mut String, text: impl AsRef<str>) {
    out.push_str(text.as_ref());
    out.push('\n');
}

fn heading(out: &mut String, title: &str) {
    let rule = "-".repeat(56_usize.saturating_sub(title.len()));
    line(out, String::new());
    line(out, format!("-- {title} {rule}"));
}

fn totals(out: &mut String, report: &BootReport) {
    let t = &report.totals;
    line(
        out,
        format!(
            "totals  {} roles | open {} task {} need {} want | stale {} blocked {}",
            t.roles, t.open_tasks, t.open_needs, t.open_wants, t.handles_stale, t.handles_blocked
        ),
    );
    line(
        out,
        format!(
            "seats   live {} | unverified {} (subagent) | unbound {} | gone {}",
            t.seats_live, t.seats_unverified, t.seats_unbound, t.seats_gone
        ),
    );
    line(
        out,
        format!(
            "board   unabsorbed {} | goals {} ({} claim mismatch) | frontier {} | loops overdue {}",
            t.unabsorbed_mail,
            t.goals_registered,
            t.goals_claim_mismatch,
            t.frontier_remaining,
            t.loops_action_required
        ),
    );
}

fn seats(out: &mut String, report: &BootReport) {
    heading(out, "seats: roles with open work or a bound process");
    if report.seats.is_empty() {
        line(out, "  (none)");
    }
    for row in &report.seats {
        line(out, seat_line(row));
    }
    let routine = report.totals.roles.saturating_sub(report.seats.len());
    if routine > 0 {
        line(
            out,
            format!("  … {routine} more roles: no open work, no bound process"),
        );
    }
}

fn loops(out: &mut String, report: &BootReport) {
    if report.loops.is_empty() {
        return;
    }
    heading(out, "loops: declared cadence vs last outbound signal");
    for row in &report.loops {
        let age = row.age.as_deref().unwrap_or("never");
        let action = if row.action_required { "ACTION" } else { "" };
        line(
            out,
            format!(
                "  {:<16} {:<8} {:<8} {:<6} {action}",
                row.role, row.cadence, row.state, age
            ),
        );
    }
}

fn unabsorbed(out: &mut String, report: &BootReport) {
    if report.unabsorbed.is_empty() {
        return;
    }
    heading(out, "unabsorbed mail: no reader has absorbed these");
    for row in &report.unabsorbed {
        line(
            out,
            format!(
                "  {}  {:<4} {:<24} {}",
                row.handle, row.age, row.from, row.subject
            ),
        );
    }
}

fn held_unknown(out: &mut String, report: &BootReport) {
    if report.held_unknown.is_empty() {
        return;
    }
    heading(out, "held for unknown roles: no seat here has these names");
    for row in &report.held_unknown {
        let noun = if row.letters == 1 {
            "letter"
        } else {
            "letters"
        };
        line(
            out,
            format!(
                "  {:<16} {} {noun}  (vivi role add {} adopts them; vivi mail list --to {} \
                 shows them)",
                row.role, row.letters, row.role, row.role
            ),
        );
    }
}

fn pump(out: &mut String, report: &BootReport) {
    let Some(pump) = &report.pump else {
        return;
    };
    heading(out, "pump: local execution state");
    let units = if pump.units.is_empty() {
        String::from("none")
    } else {
        pump.units
            .iter()
            .map(|(status, count)| format!("{status}={count}"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    line(out, format!("  units    {units}"));
    line(
        out,
        format!(
            "  complete {} | complete-but-open {} | amendments {} | stuck {} | exceptions {}",
            pump.complete, pump.complete_but_open, pump.amendments, pump.stuck, pump.exceptions
        ),
    );
}

fn handles(out: &mut String, report: &BootReport) {
    if report.handles.is_empty() {
        return;
    }
    heading(out, "handles: open work with a verdict");
    for row in &report.handles {
        let detail = row
            .detail
            .as_deref()
            .map(|detail| format!("  ({detail})"))
            .unwrap_or_default();
        line(
            out,
            format!(
                "  {}  {:<5} {:<4} {:<10} {:<22} {}{detail}",
                row.handle,
                row.kind,
                row.age,
                row.verdict.slug(),
                row.from,
                row.subject
            ),
        );
    }
}

fn goals(out: &mut String, report: &BootReport) {
    if report.goals.is_empty() {
        return;
    }
    heading(
        out,
        "goals: registered documents, register = completion contract",
    );
    for row in &report.goals {
        line(out, goal_line(row));
        if let Some(status) = &row.status_text {
            line(out, format!("      status: {status}"));
        }
        if let Some(register) = &row.register {
            line(out, format!("      register: {register}"));
        }
        if let Some(mismatch) = &row.claim_mismatch {
            line(out, format!("      MISMATCH: {mismatch}"));
        }
        if !row.frontier.is_empty() {
            line(out, format!("      frontier: {}", frontier_line(row)));
        }
    }
}

fn frontier_line(row: &GoalRow) -> String {
    row.frontier
        .iter()
        .map(|unit| format!("{} {}", unit.unit, unit.status))
        .collect::<Vec<_>>()
        .join(" | ")
}

fn rules(out: &mut String, report: &BootReport) {
    if report.rules.is_empty() {
        return;
    }
    heading(out, "rules: project rules");
    for row in &report.rules {
        line(
            out,
            format!(
                "  {}  {:<4} {:<7} {}",
                row.handle, row.age, row.scope, row.subject
            ),
        );
    }
}

fn memos(out: &mut String, report: &BootReport) {
    if report.memos.is_empty() {
        return;
    }
    heading(out, "memos: durable role memory");
    for row in &report.memos {
        let mark = if row.superseded { "superseded" } else { "" };
        line(
            out,
            format!(
                "  {}  {:<4} {:<10} {}",
                row.handle, row.age, mark, row.subject
            ),
        );
    }
}

fn charters(out: &mut String, report: &BootReport) {
    if report.charters.is_empty() {
        return;
    }
    heading(out, "charters: roles holding open work");
    for row in &report.charters {
        line(out, format!("  {}", row.role));
        for text in row.lines.iter().take(CAP_CHARTER_LINES) {
            line(out, format!("    {text}"));
        }
    }
}

fn triage(out: &mut String, report: &BootReport) {
    if report.triage.is_empty() {
        return;
    }
    heading(out, "triage: backlog sliced for one seat each");
    for slice in &report.triage {
        line(
            out,
            format!(
                "  slice {}  {} handles: {}",
                slice.index,
                slice.size,
                slice.handles.join(" ")
            ),
        );
        for subject in &slice.subjects {
            line(out, format!("      {subject}"));
        }
    }
}

fn probes(out: &mut String, report: &BootReport) {
    for fact in &report.probe_facts {
        line(out, format!("probe   {fact}"));
    }
    for section in &report.probe_sections {
        heading(out, &format!("probe: {}", section.title));
        for text in section.lines.iter().take(CAP_PROBE_LINES) {
            line(out, format!("  {text}"));
        }
        if section.lines.len() > CAP_PROBE_LINES {
            line(
                out,
                format!("  … {} more lines", section.lines.len() - CAP_PROBE_LINES),
            );
        }
    }
    if !report.probes_skipped.is_empty() {
        heading(out, "probes skipped");
        for skip in &report.probes_skipped {
            line(out, format!("  {skip}"));
        }
    }
}

fn truncations(out: &mut String, report: &BootReport) {
    if report.truncations.is_empty() {
        return;
    }
    heading(out, "truncated: every cap that bit");
    for row in &report.truncations {
        line(
            out,
            format!("  {}: showing {} of {}", row.section, row.shown, row.total),
        );
    }
}
