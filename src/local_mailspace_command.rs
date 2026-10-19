use vivi::VivariumError;
use vivi::cli::{
    Command, CycleCommand, LocalSendCommand, MailAbsorbStatus, MailCommand, MailDumpCommand,
    MailListCommand, MailReplyCommand, MailspaceCommand, MailspaceIdentityCommand,
    MailspaceImportCommand, MemoCommand, TaskCommand, TaskSendCommand, TraceCommand,
};
use vivi::mailspace::{
    DeliveredMessage, DumpFilters, MailAbsorbFilter, MailDumpRequest, Mailspace,
    MailspaceWatchRequest, SendRequest, SourceTaskRequest,
};
use vivi::message;
use vivi::storage::StoredMessageView;

pub(crate) fn run_mailspace_command(command: &Command) -> Result<(), VivariumError> {
    match command {
        Command::Mailspace { command } => handle_mailspace_command(command),
        Command::Board(command) => crate::local_board_command::handle_board_command(command),
        Command::Sync(command) => crate::local_sync_command::handle_sync_command(command),
        Command::Mcp(command) => crate::local_mcp_command::handle_mcp_command(command),
        Command::Boot {
            project,
            role,
            pump,
            json,
        } => crate::local_boot_command::handle_boot_command(
            project.as_deref(),
            role.as_deref(),
            *pump,
            *json,
        ),
        Command::Mail { command } => handle_mail_command(command),
        Command::Task { command } => handle_task_command(command),
        Command::Need { command } => crate::local_work_command::handle_need_command(command),
        Command::Want { command } => crate::local_work_command::handle_want_command(command),
        Command::Memo { command } => handle_memo_command(command),
        Command::Rule { command } => crate::local_rule_command::handle_rule_command(command),
        Command::Goal { command } => crate::local_goal_command::handle_goal_command(command),
        Command::Pool { command } => crate::local_pool_command::handle_pool_command(command),
        Command::Pump { command } => crate::local_pump_command::handle_pump_command(command),
        Command::Proxy { command } => vivi::proxy::handle(command),
        Command::Config { command } => crate::local_config_command::handle_config_command(command),
        Command::Role { command } => crate::local_role_command::handle_role_command(command),
        Command::Cycle { command } => handle_cycle_command(command),
        Command::Trace(command) => handle_trace_command(command),
        Command::Graph { command } => crate::local_graph_command::handle_graph_command(command),
        Command::Skills { command } => crate::local_skills_command::handle_skills_command(command),
        Command::Step {
            apply,
            project,
            json,
        } => crate::local_step_command::handle_step_command(
            apply.as_deref(),
            project.as_deref(),
            *json,
        ),
        Command::Update(command) => {
            if command.check {
                vivi::update::check_for_update().map(|_| ())
            } else {
                vivi::update::run_update(command)
            }
        }
        Command::Versions(command) => vivi::update::print_versions(command),
        Command::Uninstall(command) => vivi::update::uninstall(command),
    }
}

fn handle_mailspace_command(command: &MailspaceCommand) -> Result<(), VivariumError> {
    match command {
        MailspaceCommand::Init { project } => {
            let mut mailspace = Mailspace::init(project.as_deref())?;
            mailspace.ensure_operator_role()?;
            println!("mailspace {}", mailspace.config.name);
            println!("root      {}", mailspace.root.display());
            println!("store     {}", mailspace.store_path().display());
            let ignore = mailspace.dir.join(".gitignore");
            if ignore.is_file() {
                println!(
                    "ignore    {} (nothing in .vivi is committed)",
                    ignore.display()
                );
            }
        }
        MailspaceCommand::Status { project, json } => {
            let mailspace = Mailspace::discover(project.as_deref())?;
            if let Some(warning) = mailspace.tracked_config_warning() {
                eprintln!("{warning}");
            }
            let status = mailspace.status()?;
            if *json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&status).map_err(|e| {
                        VivariumError::Other(format!("failed to encode JSON: {e}"))
                    })?
                );
            } else {
                vivi::mailspace::print_status(&status);
            }
        }
        MailspaceCommand::Description { project, set } => {
            let mut mailspace = Mailspace::discover(project.as_deref())?;
            if let Some(description) = set {
                mailspace.set_description(Some(description.clone()))?;
                println!("description set");
            } else {
                let description = mailspace.config.description.as_deref().unwrap_or("(none)");
                println!("{description}");
            }
        }
        MailspaceCommand::Archive {
            project,
            set,
            clear,
            command,
        } => handle_archive_command(project.as_deref(), set.as_deref(), *clear, command.as_ref())?,
        MailspaceCommand::Watch(command) => run_watch(&command.common, &command.kinds)?,
        MailspaceCommand::Import(command) | MailspaceCommand::Merge(command) => {
            import_mailspace(command)?;
        }
        MailspaceCommand::Identity { command } => handle_mailspace_identity_command(command)?,
    }
    Ok(())
}

