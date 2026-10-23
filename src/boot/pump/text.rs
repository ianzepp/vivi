//! Text rendering for the boot pump section.

use super::{PumpBoot, PumpSnapshot, PumpUnitRow, PumpView};
use crate::boot::BootReport;

pub(in crate::boot) fn render(out: &mut String, report: &BootReport) {
    let Some(pump) = &report.pump else {
        return;
    };
    heading(out, "pump: local execution state");
    counts(out, pump);
    if let Some(snap) = &pump.snapshot {
        detail(out, pump.view, snap);
    }
}

fn counts(out: &mut String, pump: &PumpBoot) {
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
    line(
        out,
        crate::boot::pump_state_line(&pump.control, &pump.writer, pump.status_age),
    );
    let role = pump.role.as_deref().unwrap_or("-");
    line(
        out,
        format!("  view     {}  role {role}", pump.view.as_str()),
    );
}

fn detail(out: &mut String, view: PumpView, snap: &PumpSnapshot) {
    inflight(out, view, &snap.inflight);
    if view == PumpView::Own {
        return;
    }
    queue(out, snap);
    held(out, &snap.held);
    ready(out, &snap.needs_ready);
    stuck(out, &snap.stuck);
    exceptions(out, &snap.exceptions);
    lanes(out, &snap.lanes);
    pools(out, &snap.pools);
}

fn inflight(out: &mut String, view: PumpView, rows: &[PumpUnitRow]) {
    if rows.is_empty() {
        return;
    }
    let label = if view == PumpView::Own {
        "own"
    } else {
        "inflight"
    };
    for (i, row) in rows.iter().enumerate() {
        let prefix = if i == 0 { label } else { "" };
        line(out, format!("  {prefix:<9} {}", unit_line(row)));
    }
}

fn queue(out: &mut String, snap: &PumpSnapshot) {
    if snap.merge_queue.is_empty() {
        return;
    }
    for (i, row) in snap.merge_queue.iter().enumerate() {
        let prefix = if i == 0 { "queue" } else { "" };
        line(out, format!("  {prefix:<9} {}", unit_line(row)));
    }
    if let Some(oldest) = &snap.oldest_wait {
        line(out, format!("  {:<9} oldest wait {oldest}", ""));
    }
}

fn held(out: &mut String, rows: &[PumpUnitRow]) {
    for (i, row) in rows.iter().enumerate() {
        let prefix = if i == 0 { "held" } else { "" };
        line(out, format!("  {prefix:<9} {}", unit_line(row)));
    }
}

fn ready(out: &mut String, needs: &[String]) {
    if needs.is_empty() {
        return;
    }
    line(out, format!("  {:<9} {}", "needs-ready", needs.join(" ")));
}

fn stuck(out: &mut String, rows: &[PumpUnitRow]) {
    for (i, row) in rows.iter().enumerate() {
        let prefix = if i == 0 { "stuck" } else { "" };
        line(out, format!("  {prefix:<9} {}", unit_line(row)));
    }
}

fn exceptions(out: &mut String, rows: &[super::PumpExceptionRow]) {
    for (i, row) in rows.iter().enumerate() {
        let prefix = if i == 0 { "exceptions" } else { "" };
        line(
            out,
            format!("  {prefix:<11} {} {}  {}", row.kind, row.unit, row.text),
        );
    }
}

fn lanes(out: &mut String, lanes: &[String]) {
    for (i, lane) in lanes.iter().enumerate() {
        let prefix = if i == 0 { "lanes" } else { "" };
        line(out, format!("  {prefix:<9} {lane}"));
    }
}

fn pools(out: &mut String, rows: &[super::PumpPoolRow]) {
    for (i, row) in rows.iter().enumerate() {
        let prefix = if i == 0 { "pools" } else { "" };
        line(out, format!("  {prefix:<9} {}", pool_line(row)));
    }
}

fn unit_line(row: &PumpUnitRow) -> String {
    let mut parts = vec![row.unit.clone(), row.need.clone(), row.status.clone()];
    push_opt(&mut parts, row.role.as_deref());
    if let Some(slot) = row.slot.as_deref() {
        if let Some(pool) = row.pool.as_deref() {
            parts.push(format!("{pool} {slot}"));
        } else {
            parts.push(slot.to_string());
        }
    } else {
        push_opt(&mut parts, row.pool.as_deref());
    }
    push_opt(&mut parts, row.task.as_deref());
    push_opt(&mut parts, row.holder.as_deref());
    push_opt(&mut parts, row.model.as_deref());
    push_opt(&mut parts, row.branch.as_deref());
    if let Some(age) = &row.age {
        match &row.budget {
            Some(budget) => parts.push(format!("age {age} / {budget}")),
            None => parts.push(format!("age {age}")),
        }
    }
    push_opt(&mut parts, row.checkout.as_deref());
    if let Some(held) = &row.held_on {
        parts.push(format!("held_on {held}"));
    }
    push_opt(&mut parts, row.base_ref.as_deref());
    push_opt(&mut parts, row.base_sha.as_deref());
    parts.join("  ")
}

fn pool_line(row: &super::PumpPoolRow) -> String {
    let mut parts = vec![
        row.name.clone(),
        row.provider.clone(),
        format!("{}/{}", row.used, row.total),
    ];
    if row.remote {
        parts.push(String::from("remote"));
    }
    push_opt(&mut parts, row.root.as_deref());
    push_opt(&mut parts, row.worktree_of.as_deref());
    push_opt(&mut parts, row.build_dir.as_deref());
    if !row.roles.is_empty() {
        parts.push(format!("roles {}", row.roles.join(",")));
    }
    parts.join("  ")
}

fn push_opt(parts: &mut Vec<String>, value: Option<&str>) {
    if let Some(value) = value {
        parts.push(value.to_string());
    }
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
