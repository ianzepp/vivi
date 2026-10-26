use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::Path;

use vivi::VivariumError;
use vivi::cli::{ApiKeyCommand, QuarantineCommand, SyncCommand, SyncSubcommand};
use vivi::mailspace::Mailspace;
use vivi::sync::fetch::fetch_handles;
use vivi::sync::http::{self, HttpTransport};
use vivi::sync::identity::SyncIdentity;
use vivi::sync::kept::{self as sync_kept, KeptLocal};
use vivi::sync::keys;
use vivi::sync::lock::SyncLock;
use vivi::sync::prune as sync_prune;
use vivi::sync::pull::PullPlan;
use vivi::sync::quarantine::{self, RefileOutcome};
use vivi::sync::state::SyncState;
use vivi::sync::vouch::{compose_vouch, send_vouch};
use vivi::sync::window::{parse_age, parse_since};
use vivi::sync::{SyncSettings, pull, push, refs, sync_dir};

/// How a pull differs from continuing at the stored cursor.
#[derive(Default)]
struct PullOptions {
    since: Option<String>,
    resync: bool,
    refs_depth: Option<u8>,
}

/// The push pass: its `--since` window and whether to drain.
struct PushPass {
    since: Option<String>,
    drain: bool,
}

/// The mailspace with its resolved `[sync]` settings and identity: what every
/// command that talks to the server starts from.
fn connect(
    project: Option<&Path>,
) -> Result<(Mailspace, SyncSettings, SyncIdentity), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    let settings = SyncSettings::resolve(&mailspace)?;
    let identity = SyncIdentity::ensure(&mailspace.dir)?;
    Ok((mailspace, settings, identity))
}

/// A non-blank value, trimmed.
fn present(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

pub(crate) fn handle_sync_command(command: &SyncCommand) -> Result<(), VivariumError> {
    let top = command.project.as_deref();
    let plain = Some(PullOptions::default());
    match &command.command {
        None => run_passes(
            top,
            Some(PushPass {
                since: None,
                drain: command.drain,
            }),
            plain,
        ),
        Some(SyncSubcommand::Push {
            since,
            drain,
            project,
        }) => run_passes(
            project.as_deref().or(top),
            Some(PushPass {
                since: since.clone(),
                drain: *drain,
            }),
            None,
        ),
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
                None,
                Some(PullOptions {
                    since: since.clone(),
                    resync: false,
                    refs_depth: refs.then_some(*depth),
                }),
            )
        }
        Some(SyncSubcommand::Resync { project }) => run_passes(
            project.as_deref().or(top),
            None,
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
        Some(SyncSubcommand::KeepLocal {
            handles,
            undo,
            project,
        }) => keep_local(project.as_deref().or(top), handles, *undo),
        Some(SyncSubcommand::Quarantine { command }) => quarantine(top, command),
        Some(SyncSubcommand::Join { project }) => join(project.as_deref().or(top)),
        Some(SyncSubcommand::Status { project }) => status(project.as_deref().or(top)),
        Some(SyncSubcommand::Directory { project }) => directory(project.as_deref().or(top)),
        Some(SyncSubcommand::Key { project }) => key(project.as_deref().or(top)),
        Some(SyncSubcommand::Create { name, project }) => {
            create(project.as_deref().or(top), name.as_deref())
        }
        Some(SyncSubcommand::ApiKey { command }) => match command {
            ApiKeyCommand::Mint {
                label,
                out,
                project,
            } => api_key_mint(project.as_deref().or(top), label, out.as_deref()),
            ApiKeyCommand::List { project } => api_key_list(project.as_deref().or(top)),
            ApiKeyCommand::Revoke { key_id, project } => {
                api_key_revoke(project.as_deref().or(top), key_id)
            }
        },
        Some(SyncSubcommand::Vouch {
            member,
            key_file,
            key,
            project,
        }) => vouch(
            project.as_deref().or(top),
            member,
            key_file.as_deref(),
            key.as_deref(),
        ),
    }
}