pub(crate) fn handle_archive_command(
    project: Option<&std::path::Path>,
    set: Option<&str>,
    clear: bool,
    command: Option<&vivi::cli::MailspaceArchiveCommand>,
) -> Result<(), VivariumError> {
    if let Some(vivi::cli::MailspaceArchiveCommand::Export {
        project: export_project,
        json,
    }) = command
    {
        return export_archive(export_project.as_deref().or(project), *json);
    }
    let mut mailspace = Mailspace::discover(project)?;
    if clear {
        mailspace.set_archive(None)?;
        println!("archive cleared");
        return Ok(());
    }
    if let Some(path) = set {
        mailspace.set_archive(Some(path.to_string()))?;
        println!("archive set");
        return Ok(());
    }
    println!(
        "{}",
        mailspace.config.archive.as_deref().unwrap_or("(none)")
    );
    Ok(())
}

pub(crate) fn export_archive(
    project: Option<&std::path::Path>,
    json: bool,
) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    let report = mailspace.export_archive()?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report)
                .map_err(|e| VivariumError::Other(format!("failed to encode JSON: {e}")))?
        );
        return Ok(());
    }
    println!(
        "scanned {}  written {}  unchanged {}",
        report.scanned, report.written, report.unchanged
    );
    Ok(())
}

fn handle_mailspace_identity_command(
    command: &MailspaceIdentityCommand,
) -> Result<(), VivariumError> {
    match command {
        MailspaceIdentityCommand::Add { identity, project } => {
            let mut mailspace = Mailspace::discover(project.as_deref())?;
            let address = mailspace.add_identity(identity)?;
            println!("added {address}");
            let role = mailspace.resolve_identity(identity)?;
            crate::local_role_command::report_adopted(&mailspace, &role)?;
        }
        MailspaceIdentityCommand::List { project } => {
            let mailspace = Mailspace::discover(project.as_deref())?;
            for identity in &mailspace.config.identities {
                println!(
                    "{} {}",
                    identity.name,
                    mailspace.address_for(&identity.name)
                );
                if !identity.aliases.is_empty() {
                    println!("  formerly: {}", identity.aliases.join(", "));
                }
            }
        }
        MailspaceIdentityCommand::Rename { old, new, project } => {
            let mut mailspace = Mailspace::discover(project.as_deref())?;
            let address = mailspace.rename_identity(old, new)?;
            println!("renamed {old} -> {new} ({address})");
            println!("historical mail sent as {old} still resolves under {new}");
            let role = mailspace.resolve_identity(new)?;
            crate::local_role_command::report_adopted(&mailspace, &role)?;
        }
    }
    Ok(())
}

fn import_mailspace(command: &MailspaceImportCommand) -> Result<(), VivariumError> {
    let target = Mailspace::discover(command.project.as_deref())?;
    let report = vivi::mailspace::import_mailspace(
        &target,
        &command.from,
        vivi::mailspace::MailspaceImportOptions {
            dry_run: command.dry_run,
        },
    )?;
    if command.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report)
                .map_err(|e| VivariumError::Other(format!("failed to encode JSON: {e}")))?
        );
        return report.binding_conflict_error().map_or(Ok(()), Err);
    }
    let mode = if report.dry_run { "dry run" } else { "applied" };
    println!("mailspace import {mode}");
    println!("source    {}", report.source.display());
    println!("target    {}", report.target.display());
    println!(
        "messages  scanned={} imported={} deduped={}",
        report.scanned_messages, report.imported_messages, report.deduped_messages
    );
    println!(
        "blobs     imported={} deduped={}",
        report.imported_blobs, report.deduped_blobs
    );
    println!(
        "events    imported={} deduped={}",
        report.imported_events, report.deduped_events
    );
    println!(
        "links     imported={} deduped={}",
        report.imported_links, report.deduped_links
    );
    if !report.conflicts.is_empty() {
        println!("conflicts {}", report.conflicts.len());
        for conflict in &report.conflicts {
            println!("  {conflict}");
        }
    }
    report.binding_conflict_error().map_or(Ok(()), Err)
}

