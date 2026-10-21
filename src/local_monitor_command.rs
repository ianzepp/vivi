use std::time::{SystemTime, UNIX_EPOCH};

use vivi::VivariumError;
use vivi::cli::MonitorCommand;
use vivi::monitor::model::{MonitorRow, RowState};
use vivi::monitor::{merge_roots, snapshot};

pub(crate) fn run(command: &MonitorCommand) -> Result<(), VivariumError> {
    if command.json && !command.once {
        return Err(VivariumError::Message(String::from("--json needs --once")));
    }
    let (registered, mut warnings) = match vivi::registry::load() {
        Ok(registry) => (registry.roots(), Vec::new()),
        Err(error) => (
            Default::default(),
            vec![format!("pump service registry: {error}")],
        ),
    };
    let roots = merge_roots(registered, &command.projects);
    if command.once {
        let result = snapshot(&roots, epoch_secs());
        warnings.extend(result.warnings);
        if command.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&result.rows).map_err(|error| {
                    VivariumError::Other(format!("failed to encode monitor JSON: {error}"))
                })?
            );
        } else {
            print_rows(&result.rows);
        }
        for warning in warnings {
            eprintln!("warning: {warning}");
        }
        return Ok(());
    }
    for warning in warnings {
        eprintln!("warning: {warning}");
    }
    vivi::monitor::tui::run(&roots)
}

fn print_rows(rows: &[MonitorRow]) {
    println!(
        "PROJECT       POOL     SLOT            PROVIDER     ROLE       TASK / SUBJECT          MODEL                STATE                 START   RUN / BUDGET  RO"
    );
    for row in rows {
        println!(
            "{:<13} {:<8} {:<15} {:<12} {:<10} {:<23} {:<20} {:<21} {:<7} {:<13} {}",
            row.project,
            row.pool,
            row.slot,
            provider(row),
            row.role,
            task(row),
            row.model,
            state(&row.state),
            vivi::monitor::model::started_text(row.started_at),
            vivi::monitor::model::running_text(row),
            row.read_only
                .map_or("-", |read_only| if read_only { "ro" } else { "rw" }),
        );
    }
}

fn provider(row: &MonitorRow) -> String {
    if row.remote {
        format!("{} remote", row.provider)
    } else {
        row.provider.clone()
    }
}

fn task(row: &MonitorRow) -> String {
    match (&row.task, &row.subject) {
        (Some(handle), Some(subject)) => format!("{handle} {subject}"),
        (Some(handle), None) => handle.clone(),
        (None, Some(subject)) => subject.clone(),
        (None, None) => String::new(),
    }
}

fn state(state: &RowState) -> String {
    match state {
        RowState::Running => String::from("running"),
        RowState::Finished(outcome) => outcome.clone(),
    }
}

fn epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}