/// Push, then pull. A stopped or refused push does not skip the pull; every
/// failure is reported and the command exits non-zero at the end.
///
/// `push` is `None` for no push pass, `Some` for one with a `--since` window
/// and a `--drain` flag.
fn run_passes(
    project: Option<&Path>,
    push: Option<PushPass>,
    pull: Option<PullOptions>,
) -> Result<(), VivariumError> {
    let (mailspace, settings, identity) = connect(project)?;
    let transport = HttpTransport::new(&settings, &identity);
    let _lock = SyncLock::acquire(&sync_dir(&mailspace.dir))?;
    let mut failures = Vec::new();
    if let Some(pass) = push {
        let window = pass.since.as_deref().map(push_window).transpose()?;
        let report = run_push(&mailspace, &settings, &transport, window, pass.drain)?;
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

/// Resolve a push `--since` value to the instant the window starts at.
fn push_window(since: &str) -> Result<chrono::DateTime<chrono::Utc>, VivariumError> {
    let start = parse_since(since, chrono::Utc::now())?;
    chrono::DateTime::parse_from_rfc3339(&start)
        .map(|time| time.with_timezone(&chrono::Utc))
        .map_err(|error| VivariumError::Config(format!("invalid --since '{since}': {error}")))
}

/// One push pass; with `drain`, wait out the rate limit between passes. The
/// sync lock is the caller's and stays held across all passes. The wait is a
/// real sleep here; tests pass a no-op.
fn run_push(
    mailspace: &Mailspace,
    settings: &SyncSettings,
    transport: &HttpTransport<'_>,
    window: Option<chrono::DateTime<chrono::Utc>>,
    drain: bool,
) -> Result<push::PushReport, VivariumError> {
    if !drain {
        return push::push_since(mailspace, settings, transport, window);
    }
    push::push_drain(
        mailspace,
        settings,
        transport,
        window,
        &mut |duration| {
            println!(
                "sync push: rate limit reached; waiting {}s",
                duration.as_secs()
            );
            std::thread::sleep(duration);
        },
        &mut |report, pass| {
            let left = report.stopped.as_ref().map_or(0, |stop| stop.remaining);
            println!(
                "sync push: pass {pass}: sent {}, duplicates {}, {left} left",
                report.sent, report.duplicates
            );
        },
    )
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
    let (mailspace, settings, identity) = connect(project)?;
    let transport = HttpTransport::new(&settings, &identity);
    let _lock = SyncLock::acquire(&sync_dir(&mailspace.dir))?;
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
    let age = parse_age(older_than)?;
    let _lock = SyncLock::acquire(&sync_dir(&mailspace.dir))?;
    let report = sync_prune::prune(&mailspace, age, chrono::Utc::now(), dry_run)?;
    for line in sync_prune::report_lines(&report) {
        println!("{line}");
    }
    Ok(())
}

/// Local only: no server, no key.
fn keep_local(project: Option<&Path>, handles: &[String], undo: bool) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    let settings = SyncSettings::resolve(&mailspace)?;
    let _lock = SyncLock::acquire(&sync_dir(&mailspace.dir))?;
    for line in sync_kept::keep_local(&mailspace, &settings, handles, undo)? {
        println!("{line}");
    }
    Ok(())
}

/// Local only: no server, no key. The clearing surface for letters a pull
/// held unread under `.vivi/quarantine/`. An unknown or malformed id is an
/// error, so every verb either prints or exits non-zero.
fn quarantine(top: Option<&Path>, command: &QuarantineCommand) -> Result<(), VivariumError> {
    let project = match command {
        QuarantineCommand::List { project }
        | QuarantineCommand::Retry { project, .. }
        | QuarantineCommand::Drop { project, .. } => project.as_deref().or(top),
    };
    let mailspace = Mailspace::discover(project)?;
    let _lock = SyncLock::acquire(&sync_dir(&mailspace.dir))?;
    match command {
        QuarantineCommand::List { .. } => {
            let held = quarantine::list(&mailspace.dir)?;
            if held.is_empty() {
                println!("no quarantined letters");
                return Ok(());
            }
            for letter in &held {
                println!(
                    "{}  held {}  from {}  {}  {}",
                    letter.id, letter.at, letter.from, letter.message_id, letter.reason
                );
            }
        }
        QuarantineCommand::Retry { id, .. } => {
            let settings = SyncSettings::resolve(&mailspace)?;
            match quarantine::refile(&mailspace, &settings, id)? {
                RefileOutcome::Filed => println!("filed {id}; the pair moved to dropped/"),
                RefileOutcome::StillHeld(reason) => println!("still held {id}: {reason}"),
            }
        }
        QuarantineCommand::Drop { id, .. } => {
            quarantine::discard(&mailspace.dir, id)?;
            println!("dropped {id}; the pair moved to dropped/");
        }
    }
    Ok(())
}

fn join(project: Option<&Path>) -> Result<(), VivariumError> {
    let mut mailspace = Mailspace::discover(project)?;
    warn_if_config_tracked(&mailspace);
    // Before the request, so an unreachable server still leaves the role in
    // place.
    mailspace.ensure_operator_role()?;
    mailspace.ensure_merge_role()?;
    mailspace.ensure_pump_planner_role()?;
    mailspace.ensure_flow_roles()?;
    let settings = SyncSettings::resolve(&mailspace)?;
    let identity = SyncIdentity::ensure(&mailspace.dir)?;
    let receipt = http::join(&settings, &identity)?;
    let verb = if receipt.already_joined {
        "already joined"
    } else {
        "joined"
    };
    println!("{verb} project {} as {}", settings.project, receipt.name);
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
    let identity = SyncIdentity::ensure(&mailspace.dir)?;
    let config = mailspace.config.sync.clone().unwrap_or_default();
    let show = |value: &Option<String>| value.clone().unwrap_or_else(|| "(not set)".into());
    println!(
        "sync identity: {} (wire {})",
        identity.fingerprint(),
        identity.wire_fingerprint()
    );
    println!(
        "sync key: vivi sync key prints the public-key PEM to send to an admitted member; they \
         run vivi sync vouch <name> --key-file <pem>"
    );
    println!("sync server: {}", show(&config.server));
    println!("sync project: {}", show(&config.project));
    println!("sync name: {}", show(&config.name));
    println!("sync address: {}", mailspace.address_domain());
    if let Some(project) = config.project.as_deref().filter(|p| !p.trim().is_empty()) {
        let state = SyncState::load(&sync_dir(&mailspace.dir), project.trim())?;
        println!("sync cursor: {}", state.since);
        println!("sync {}", state.epoch_line());
        println!("sync {}", state.history_line());
    } else {
        println!("sync cursor: (project not set; vivi sync create makes a new project)");
    }
    match SyncSettings::resolve(&mailspace) {
        Ok(settings) => {
            let pending = push::pending(&mailspace, &settings)?;
            let kept = KeptLocal::load(&sync_dir(&mailspace.dir))?;
            println!("sync unsent: {} letter(s)", pending.unsent());
            for line in sync_kept::status_lines(&pending, kept.len()) {
                println!("{line}");
            }
        }
        Err(error) => println!("sync unsent: unknown ({error})"),
    }
    let held = mailspace.held_for_unknown_roles()?;
    if !held.is_empty() {
        let list: Vec<String> = held
            .iter()
            .map(|(name, count)| format!("{name} ({count})"))
            .collect();
        println!(
            "sync held for unknown roles: {}; `vivi role add <name>` adopts them",
            list.join(", ")
        );
    }
    Ok(())
}

/// One line per member: name, address domain (`-` when the server has none),
/// and a short fingerprint.
fn directory(project: Option<&Path>) -> Result<(), VivariumError> {
    let (_, settings, identity) = connect(project)?;
    let transport = HttpTransport::new(&settings, &identity);
    for member in transport.directory()? {
        println!("{}", member.line());
    }
    Ok(())
}

/// Local only: no [sync] table, no server, no network. Mints the key when
/// the mailspace has none.
fn key(project: Option<&Path>) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    let identity = SyncIdentity::ensure(&mailspace.dir)?;
    println!("{}", identity.public_key_pem());
    Ok(())
}