fn handle_cycle_command(command: &CycleCommand) -> Result<(), VivariumError> {
    match command {
        CycleCommand::Intake {
            for_identity,
            cursor_file,
            write_cursor,
            json,
            project,
        } => {
            let mailspace = Mailspace::discover(project.as_deref())?;
            let intake =
                mailspace.cycle_intake(for_identity, cursor_file.as_deref(), *write_cursor)?;
            if *json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&intake)
                        .map_err(|e| VivariumError::Other(format!("failed to encode JSON: {e}")))?
                );
            } else {
                print_cycle_intake(&intake);
            }
        }
    }
    Ok(())
}

fn print_cycle_intake(intake: &vivi::mailspace::CycleIntake) {
    println!("cursor {} -> {}", intake.cursor, intake.next_cursor);
    println!("unabsorbed_mail {}", intake.unabsorbed_mail.len());
    println!("completed_tasks {}", intake.completed_tasks.len());
    println!("open_needs {}", intake.open_needs.len());
    println!("open_wants {}", intake.open_wants.len());
}

fn handle_mail_command(command: &MailCommand) -> Result<(), VivariumError> {
    match command {
        MailCommand::Send(command) => send_local_mail(command)?,
        MailCommand::Watch(command) => run_watch(&command.common, "mail")?,
        MailCommand::Reply(command) => reply_local_mail(command)?,
        MailCommand::Deliver {
            path,
            folder,
            project,
        } => deliver_local_mail(path, folder, project.as_deref())?,
        MailCommand::List(command) => list_local_mail(command)?,
        MailCommand::Show {
            handles,
            json,
            project,
        } => {
            let mailspace = Mailspace::discover(project.as_deref())?;
            print_local_messages(&mailspace, handles, *json)?;
        }
        MailCommand::Thread(command) => {
            print_local_thread(command)?;
        }
        MailCommand::Absorb(command) => absorb_record("mail", command)?,
        MailCommand::Dump(command) => {
            let mailspace = Mailspace::discover(command.project.as_deref())?;
            let records = mailspace.dump_mail(mail_dump_request(command))?;
            crate::local_mailspace_dump::write_dump(
                "Vivi Mail Dump",
                &records,
                command.json,
                command.output.as_deref(),
                command.confirm_large,
            )?;
        }
    }
    Ok(())
}

fn print_local_thread(command: &vivi::cli::MailThreadCommand) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(command.project.as_deref())?;
    vivi::mailspace::print_thread(
        &mailspace,
        &command.handle,
        command.infer,
        command.limit,
        command.max_depth,
        command.json,
        false,
    )
}

fn handle_trace_command(command: &TraceCommand) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(command.project.as_deref())?;
    vivi::mailspace::print_trace(
        &mailspace,
        &command.handle,
        command.max_depth,
        command.limit,
        command.json,
    )
}

fn deliver_local_mail(
    path: &std::path::Path,
    folder: &str,
    project: Option<&std::path::Path>,
) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    let data = std::fs::read(path)?;
    let result = mailspace.deliver_raw(&data, folder)?;
    for delivered in &result.delivered {
        println!("delivered {} {}", delivered.identity, delivered.handle);
    }
    if let Some(sent) = &result.sent {
        println!("sent {sent}");
    }
    mailspace.announce_remote(&result.remote);
    Ok(())
}

fn list_local_mail(command: &MailListCommand) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(command.project.as_deref())?;
    crate::local_mail_list::print_mail_list(&mailspace, command, mail_absorb_filter(command.status))
}

pub(crate) fn absorb_record(
    kind: &str,
    command: &vivi::cli::AbsorbCommand,
) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(command.project.as_deref())?;
    let handle = mailspace.absorb(
        &command.for_identity,
        &command.handle,
        command.note.as_deref(),
        kind,
    )?;
    println!("absorbed {handle}");
    Ok(())
}

