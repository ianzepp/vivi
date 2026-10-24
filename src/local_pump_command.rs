//! `vivi pump` dispatch: `run`, `once`, control, reset, records and operator actions.

use std::path::{Path, PathBuf};

use vivi::VivariumError;
use vivi::cli::{
    PumpHealthCommand, PumpLogCommand, PumpPriorityCommand, PumpResetCommand, PumpRungsCommand,
    PumpServiceCommand, PumpSubcommand, PumpTargetCommand,
};
use vivi::mailspace::{Mailspace, PumpActionRequest};
use vivi::pump::config::RungsConfig;
use vivi::pump::config::targets::{self, TargetSpec};
use vivi::pump::control::PumpMode;
use vivi::pump::health::{self, HealthOptions};
use vivi::pump::logview::{self, LogFile, LogFilter, ShowOptions};
use vivi::pump::priority;
use vivi::pump::reset::ResetReport;
use vivi::pump::run as pump_run;

/// Post an unlock action on the need's open pump record (`pump unlock`).
fn post_unlock(
    project: &Path,
    need: &str,
    entry: Option<&str>,
    note: Option<&str>,
) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(Some(project))?;
    let request = PumpActionRequest::unlock(entry, note.unwrap_or_default());
    let posted = mailspace.pump_post_action(None, need, &request)?;
    println!(
        "unlock posted on {} for need {}; the pump applies it at its next tick",
        posted.record, need
    );
    Ok(())
}

pub(crate) fn handle_pump_command(command: &PumpSubcommand) -> Result<(), VivariumError> {
    match command {
        PumpSubcommand::Run(cmd) if cmd.service_child => {
            pump_run::run_service_child(&project_root(&cmd.project))
        }
        PumpSubcommand::Run(cmd) => pump_run::run(&project_root(&cmd.project), false),
        PumpSubcommand::Once(cmd) => pump_run::run(&project_root(&cmd.project), true),
        PumpSubcommand::Health(cmd) => pump_health(cmd),
        PumpSubcommand::Log(cmd) => pump_log(cmd),
        PumpSubcommand::Pause(cmd) => {
            vivi::pump::control::set_project_mode(&project_root(&cmd.project), PumpMode::Paused)
        }
        PumpSubcommand::Resume(cmd) | PumpSubcommand::Start(cmd) => {
            vivi::pump::control::set_project_mode(&project_root(&cmd.project), PumpMode::Running)
        }
        PumpSubcommand::Stop(cmd) => {
            vivi::pump::control::set_project_mode(&project_root(&cmd.project), PumpMode::Stopped)
        }
        PumpSubcommand::Reset(cmd) => pump_reset(cmd),
        PumpSubcommand::Priority { command } => match command {
            PumpPriorityCommand::Set(cmd) => {
                priority::run_set(&project_root(&cmd.project), &cmd.need, cmd.level)
            }
            PumpPriorityCommand::Clear(cmd) => {
                priority::run_clear(&project_root(&cmd.project), &cmd.need)
            }
            PumpPriorityCommand::List(cmd) => priority::run_list(&project_root(&cmd.project)),
        },
        PumpSubcommand::Rungs { command } => pump_rungs(command),
        PumpSubcommand::Target { command } => pump_target(command),
        PumpSubcommand::Requeue(cmd) => vivi::pump::requeue::run(&project_root(&cmd.project), cmd),
        PumpSubcommand::Withdraw(cmd) => {
            vivi::pump::withdraw::run(&project_root(&cmd.project), cmd)
        }
        PumpSubcommand::Replan(cmd) => vivi::pump::replan::run(&project_root(&cmd.project), cmd),
        PumpSubcommand::Answer {
            step,
            word,
            note,
            project,
        } => vivi::pump::flow::answer::run(&project_root(project), step, word, note.as_deref()),
        PumpSubcommand::Unlock {
            need,
            entry,
            note,
            project,
        } => post_unlock(
            &project_root(project),
            need,
            entry.as_deref(),
            note.as_deref(),
        ),
        PumpSubcommand::Send(cmd) => crate::local_pump_record_command::send(cmd),
        PumpSubcommand::List(cmd) => crate::local_pump_record_command::list(cmd),
        PumpSubcommand::Show(cmd) => crate::local_pump_record_command::show(cmd),
        PumpSubcommand::Done(cmd) => crate::local_pump_record_command::done(cmd),
        PumpSubcommand::Reopen(cmd) => crate::local_pump_record_command::reopen(cmd),
        PumpSubcommand::History(cmd) => crate::local_pump_record_command::history(cmd),
        PumpSubcommand::Service { command } => match command {
            PumpServiceCommand::Add(cmd) => {
                let (pump, serve) = cmd.flag_change();
                vivi::pump::service::add(
                    &cmd.project,
                    vivi::pump::service::FlagChange { pump, serve },
                )
            }
            PumpServiceCommand::Remove(cmd) => vivi::pump::service::remove(&cmd.project),
            PumpServiceCommand::List => vivi::pump::service::list(),
            PumpServiceCommand::Install => vivi::pump::service::install(),
            PumpServiceCommand::Uninstall => vivi::pump::service::uninstall(),
            PumpServiceCommand::Status => vivi::pump::service::status(),
            PumpServiceCommand::Run => Err(VivariumError::Other(
                "pump service run requires the async command entry point".into(),
            )),
        },
    }
}

