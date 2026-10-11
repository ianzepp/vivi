use std::path::Path;

use vivi::VivariumError;
use vivi::chat::chat_dir;
use vivi::chat::identity::ChatIdentity;
use vivi::cli::{SyncCommand, SyncSubcommand};
use vivi::mailspace::Mailspace;
use vivi::sync::fetch::fetch_handles;
use vivi::sync::http::{self, HttpTransport};
use vivi::sync::prune as sync_prune;
use vivi::sync::pull::PullPlan;
use vivi::sync::state::SyncState;
use vivi::sync::window::parse_since;
use vivi::sync::{SyncSettings, pull, push, refs, sync_dir};

/// How a pull differs from continuing at the stored cursor.
#[derive(Default)]
struct PullOptions {
    since: Option<String>,
    resync: bool,
    refs_depth: Option<u8>,
}

pub(crate) fn handle_sync_command(command: &SyncCommand) -> Result<(), VivariumError> {
    let top = command.project.as_deref();
    let plain = Some(PullOptions::default());
    match &command.command {
        None => run_passes(top, true, plain),
        Some(SyncSubcommand::Push { project }) => {
            run_passes(project.as_deref().or(top), true, None)
        }
        Some(SyncSubcommand::Pull {
            since,
            refs,
            depth,
            project,
        }) => {
            if *refs {
                vivi::sync::refs::check_depth(*depth)?;
            }
            run_passes(
                project.as_deref().or(top),
                false,
                Some(PullOptions {
                    since: since.clone(),
                    resync: false,
                    refs_depth: refs.then_some(*depth),
                }),
            )
        }
        Some(SyncSubcommand::Resync { project }) => run_passes(
            project.as_deref().or(top),
            false,
            Some(PullOptions {
                resync: true,
                ..PullOptions::default()
            }),
        ),
        Some(SyncSubcommand::Fetch { handles, project }) => {
            fetch(project.as_deref().or(top), handles)
        }
        Some(SyncSubcommand::Prune {
            older_than,
            dry_run,
            project,
        }) => prune(project.as_deref().or(top), older_than, *dry_run),
        Some(SyncSubcommand::Join { project }) => join(project.as_deref().or(top)),
        Some(SyncSubcommand::Status { project }) => status(project.as_deref().or(top)),
        Some(SyncSubcommand::Directory { project }) => directory(project.as_deref().or(top)),
    }
}

/// Push, then pull. A stopped or refused push does not skip the pull; every
/// failure is reported and the command exits non-zero at the end.
fn run_passes(
    project: Option<&Path>,
    push: bool,
    pull: Option<PullOptions>,
) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    let settings = SyncSettings::resolve(&mailspace)?;
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
    if let Some(options) = pull {
        match run_pull(&mailspace, &settings, &transport, &options) {
            Ok(report) => {
                print_pull(&report);
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

fn run_pull(
    mailspace: &Mailspace,
    settings: &SyncSettings,
    transport: &HttpTransport<'_>,
    options: &PullOptions,
) -> Result<pull::PullReport, VivariumError> {
    let window = options
        .since
        .as_deref()
        .map(|since| parse_since(since, chrono::Utc::now()))
        .transpose()?;
    let plan = PullPlan {
        window: window.as_deref(),
        resync: options.resync,
    };
    let mut report = pull::pull_with(mailspace, settings, transport, &plan, &mut |page| {
        // One line per page, once a pull is known to span more than one.
        if page.page > 1 || page.more {
            println!(
                "sync pull: page {}: {} letter(s), cursor {}{}",
                page.page,
                page.records,
                page.cursor,
                if page.more { "" } else { " (head)" }
            );
        }
    })?;
    if let Some(depth) = options.refs_depth {
        let refs = refs::follow_refs(mailspace, settings, transport, &mut report, depth)?;
        println!("{}", refs.summary());
    }
    Ok(report)
}

fn print_pull(report: &pull::PullReport) {
    println!("{}", report.summary());
    for warning in &report.warnings {
        eprintln!("warning: {warning}");
    }
    for refused in &report.refused {
        eprintln!("refused {}: {}", refused.message_id, refused.reason);
    }
}

fn fetch(project: Option<&Path>, handles: &[String]) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    let settings = SyncSettings::resolve(&mailspace)?;
    let identity = ChatIdentity::ensure(&chat_dir(&mailspace.dir))?;
    let transport = HttpTransport::new(&settings, &identity);
    let report = fetch_handles(&mailspace, &settings, &transport, handles)?;
    println!("{}", report.summary());
    for warning in &report.pull.warnings {
        eprintln!("warning: {warning}");
    }
    for refused in report.refused() {
        eprintln!("refused {}: {}", refused.message_id, refused.reason);
    }
    report
        .failure()
        .map_or(Ok(()), |m| Err(VivariumError::Message(m)))
}

/// Local only: no server, no key.
fn prune(project: Option<&Path>, older_than: &str, dry_run: bool) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    let age = vivi::chat::prune::parse_age(older_than)?;
    let report = sync_prune::prune(&mailspace, age, chrono::Utc::now(), dry_run)?;
    for line in sync_prune::report_lines(&report) {
        println!("{line}");
    }
    Ok(())
}

fn join(project: Option<&Path>) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    warn_if_config_tracked(&mailspace);
    let settings = SyncSettings::resolve(&mailspace)?;
    let identity = ChatIdentity::ensure(&chat_dir(&mailspace.dir))?;
    let receipt = http::join(&settings, &identity)?;
    println!("joined project {} as {}", settings.project, receipt.name);
    println!("fingerprint {}", receipt.fingerprint);
    Ok(())
}

fn warn_if_config_tracked(mailspace: &Mailspace) {
    if let Some(warning) = mailspace.tracked_config_warning() {
        eprintln!("{warning}");
    }
}

fn status(project: Option<&Path>) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    warn_if_config_tracked(&mailspace);
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
    println!("sync address: {}", mailspace.address_domain());
    if let Some(project) = config.project.as_deref().filter(|p| !p.trim().is_empty()) {
        let state = SyncState::load(&sync_dir(&mailspace.dir), project.trim())?;
        println!("sync cursor: {}", state.since);
        println!("sync {}", state.history_line());
    } else {
        println!("sync cursor: (project not set)");
    }
    match SyncSettings::resolve(&mailspace) {
        Ok(settings) => {
            let pending = push::pending(&mailspace, &settings)?;
            println!("sync unsent: {} letter(s)", pending.unsent());
            if let Some(line) = pending.not_uploaded_line() {
                println!("sync not uploaded: {line}; `vivi sync push` lists them");
            }
        }
        Err(error) => println!("sync unsent: unknown ({error})"),
    }
    Ok(())
}

/// One line per member: name, address domain (`-` when the server has none),
/// and a short fingerprint.
fn directory(project: Option<&Path>) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    let settings = SyncSettings::resolve(&mailspace)?;
    let identity = ChatIdentity::ensure(&chat_dir(&mailspace.dir))?;
    let transport = HttpTransport::new(&settings, &identity);
    for member in transport.directory()? {
        println!("{}", member.line());
    }
    Ok(())
}