#[rustfmt::skip]
fn handle_memo_command(command: &MemoCommand) -> Result<(), VivariumError> {
    match command {
        MemoCommand::Save(command) => {
            let mailspace = Mailspace::discover(command.project.as_deref())?;
            let body = vivi::mailspace::read_body_input(
                command.body.as_deref(),
                command.body_file.as_deref(),
            )?;
            let handle = mailspace.save_memo(
                &command.for_identity,
                &command.subject,
                &body,
                &command.tags,
            )?;
            println!("saved {handle}");
        }
        MemoCommand::Absorb(command) => absorb_record("memo", command)?,
        MemoCommand::Delete { handle, for_identity, project } => {
            let mailspace = Mailspace::discover(project.as_deref())?;
            let handle = mailspace.delete_memo(for_identity, handle)?;
            println!("deleted {handle}");
        }
        MemoCommand::List { for_identity, tags, exclude_tags, since, limit, all, json, project } => {
            let mailspace = Mailspace::discover(project.as_deref())?;
            let filter = memo_filter(&mailspace, tags, exclude_tags, since, *limit, *all)?;
            print_memo_list(&mailspace, for_identity, &filter, *json)?;
        }
        MemoCommand::Search { query, for_identity, subject, tags, exclude_tags, since, limit, all, json, project } => {
            let mailspace = Mailspace::discover(project.as_deref())?;
            let filter = memo_filter(&mailspace, tags, exclude_tags, since, *limit, *all)?;
            let memos = mailspace.search_memos_filtered(for_identity, query, *subject, &filter)?;
            print_memo_list_items(&memos, *json)?;
        }
        MemoCommand::Show { handle, json, project } => {
            vivi::mailspace::print_thread(
                &Mailspace::discover(project.as_deref())?,
                handle,
                false,
                50,
                50,
                *json,
                false,
            )?;
        }
        MemoCommand::Dump { for_identity, json, output, confirm_large, project } => {
            dump_memos(for_identity, *json, output.as_deref(), *confirm_large, project.as_deref())?;
        }
    }
    Ok(())
}

/// Build a memo filter from the CLI flags. Hidden tags stay visible only when
/// the caller asked for them: `--all`, or `--tag` naming a hidden tag.
fn memo_filter(
    mailspace: &Mailspace,
    tags: &[String],
    exclude_tags: &[String],
    since: &Option<String>,
    limit: Option<usize>,
    all: bool,
) -> Result<vivi::mailspace::memos::MemoFilter, VivariumError> {
    let hidden = vivi::mailspace::memos::hidden_tags(&mailspace.config.memo);
    let since = since
        .as_deref()
        .map(|raw| vivi::mailspace::memos::since_cutoff(raw, chrono::Utc::now()))
        .transpose()?;
    Ok(vivi::mailspace::memos::MemoFilter {
        tags: tags.to_vec(),
        exclude_tags: exclude_tags.to_vec(),
        since,
        limit,
        include_hidden: all || tags.iter().any(|tag| hidden.contains(tag)),
    })
}

fn print_memo_list(
    mailspace: &Mailspace,
    identity: &str,
    filter: &vivi::mailspace::memos::MemoFilter,
    json: bool,
) -> Result<(), VivariumError> {
    let memos = mailspace.list_memos_filtered(identity, filter)?;
    print_memo_list_items(&memos, json)
}