/// Create the project on the server and print its id. Writes nothing: the
/// operator puts the printed id into [sync] and runs `vivi sync join`.
fn create(project: Option<&Path>, creator: Option<&str>) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    let config = mailspace.config.sync.clone().unwrap_or_default();
    let server = present(config.server.as_deref()).ok_or_else(|| {
        VivariumError::Config(
            "[sync] server is not set; set server = \"...\" in mailspace.toml (the project \
             server URL)"
                .into(),
        )
    })?;
    let creator_name = present(creator)
        .or_else(|| present(config.name.as_deref()))
        .ok_or_else(|| {
            VivariumError::Config(
                "no creator name: pass --name CREATOR or set [sync] name in mailspace.toml".into(),
            )
        })?;
    let identity = SyncIdentity::ensure(&mailspace.dir)?;
    let project_id = http::create_project(server, &identity, creator_name)?;
    println!("project {project_id}");
    println!("creator {creator_name}");
    println!(
        "write project = \"{project_id}\" to the [sync] table in mailspace.toml, then run \
         vivi sync join"
    );
    Ok(())
}

/// Vouch for a member as this mailspace, already admitted: one governance
/// letter to the postmaster. Nothing is stored or filed locally, so no sync
/// lock is taken; the name and key are checked before any request is made.
fn vouch(
    project: Option<&Path>,
    member: &str,
    key_file: Option<&Path>,
    key: Option<&str>,
) -> Result<(), VivariumError> {
    let pem = match (key_file, key) {
        (Some(path), _) => std::fs::read_to_string(path)?,
        (None, Some(key)) => key.to_string(),
        (None, None) => {
            return Err(VivariumError::Config(
                "nothing to vouch: pass --key-file PATH or --key PEM".into(),
            ));
        }
    };
    let (mailspace, settings, identity) = connect(project)?;
    let letter = compose_vouch(
        &settings.name,
        &mailspace.address_domain(),
        member,
        pem.trim(),
        chrono::Utc::now(),
    )?;
    let transport = HttpTransport::new(&settings, &identity);
    let appended = send_vouch(&transport, &letter)?;
    println!("vouched {member} as {}", appended.message_id);
    Ok(())
}