const RESET_NOTE: &str = "pump records are letters: the next tick re-ingests every open record and starts fresh runs; use vivi pump withdraw to stop a need instead";

fn pump_reset(cmd: &PumpResetCommand) -> Result<(), VivariumError> {
    let report = vivi::pump::reset::archive(&project_root(&cmd.project))?;
    print_reset(&report, cmd.json)
}

fn print_reset(report: &ResetReport, json: bool) -> Result<(), VivariumError> {
    if json {
        let text = reset_json(report)?;
        println!("{text}");
        return Ok(());
    }
    println!("{}", report.archive_dir.display());
    for name in &report.moved {
        println!("moved {name}");
    }
    println!("control: {}", report.mode);
    println!("open pump records: {}", report.open_records);
    println!("{RESET_NOTE}");
    Ok(())
}

fn reset_json(report: &ResetReport) -> Result<String, VivariumError> {
    let value = serde_json::json!({
        "archive_dir": report.archive_dir.display().to_string(),
        "moved": &report.moved,
        "mode": report.mode.to_string(),
        "open_records": report.open_records,
        "note": RESET_NOTE,
    });
    serde_json::to_string_pretty(&value)
        .map_err(|error| VivariumError::Message(format!("cannot encode reset: {error}")))
}

fn pump_log(cmd: &PumpLogCommand) -> Result<(), VivariumError> {
    let file = if cmd.exceptions {
        LogFile::Exceptions
    } else {
        LogFile::Log
    };
    let options = ShowOptions {
        count: cmd.lines,
        follow: cmd.follow,
        filter: LogFilter::new(cmd.unit.as_deref(), cmd.grep.as_deref()),
    };
    logview::run(&project_root(&cmd.project), file, &options)
}

/// The project root: the subcommand's `--project`, else the current
/// directory (whose ancestors `Mailspace::discover` walks).
fn pump_health(cmd: &PumpHealthCommand) -> Result<(), VivariumError> {
    let opts = HealthOptions {
        since: cmd.since,
        stale_after: cmd.stale_after,
        now: None,
    };
    let report = health::health(&project_root(&cmd.project), &opts)?;
    if cmd.json {
        let text = serde_json::to_string_pretty(&report)
            .map_err(|error| VivariumError::Message(format!("cannot encode health: {error}")))?;
        println!("{text}");
    } else {
        print!("{}", health::render(&report));
    }
    Ok(())
}

fn pump_rungs(command: &PumpRungsCommand) -> Result<(), VivariumError> {
    match command {
        PumpRungsCommand::Show(cmd) => {
            let text = RungsConfig::show(&project_root(&cmd.project), cmd.target.as_deref())?;
            print!("{text}");
            Ok(())
        }
        PumpRungsCommand::Set(cmd) => {
            let root = project_root(&cmd.project);
            let write = RungsConfig::set(&root, &cmd.rung, &cmd.command, cmd.target.as_deref())?;
            println!("{} rung set for {}", cmd.rung, root.display());
            for line in write.notes() {
                eprintln!("{line}");
            }
            Ok(())
        }
    }
}

fn pump_target(command: &PumpTargetCommand) -> Result<(), VivariumError> {
    match command {
        PumpTargetCommand::List(cmd) => {
            let rows = targets::list(&project_root(&cmd.project))?;
            if rows.is_empty() {
                println!("no pump targets");
            }
            for row in rows {
                let rungs = if row.rungs.is_empty() {
                    String::from("-")
                } else {
                    row.rungs.join(",")
                };
                println!(
                    "{} path={} main={} rungs={rungs}",
                    row.id, row.path, row.main
                );
            }
            Ok(())
        }
        PumpTargetCommand::Add(cmd) => {
            let spec = TargetSpec {
                id: cmd.id.clone(),
                path: cmd.path.clone(),
                remote_url: cmd.remote_url.clone(),
                remote: cmd.remote.clone(),
                main: cmd.main.clone(),
                github_repo: cmd.github_repo.clone(),
                rungs: split_all(&cmd.rungs, "rung")?,
                merge: split_all(&cmd.merge, "merge")?,
            };
            let write = targets::add(&project_root(&cmd.project), &spec)?;
            println!("added pump target {}", cmd.id);
            for line in write.notes() {
                eprintln!("{line}");
            }
            Ok(())
        }
        PumpTargetCommand::Remove(cmd) => {
            let write = targets::remove(&project_root(&cmd.project), &cmd.id)?;
            println!(
                "removed pump target {}; a pump that is already up keeps it until it restarts",
                cmd.id
            );
            for line in write.notes() {
                eprintln!("{line}");
            }
            Ok(())
        }
    }
}

fn split_all(items: &[String], flag: &str) -> Result<Vec<(String, String)>, VivariumError> {
    items
        .iter()
        .map(|item| targets::split_pair(item, flag))
        .collect()
}

fn project_root(project: &Option<PathBuf>) -> PathBuf {
    project
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}