fn print_memo_list_items(
    memos: &[(StoredMessageView, Vec<String>)],
    json: bool,
) -> Result<(), VivariumError> {
    if json {
        let items: Vec<serde_json::Value> = memos
            .iter()
            .map(|(memo, tags)| {
                serde_json::json!({
                    "handle": memo.handle,
                    "date": memo.date,
                    "tags": tags,
                    "subject": memo.subject,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&items)
                .map_err(|e| VivariumError::Other(format!("failed to encode JSON: {e}")))?
        );
        return Ok(());
    }
    if memos.is_empty() {
        println!("  no memos");
        return Ok(());
    }
    println!("  handle  date  tags  subject");
    for (memo, tags) in memos {
        let tags = if tags.is_empty() {
            "-".to_string()
        } else {
            tags.join(",")
        };
        println!(
            "  {}  {}  {}  {}",
            memo.handle, memo.date, tags, memo.subject
        );
    }
    Ok(())
}

fn dump_memos(
    for_identity: &str,
    json: bool,
    output: Option<&std::path::Path>,
    confirm_large: bool,
    project: Option<&std::path::Path>,
) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    let request = MailDumpRequest {
        folder: "memos".into(),
        kind: Some("memo".into()),
        filters: DumpFilters {
            for_identity: Some(for_identity.into()),
            ..Default::default()
        },
    };
    let records = mailspace.dump_mail(request)?;
    crate::local_mailspace_dump::write_dump("Vivi Memo Dump", &records, json, output, confirm_large)
}

fn send_local_mail(command: &LocalSendCommand) -> Result<(), VivariumError> {
    send_mail(command, "inbox", "mail", "delivered")
}

fn handle_task_command(command: &TaskCommand) -> Result<(), VivariumError> {
    match command {
        TaskCommand::Send(command) => send_task(command)?,
        TaskCommand::From(command) => task_from_source(command)?,
        TaskCommand::Watch(command) => run_watch(&command.common, "task")?,
        TaskCommand::List { .. } => run_task_list(command)?,
        TaskCommand::Show {
            handle,
            json,
            latest,
            project,
        } => show_task(handle, *json, *latest, project.as_deref())?,
        TaskCommand::Dump(command) => crate::local_work_command::dump_tasks(command)?,
        TaskCommand::Absorb(command) => absorb_record("task", command)?,
        TaskCommand::Done {
            handle,
            for_identity,
            note,
            verdict,
            repo,
            tip,
            project,
        } => done_task(
            handle,
            for_identity,
            note.as_deref(),
            verdict.as_deref(),
            repo,
            tip,
            project.as_deref(),
        )?,
        TaskCommand::Reopen {
            handle,
            for_identity,
            note,
            project,
        } => reopen_task(handle, for_identity, note.as_deref(), project.as_deref())?,
    }
    Ok(())
}

fn done_task(
    handle: &str,
    for_identity: &str,
    note: Option<&str>,
    verdict: Option<&str>,
    repo: &[String],
    tip: &[String],
    project: Option<&std::path::Path>,
) -> Result<(), VivariumError> {
    move_task(
        handle,
        for_identity,
        note,
        project,
        "done",
        verdict,
        repo,
        tip,
    )?;
    // Task threads carry the pool's result replies, not attempt records.
    crate::local_work_command::print_close_timing(project, handle, true);
    Ok(())
}

fn reopen_task(
    handle: &str,
    for_identity: &str,
    note: Option<&str>,
    project: Option<&std::path::Path>,
) -> Result<(), VivariumError> {
    move_task(handle, for_identity, note, project, "tasks", None, &[], &[])
}

fn run_task_list(command: &TaskCommand) -> Result<(), VivariumError> {
    let TaskCommand::List {
        for_identity,
        held,
        from,
        to,
        status,
        blocked,
        blocking,
        json,
        project,
    } = command
    else {
        return Err(VivariumError::Message(
            "internal: run_task_list requires task list".into(),
        ));
    };
    let held = crate::local_work_list::held_scope(for_identity.as_deref(), *held)?;
    if *blocked || blocking.is_some() {
        let Some(for_identity) = for_identity.as_deref() else {
            return Err(VivariumError::Message(
                "task list --blocked/--blocking requires --for".into(),
            ));
        };
        return list_tasks_with_deps(
            for_identity,
            *blocked,
            blocking.as_deref(),
            *json,
            project.as_deref(),
        );
    }
    list_tasks(
        for_identity.as_deref(),
        from.as_deref(),
        to.as_deref(),
        status,
        *json,
        project.as_deref(),
        held.as_ref(),
    )
}

fn list_tasks(
    for_identity: Option<&str>,
    from: Option<&str>,
    to: Option<&str>,
    status: &vivi::cli::TaskStatus,
    json: bool,
    project: Option<&std::path::Path>,
    held: Option<&crate::local_work_list::HeldScope>,
) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    let roles = match status {
        vivi::cli::TaskStatus::Open => vec!["tasks", "sent"],
        vivi::cli::TaskStatus::Done => vec!["done"],
        vivi::cli::TaskStatus::All => vec!["tasks", "sent", "done"],
    };
    if let Some(scope) = held {
        return crate::local_work_list::print_held_work_lists(
            &mailspace,
            scope,
            &roles,
            "task",
            from,
            to,
            &[],
            json,
            false,
        );
    }
    crate::local_work_list::print_work_lists(
        &mailspace,
        for_identity,
        &roles,
        "task",
        from,
        to,
        &[],
        json,
        false,
    )
}

