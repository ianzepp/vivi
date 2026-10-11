use std::path::Path;

use vivi::VivariumError;
use vivi::chat::chat_dir;
use vivi::chat::identity::ChatIdentity;
use vivi::cli::{SyncCommand, SyncSubcommand};
use vivi::mailspace::Mailspace;
use vivi::sync::http::{self, HttpTransport};
use vivi::sync::state::SyncState;
use vivi::sync::{SyncSettings, pull, push, sync_dir};

pub(crate) fn handle_sync_command(command: &SyncCommand) -> Result<(), VivariumError> {
    let top = command.project.as_deref();
    match &command.command {
        None => run_passes(top, true, true),
        Some(SyncSubcommand::Push { project }) => {
            run_passes(project.as_deref().or(top), true, false)
        }
        Some(SyncSubcommand::Pull { project }) => {
            run_passes(project.as_deref().or(top), false, true)
        }
        Some(SyncSubcommand::Join { project }) => join(project.as_deref().or(top)),
        Some(SyncSubcommand::Status { project }) => status(project.as_deref().or(top)),
    }
}

/// Push, then pull. A stopped or refused push does not skip the pull; every
/// failure is reported and the command exits non-zero at the end.
fn run_passes(project: Option<&Path>, push: bool, pull: bool) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    let settings = SyncSettings::resolve(&mailspace, pull)?;
    let identity = ChatIdentity::ensure(&chat_dir(&mailspace.dir))?;
    let transport = HttpTransport::new(&settings, &identity);
    let mut failures = Vec::new();
    if push {
        let report = push::push(&mailspace, &settings, &transport)?;
        for line in report.lines() {
            println!("{line}");
        }
        failures.extend(report.failure());
    }
    if pull {
        match pull::pull(&mailspace, &settings, &transport) {
            Ok(report) => {
                println!("{}", report.summary());
                for warning in &report.warnings {
                    eprintln!("warning: {warning}");
                }
                for refused in &report.refused {
                    eprintln!("refused {}: {}", refused.message_id, refused.reason);
                }
                failures.extend(report.failure());
            }
            Err(error) => failures.push(format!("pull failed: {error}")),
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(VivariumError::Message(failures.join("; ")))
    }
}

fn join(project: Option<&Path>) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    let settings = SyncSettings::resolve(&mailspace, false)?;
    let identity = ChatIdentity::ensure(&chat_dir(&mailspace.dir))?;
    let receipt = http::join(&settings, &identity)?;
    println!("joined project {} as {}", settings.project, receipt.name);
    println!("fingerprint {}", receipt.fingerprint);
    Ok(())
}

fn status(project: Option<&Path>) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    let identity = ChatIdentity::ensure(&chat_dir(&mailspace.dir))?;
    let config = mailspace.config.sync.clone().unwrap_or_default();
    let show = |value: &Option<String>| value.clone().unwrap_or_else(|| "(not set)".into());
    println!(
        "sync identity: {} (wire {})",
        identity.fingerprint(),
        identity.wire_fingerprint()
    );
    println!("sync server: {}", show(&config.server));
    println!("sync project: {}", show(&config.project));
    println!("sync name: {}", show(&config.name));
    println!("sync recipient: {}", show(&config.recipient));
    if let Some(project) = config.project.as_deref().filter(|p| !p.trim().is_empty()) {
        let state = SyncState::load(&sync_dir(&mailspace.dir), project.trim())?;
        println!("sync cursor: {}", state.since);
    } else {
        println!("sync cursor: (project not set)");
    }
    match SyncSettings::resolve(&mailspace, false) {
        Ok(settings) => println!(
            "sync unsent: {} letter(s)",
            push::unsent(&mailspace, &settings)?
        ),
        Err(error) => println!("sync unsent: unknown ({error})"),
    }
    Ok(())
}
