//! `vivi pump` dispatch: `run`, `once`, and `status` over the run module.

use std::path::PathBuf;

use vivi::VivariumError;
use vivi::cli::{
    PumpGateCommand, PumpHealthCommand, PumpLogCommand, PumpPriorityCommand, PumpServiceCommand,
    PumpSubcommand,
};
use vivi::pump::control::PumpMode;
use vivi::pump::health::{self, HealthOptions};
use vivi::pump::logview::{self, LogFile, LogFilter, ShowOptions};
use vivi::pump::priority;
use vivi::pump::run as pump_run;

pub(crate) fn handle_pump_command(command: &PumpSubcommand) -> Result<(), VivariumError> {
    match command {
        PumpSubcommand::Run(cmd) if cmd.service_child => {
            pump_run::run_service_child(&project_root(&cmd.project))
        }
        PumpSubcommand::Run(cmd) => pump_run::run(&project_root(&cmd.project), false),
        PumpSubcommand::Once(cmd) => pump_run::run(&project_root(&cmd.project), true),
        PumpSubcommand::Status(cmd) => {
            let text = pump_run::status(&project_root(&cmd.project))?;
            print!("{text}");
            Ok(())
        }
        PumpSubcommand::Health(cmd) => pump_health(cmd),
        PumpSubcommand::Log(cmd) => pump_log(cmd),
        PumpSubcommand::Export(cmd) => pump_run::export(&project_root(&cmd.project), &cmd.json),
        PumpSubcommand::Closeout(cmd) => pump_run::closeout(
            &project_root(&cmd.project),
            cmd.json,
            cmd.close.as_deref(),
            cmd.for_identity.as_deref(),
        ),
        PumpSubcommand::Pause(cmd) => {
            vivi::pump::control::set_project_mode(&project_root(&cmd.project), PumpMode::Paused)
        }
        PumpSubcommand::Resume(cmd) | PumpSubcommand::Start(cmd) => {
            vivi::pump::control::set_project_mode(&project_root(&cmd.project), PumpMode::Running)
        }
        PumpSubcommand::Stop(cmd) => {
            vivi::pump::control::set_project_mode(&project_root(&cmd.project), PumpMode::Stopped)
        }
        PumpSubcommand::Priority { command } => match command {
            PumpPriorityCommand::Set(cmd) => {
                priority::run_set(&project_root(&cmd.project), &cmd.need, cmd.level)
            }
            PumpPriorityCommand::Clear(cmd) => {
                priority::run_clear(&project_root(&cmd.project), &cmd.need)
            }
            PumpPriorityCommand::List(cmd) => priority::run_list(&project_root(&cmd.project)),
        },
        PumpSubcommand::Gate { command } => pump_gate(command),
        PumpSubcommand::Requeue(cmd) => vivi::pump::requeue::run(&project_root(&cmd.project), cmd),
        PumpSubcommand::Withdraw(cmd) => {
            vivi::pump::withdraw::run(&project_root(&cmd.project), cmd)
        }
        PumpSubcommand::Replan(cmd) => vivi::pump::replan::run(&project_root(&cmd.project), cmd),
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

fn pump_log(cmd: &PumpLogCommand) -> Result<(), VivariumError> {
    let file = if cmd.exceptions {
        LogFile::Exceptions
    } else {
        LogFile::Log
    };
    let options = ShowOptions {
        count: cmd.lines,
        follow: cmd.follow,
        filter: LogFilter::new(
            cmd.lane.as_deref(),
            cmd.unit.as_deref(),
            cmd.grep.as_deref(),
        ),
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

fn pump_gate(command: &PumpGateCommand) -> Result<(), VivariumError> {
    match command {
        PumpGateCommand::Show(cmd) => {
            match vivi::pump::gate_config::command(&project_root(&cmd.project))? {
                Some(text) => println!("{text}"),
                None => println!("no gate command set"),
            }
            Ok(())
        }
        PumpGateCommand::Set(cmd) => {
            let root = project_root(&cmd.project);
            vivi::pump::gate_config::set_command(&root, &cmd.command)?;
            println!("gate command set for {}", root.display());
            Ok(())
        }
    }
}

fn project_root(project: &Option<PathBuf>) -> PathBuf {
    project
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}
