//! `vivi slot list | show | history | rebuild`: the declared slots and the jobs
//! they ran, read from the `slot_runs` index. No host is contacted.

use chrono::{DateTime, Utc};
use serde::Serialize;
use vivi_core::cli::{
    SlotHistoryCommand, SlotListCommand, SlotRebuildCommand, SlotShowCommand, SlotSubcommand,
};
use vivi_core::mailspace::{Mailspace, SlotRow, SlotRunView, SlotState};
use vivi_core::pool::work::epoch_secs;
use vivi_error::VivariumError;

pub(crate) fn handle_slot_command(command: &SlotSubcommand) -> Result<(), VivariumError> {
    match command {
        SlotSubcommand::List(c) => list(c),
        SlotSubcommand::Show(c) => show(c),
        SlotSubcommand::History(c) => history(c),
        SlotSubcommand::Rebuild(c) => rebuild(c),
    }
}

fn now() -> i64 {
    i64::try_from(epoch_secs()).unwrap_or(i64::MAX)
}

fn print_json<T: Serialize + ?Sized>(value: &T) -> Result<(), VivariumError> {
    println!(
        "{}",
        serde_json::to_string_pretty(value)
            .map_err(|e| VivariumError::Other(format!("failed to encode JSON: {e}")))?
    );
    Ok(())
}

fn list(command: &SlotListCommand) -> Result<(), VivariumError> {
    let space = Mailspace::discover(command.project.as_deref())?;
    let rows = space.slot_rows()?;
    if command.json {
        return print_json(&rows);
    }
    if rows.is_empty() {
        println!("no slots declared and no job running");
    }
    let now = now();
    for row in &rows {
        println!("{}", row_line(row, now));
    }
    Ok(())
}

/// `slot-a  busy  pools lanes  task ab12cd34  need n1  model m/x  running 95s of 1800s`.
fn row_line(row: &SlotRow, now: i64) -> String {
    let undeclared = if row.declared { "" } else { "  (not declared)" };
    let mut line = format!(
        "{}  {}  pools {}{undeclared}",
        row.slot,
        row.state.word(),
        row.pools.join(",")
    );
    if let Some(run) = &row.run {
        line.push_str(&format!(
            "  task {}  need {}  model {}  {}",
            run.task,
            run.run.need.as_deref().unwrap_or("-"),
            run.run.model,
            running_text(run, now)
        ));
    }
    line
}

/// `running 95s of 1800s`, or `running 95s` when the dispatch named no budget.
fn running_text(run: &SlotRunView, now: i64) -> String {
    let ran = (now - run.run.dispatched_at).max(0);
    match run.run.budget_s {
        Some(budget) => format!("running {ran}s of {budget}s"),
        None => format!("running {ran}s"),
    }
}

fn show(command: &SlotShowCommand) -> Result<(), VivariumError> {
    let space = Mailspace::discover(command.project.as_deref())?;
    let row = space.slot_row(&command.slot)?.ok_or_else(|| {
        VivariumError::Message(format!(
            "no slot {:?}: it is not declared and has no running job",
            command.slot
        ))
    })?;
    let last = if row.state == SlotState::Free {
        space.slot_history_any_pool(&row.slot, 1)?.pop()
    } else {
        None
    };
    if command.json {
        return print_json(&serde_json::json!({ "slot": row, "last": last }));
    }
    print_row(&row, last.as_ref());
    Ok(())
}

fn print_row(row: &SlotRow, last: Option<&SlotRunView>) {
    println!("slot {}  {}", row.slot, row.state.word());
    if row.declared {
        println!("  repository {}  host {}", row.repository, row.host);
    } else {
        println!("  not declared in the config");
    }
    println!("  pools {}", row.pools.join(","));
    if let Some(run) = &row.run {
        print_run(run, "job", Some(now()));
    } else if let Some(run) = last {
        print_run(run, "last job", None);
    }
}

fn print_run(run: &SlotRunView, label: &str, now: Option<i64>) {
    let r = &run.run;
    println!("  {label} {}  task {}", r.job, run.task);
    println!(
        "    pool {}  machine {}  model {}  need {}  kind {}",
        r.pool,
        r.machine,
        r.model,
        r.need.as_deref().unwrap_or("-"),
        r.kind.as_deref().unwrap_or("-")
    );
    println!(
        "    {}  from {}",
        if r.read_only {
            "read-only".to_string()
        } else {
            format!("branch {}", r.branch)
        },
        r.start_sha.get(..12).unwrap_or(&r.start_sha)
    );
    println!("    dispatched {}", when(r.dispatched_at));
    match now {
        Some(now) => println!("    {}", running_text(run, now)),
        None => println!("    {}", result_text(run)),
    }
}

/// The result columns of a collected run.
fn result_text(run: &SlotRunView) -> String {
    let r = &run.run;
    let secs = |value: Option<i64>| value.map_or_else(|| "-".to_string(), |v| format!("{v}s"));
    format!(
        "{} push {}  agent {}  check {}  job {}",
        r.outcome.as_deref().unwrap_or("running"),
        r.push.as_deref().unwrap_or("-"),
        secs(r.agent_s),
        secs(r.check_s),
        secs(r.job_s)
    )
}

/// An epoch time as UTC, to the second.
fn when(epoch: i64) -> String {
    DateTime::<Utc>::from_timestamp(epoch, 0).map_or_else(
        || epoch.to_string(),
        |time| time.format("%Y-%m-%d %H:%M:%SZ").to_string(),
    )
}

fn history(command: &SlotHistoryCommand) -> Result<(), VivariumError> {
    let space = Mailspace::discover(command.project.as_deref())?;
    let runs = match &command.pool {
        Some(pool) => space.slot_history(pool, &command.slot, command.limit)?,
        None => space.slot_history_any_pool(&command.slot, command.limit)?,
    };
    if command.json {
        return print_json(&runs);
    }
    if runs.is_empty() {
        println!("no jobs recorded for slot {}", command.slot);
    }
    for run in &runs {
        println!(
            "{}  {}  {}  task {}  need {}  model {}  {}",
            when(run.run.dispatched_at),
            run.run.job,
            run.run.pool,
            run.task,
            run.run.need.as_deref().unwrap_or("-"),
            run.run.model,
            result_text(run)
        );
    }
    Ok(())
}

fn rebuild(command: &SlotRebuildCommand) -> Result<(), VivariumError> {
    let space = Mailspace::discover(command.project.as_deref())?;
    let rows = space.rebuild_slot_runs()?;
    println!("rebuilt slot_runs from the pool events: {rows} job(s)");
    Ok(())
}