fn list_tasks_with_deps(
    for_identity: &str,
    blocked: bool,
    blocking: Option<&str>,
    json: bool,
    project: Option<&std::path::Path>,
) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    let tasks = mailspace.list_tasks_with_deps(for_identity, blocked, blocking)?;
    crate::local_work_list::print_task_list(&mailspace, &tasks, json)
}

fn show_task(
    handle: &str,
    json: bool,
    latest: bool,
    project: Option<&std::path::Path>,
) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    vivi::mailspace::print_thread(&mailspace, handle, false, 50, 50, json, latest)?;
    Ok(())
}

fn task_from_source(command: &vivi::cli::TaskFromCommand) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(command.project.as_deref())?;
    let result = mailspace.task_from_source(SourceTaskRequest {
        source_handle: command.handle.clone(),
        actor: command.for_identity.clone(),
        to: command.to.clone(),
        cc: command.cc.clone(),
        subject: command.subject.clone(),
        body: vivi::mailspace::read_body_input(
            command.body.as_deref(),
            command.body_file.as_deref(),
        )?,
    })?;
    for delivered in &result.delivered {
        println!("created {} {}", delivered.identity, delivered.handle);
    }
    println!("source {} {}", result.source_kind, result.source_handle);
    println!("sent {}", result.sent);
    print_send_node_lines(&result.delivered, &result.sent);
    mailspace.announce_remote(&result.remote);
    Ok(())
}

/// Print one `node <handle>` line per backlog node a work send minted: the
/// delivered copies hold the nodes when any local recipient exists,
/// otherwise the sent copy does (mirrors `attach_sent_work`). Printed after
/// the `sent` line so every pre-existing line keeps its position.
pub(crate) fn print_send_node_lines(delivered: &[DeliveredMessage], sent: &str) {
    if delivered.is_empty() {
        println!("node {sent}");
        return;
    }
    for delivered in delivered {
        println!("node {}", delivered.handle);
    }
}

fn send_task(command: &TaskSendCommand) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(command.send.project.as_deref())?;
    let body = vivi::mailspace::read_body_input(
        command.send.body.as_deref(),
        command.send.body_file.as_deref(),
    )?;
    let lacks_clause = !vivi::mailspace::body_has_labeled_clause(&body, "done_when");
    let result = mailspace.send(SendRequest {
        from: command.send.from.clone(),
        to: command.send.to.clone(),
        cc: command.send.cc.clone(),
        bcc: command.send.bcc.clone(),
        subject: command.send.subject.clone(),
        body,
        role: "tasks".into(),
        kind: Some("task".into()),
        reply_to: command.send.reply_to.clone(),
        depends_on: command.depends_on.clone(),
    })?;
    for delivered in &result.delivered {
        println!("created {} {}", delivered.identity, delivered.handle);
    }
    println!("sent {}", result.sent);
    print_send_node_lines(&result.delivered, &result.sent);
    mailspace.announce_remote(&result.remote);
    if lacks_clause {
        eprintln!(
            "note: body declares no 'done_when:' clause; vivi step will report this task as \
             no_done_when"
        );
    }
    Ok(())
}

fn send_mail(
    command: &LocalSendCommand,
    role: &str,
    kind: &str,
    delivered_label: &str,
) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(command.project.as_deref())?;
    let result = mailspace.send(SendRequest {
        from: command.from.clone(),
        to: command.to.clone(),
        cc: command.cc.clone(),
        bcc: command.bcc.clone(),
        subject: command.subject.clone(),
        body: vivi::mailspace::read_body_input(
            command.body.as_deref(),
            command.body_file.as_deref(),
        )?,
        role: role.into(),
        kind: Some(kind.into()),
        reply_to: command.reply_to.clone(),
        depends_on: Vec::new(),
    })?;
    for delivered in result.delivered {
        println!(
            "{delivered_label} {} {}",
            delivered.identity, delivered.handle
        );
    }
    println!("sent {}", result.sent);
    mailspace.announce_remote(&result.remote);
    Ok(())
}

