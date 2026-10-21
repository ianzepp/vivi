use std::fs;
use std::io::ErrorKind;

use chrono::{DateTime, Utc};
use vivi::VivariumError;
use vivi::cli::BoardCommand;
use vivi::mailspace::board::{
    Board, BoardGraph, BoardItem, BoardOptions, BoardPump, IdentityBoard,
};
use vivi::mailspace::{GoalView, Mailspace};
use vivi::role_schedule::{ScheduleReport, ScheduleState, state_label as schedule_state_label};

pub(crate) fn handle_board_command(command: &BoardCommand) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(command.project.as_deref())?;
    let board = mailspace.board(&BoardOptions {
        for_identity: command.for_identity.clone(),
        wants_cap: command.wants,
        since: resolve_since(command)?,
        with_process: command.process,
        with_graph: command.graph,
        with_pump: command.pump,
    })?;
    if command.json {
        print_json(&board)?;
    } else {
        print_board(&board);
    }
    write_watermark(command)?;
    Ok(())
}

fn resolve_since(command: &BoardCommand) -> Result<Option<DateTime<Utc>>, VivariumError> {
    if let Some(since) = &command.since {
        return vivi::mailspace::parse_time_bound(since).map(Some);
    }
    let Some(path) = &command.watermark_file else {
        return Ok(None);
    };
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let value = raw.trim();
    if value.is_empty() {
        Ok(None)
    } else {
        vivi::mailspace::parse_time_bound(value).map(Some)
    }
}

fn write_watermark(command: &BoardCommand) -> Result<(), VivariumError> {
    if !command.write_watermark {
        return Ok(());
    }
    let Some(path) = &command.watermark_file else {
        return Ok(());
    };
    fs::write(path, Utc::now().to_rfc3339()).map_err(Into::into)
}

fn print_json(board: &Board) -> Result<(), VivariumError> {
    println!(
        "{}",
        serde_json::to_string_pretty(board)
            .map_err(|e| VivariumError::Other(format!("failed to encode JSON: {e}")))?
    );
    Ok(())
}

fn print_board(board: &Board) {
    println!("mailspace {}", board.name);
    println!("root      {}", board.root.display());
    println!(
        "actionable open: {}  tasks: {}  needs: {}  wants: {}",
        board.totals.actionable_open,
        board.totals.tasks_open,
        board.totals.needs_open,
        board.totals.wants_open
    );
    if let Some(pump) = &board.pump {
        print_board_pump(pump);
    }
    print_board_goals(&board.goals);
    for identity in &board.identities {
        print_identity(identity);
    }
    if let Some(graphs) = &board.graphs {
        print_board_graphs(graphs);
    }
}

fn print_board_pump(pump: &BoardPump) {
    println!();
    println!("pump (local):");
    let units = if pump.units.is_empty() {
        String::from("none")
    } else {
        pump.units
            .iter()
            .map(|(status, count)| format!("{status}={count}"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    println!("  units    {units}");
    println!(
        "  complete {}  complete-but-open {}  amendments {}  stuck {}  exceptions {}",
        pump.complete, pump.complete_but_open, pump.amendments, pump.stuck, pump.exceptions
    );
}

fn print_board_goals(goals: &[GoalView]) {
    if goals.is_empty() {
        return;
    }
    println!();
    println!("goals:");
    for goal in goals {
        let label = goal
            .label
            .as_deref()
            .map(|l| format!("  {l}"))
            .unwrap_or_default();
        let missing = if goal.exists { "" } else { "  MISSING" };
        println!("  {}  {}{label}{missing}", goal.handle, goal.path);
    }
}

fn print_board_graphs(graphs: &[BoardGraph]) {
    println!();
    println!("graphs:");
    if graphs.is_empty() {
        println!("  (none)");
        return;
    }
    for graph in graphs {
        println!(
            "  {}  handle={}  rev={}  ready={}  blocked={}  active={}",
            graph.code,
            graph.handle,
            graph.revision,
            graph.ready.len(),
            graph.blocked.len(),
            graph.active.len()
        );
        for node in &graph.ready {
            println!(
                "    ready  {}  {}  [{}]",
                node.source_id, node.handle, node.state
            );
        }
        for node in &graph.blocked {
            println!(
                "    blocked {}  {}  by={}",
                node.source_id,
                node.handle,
                node.blocked_by.join(",")
            );
        }
    }
}

fn print_identity(identity: &IdentityBoard) {
    println!();
    println!(
        "{} <{}>  actionable:{}  tasks:{}  needs:{}  wants:{}",
        identity.identity,
        identity.address,
        identity.actionable_open,
        identity.tasks.len(),
        identity.needs.len(),
        identity.wants.len() + identity.wants_hidden
    );
    print_capacity_line(identity);
    if let Some(process) = &identity.process {
        let running = match process.running {
            Some(true) => "yes",
            Some(false) => "no",
            None => "unknown",
        };
        let name = process
            .name
            .as_deref()
            .map(|name| format!("  ({name})"))
            .unwrap_or_default();
        println!(
            "  process: {} running:{}{name}",
            role_status_state_label(&process.state),
            running
        );
    }
    print_schedule_line(&identity.schedule, identity.last_auto_run.as_deref());
    print_items("tasks", &identity.tasks);
    print_items("needs", &identity.needs);
    print_items("wants", &identity.wants);
    if identity.wants_hidden > 0 {
        println!("  wants hidden by cap: {}", identity.wants_hidden);
    }
}

fn print_capacity_line(identity: &IdentityBoard) {
    if identity.model.is_none() && identity.thinking.is_none() {
        return;
    }
    print!("  capacity:");
    if let Some(model) = &identity.model {
        print!(" model: {model}");
    }
    if let Some(thinking) = &identity.thinking {
        print!("  thinking: {thinking}");
    }
    println!();
}

fn role_status_state_label(state: &vivi::role_status::ProcessState) -> &'static str {
    use vivi::role_status::ProcessState;
    match state {
        ProcessState::NotSet => "not_set",
        ProcessState::Remote => "remote",
        ProcessState::Alive => "alive",
        ProcessState::Zombie => "zombie",
        ProcessState::Dead => "dead",
        ProcessState::Subagent => "subagent",
        ProcessState::Unknown => "unknown",
    }
}

fn print_schedule_line(schedule: &ScheduleReport, last_auto_run: Option<&str>) {
    if matches!(schedule.state, ScheduleState::None) {
        return;
    }
    print!("  schedule: {}", schedule_state_label(schedule.state));
    if let Some(cadence) = &schedule.cadence {
        print!("  cadence {cadence}");
    }
    if let Some(next_due) = &schedule.next_due_at {
        print!("  next due {next_due}");
    }
    match schedule.state {
        ScheduleState::Never => print!("  (no outbound signal)"),
        _ => {
            if let Some(age) = schedule.age_seconds {
                print!("  last signal {age}s ago");
            }
        }
    }
    if let Some(handle) = &schedule.last_signal_handle {
        print!("  {handle}");
    }
    if let Some(last_auto_run) = last_auto_run {
        print!("  last auto-run {last_auto_run}");
    }
    if schedule.action_required {
        print!("  ACTION REQUIRED");
    }
    println!();
}

fn print_items(label: &str, items: &[BoardItem]) {
    if items.is_empty() {
        return;
    }
    println!("{label}:");
    for item in items {
        println!(
            "  {}  {}  {}  {}",
            item.handle, item.date, item.from, item.subject
        );
    }
}