/// Mint a PROJECT API key (a bearer for `vivi mcp` and the letter wire; not
/// `vivi sync key`). Without `--out` the raw key is printed once on stdout
/// and stored nowhere; the one stderr note says so and how to revoke. With
/// `--out <file>` the key is written to a new 0600 file and stdout carries
/// only the key id, the label, and the file path — never the key. The file
/// is created before the server is contacted, so a refused path mints
/// nothing; a mint that fails after the file was created removes it again.
fn api_key_mint(
    project: Option<&Path>,
    label: &str,
    out: Option<&Path>,
) -> Result<(), VivariumError> {
    let (_, settings, identity) = connect(project)?;
    let target = match out {
        Some(path) => Some((path, create_secret_file(path)?)),
        None => None,
    };
    let minted = match keys::mint_key(&settings, &identity, label) {
        Ok(minted) => minted,
        Err(error) => {
            if let Some((path, _)) = target {
                remove_created_key(path);
            }
            return Err(error);
        }
    };
    match target {
        Some((path, file)) => {
            if let Err(error) = write_secret_file(file, path, &minted.key) {
                remove_created_key(path);
                return Err(error);
            }
            println!("key_id {}", minted.record.key_id);
            println!("label {}", minted.record.label);
            println!("out {}", path.display());
        }
        None => {
            println!("key_id {}", minted.record.key_id);
            println!("label {}", minted.record.label);
            println!("key {}", minted.key);
            eprintln!(
                "the key is shown once; Vivi does not store it. Revoke with: vivi sync api-key revoke {}",
                minted.record.key_id
            );
        }
    }
    Ok(())
}

/// Create `path` as a brand-new file with mode 0600: an existing file is
/// refused (never overwritten) and a missing parent directory is refused
/// (never created). Called before the server is contacted, so a refused
/// path mints nothing.
fn create_secret_file(path: &Path) -> Result<std::fs::File, VivariumError> {
    let parent = path.parent().unwrap_or(Path::new(""));
    if !parent.as_os_str().is_empty() && !parent.is_dir() {
        return Err(VivariumError::Message(format!(
            "cannot create {}: the parent directory {} does not exist",
            path.display(),
            parent.display()
        )));
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::AlreadyExists => VivariumError::Message(format!(
                "refusing to write the key: {} already exists",
                path.display()
            )),
            _ => VivariumError::Message(format!("cannot create {}: {error}", path.display())),
        })
}

/// Remove the key file this command just created, after a failed mint or a
/// failed write. The command's own error is what gets returned; a file that
/// cannot be removed is reported as a warning beside it.
fn remove_created_key(path: &Path) {
    if let Err(error) = std::fs::remove_file(path) {
        eprintln!("warning: cannot remove {}: {error}", path.display());
    }
}

/// Write the raw key and one newline to the fresh file, then pin the mode
/// at 0600 whatever the umask allowed at creation.
fn write_secret_file(mut file: std::fs::File, path: &Path, key: &str) -> Result<(), VivariumError> {
    let mut write = || -> std::io::Result<()> {
        file.write_all(key.as_bytes())?;
        file.write_all(b"\n")?;
        file.flush()?;
        let mut permissions = std::fs::metadata(path)?.permissions();
        permissions.set_mode(0o600);
        std::fs::set_permissions(path, permissions)
    };
    write().map_err(|error| {
        VivariumError::Message(format!(
            "cannot write the key to {}: {error}",
            path.display()
        ))
    })
}

/// List the project's live API keys; revoked ones are not shown and no
/// secret ever is.
fn api_key_list(project: Option<&Path>) -> Result<(), VivariumError> {
    let (_, settings, identity) = connect(project)?;
    let records = keys::list_keys(&settings, &identity)?;
    if records.is_empty() {
        println!("no api keys");
        return Ok(());
    }
    for record in records {
        println!(
            "{}  {}  {}  {}",
            record.key_id, record.label, record.created_by, record.created_at
        );
    }
    Ok(())
}

/// Revoke one PROJECT API key by its key id.
fn api_key_revoke(project: Option<&Path>, key_id: &str) -> Result<(), VivariumError> {
    let (_, settings, identity) = connect(project)?;
    keys::revoke_key(&settings, &identity, key_id)?;
    println!("revoked {key_id}");
    Ok(())
}