fn reply_local_mail(command: &MailReplyCommand) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(command.project.as_deref())?;
    let result = mailspace.reply(
        &command.handle,
        &command.from,
        command.to.clone(),
        command.cc.clone(),
        command.subject.clone(),
        vivi::mailspace::read_body_input(command.body.as_deref(), command.body_file.as_deref())?,
    )?;
    for delivered in result.delivered {
        println!("replied {} {}", delivered.identity, delivered.handle);
    }
    println!("sent {}", result.sent);
    mailspace.announce_remote(&result.remote);
    Ok(())
}

pub(crate) fn run_watch(
    command: &vivi::cli::WatchCommon,
    kinds: &str,
) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(command.project.as_deref())?;
    let request = MailspaceWatchRequest {
        for_identity: command.for_identity.clone(),
        kinds: kinds.to_string(),
        events: command.events.clone(),
        statuses: command.statuses.clone(),
        match_from: command.match_from.clone(),
        match_subject_prefix: command.match_subject_prefix.clone(),
        handle: command.handle.clone(),
        until_count: command.until_count,
        timeout: command.timeout.clone(),
        once: command.once,
        since: command.since.clone(),
        cursor_file: command
            .cursor_file
            .clone()
            .or_else(|| command.watermark_file.clone()),
        write_cursor: command.write_cursor || command.write_watermark,
        poll_interval: command.poll_interval.clone(),
        json: command.json,
    };
    vivi::mailspace::run_watch(&mailspace, request)
}

#[allow(clippy::too_many_arguments)]
fn move_task(
    handle: &str,
    for_identity: &str,
    note: Option<&str>,
    project: Option<&std::path::Path>,
    role: &str,
    verdict: Option<&str>,
    repo: &[String],
    tip: &[String],
) -> Result<(), VivariumError> {
    if repo.len() != tip.len() {
        return Err(VivariumError::Other(
            "--repo and --tip must be provided in matching pairs".into(),
        ));
    }
    let mailspace = Mailspace::discover(project)?;
    let moved =
        mailspace.move_task_outcome(for_identity, handle, role, note, verdict, repo, tip)?;
    let verb = if role == "done" { "done" } else { "reopened" };
    crate::local_work_command::print_move_receipt(&moved, verb);
    Ok(())
}

fn print_local_messages(
    mailspace: &Mailspace,
    handles: &[String],
    as_json: bool,
) -> Result<(), VivariumError> {
    let storage = mailspace.storage()?;
    if as_json {
        let mut messages = Vec::new();
        for handle in handles {
            let resolved = storage.resolve_message_token(handle)?;
            let data = storage.read_message(&resolved)?;
            let display_handle = storage.display_handle(&resolved)?;
            messages.push(message::to_json_message(&display_handle, &data)?);
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&messages)
                .map_err(|e| VivariumError::Other(format!("failed to encode JSON: {e}")))?
        );
        return Ok(());
    }
    for (i, handle) in handles.iter().enumerate() {
        if i > 0 {
            println!("\n---\n");
        }
        let data = storage.read_message(handle)?;
        println!("{}", message::render_message(&data)?);
    }
    Ok(())
}

fn mail_dump_request(command: &MailDumpCommand) -> MailDumpRequest {
    MailDumpRequest {
        folder: command.folder.clone(),
        kind: Some("mail".into()),
        filters: dump_filters(command),
    }
}

fn mail_absorb_filter(status: MailAbsorbStatus) -> MailAbsorbFilter {
    match status {
        MailAbsorbStatus::All => MailAbsorbFilter::All,
        MailAbsorbStatus::Absorbed => MailAbsorbFilter::Absorbed,
        MailAbsorbStatus::Unabsorbed => MailAbsorbFilter::Unabsorbed,
    }
}

fn dump_filters(command: &MailDumpCommand) -> DumpFilters {
    DumpFilters {
        for_identity: command.for_identity.clone(),
        from: command.from.clone(),
        to: command.to.clone(),
        participant: command.participant.clone(),
        subject: command.subject.clone(),
        body: command.body.clone(),
        since: command.since.clone(),
        before: command.before.clone(),
        absorb_status: mail_absorb_filter(command.status),
        absorbed_by: command.absorbed_by.clone(),
    }
}
