use vivi::VivariumError;
use vivi::cli::{
    LocalSendCommand, NeedCommand, TaskDumpCommand, TaskDumpStatusArg, TaskStatus, WantCommand,
    WantStatus,
};
use vivi::mailspace::{
    DumpFilters, Mailspace, SendRequest, TaskDumpRequest, WantListOptions, WantMetadataUpdate,
};

pub(crate) fn handle_need_command(command: &NeedCommand) -> Result<(), VivariumError> {
    match command {
        NeedCommand::Send(command) => send_local_item(
            &command.send,
            "needs",
            "need",
            "created",
            &command.depends_on,
        )?,
        NeedCommand::Watch(command) => {
            crate::local_mailspace_command::run_watch(&command.common, "need")?;
        }
        NeedCommand::List {
            for_identity,
            from,
            to,
            status,
            marks,
            json,
            project,
        } => {
            vivi::mailspace::validate_marks_filter(marks)?;
            let mailspace = Mailspace::discover(project.as_deref())?;
            crate::local_work_list::print_work_lists(
                &mailspace,
                for_identity.as_deref(),
                &status_roles(status, "needs"),
                "need",
                from.as_deref(),
                to.as_deref(),
                marks,
                *json,
            )?;
        }
        NeedCommand::Show {
            handle,
            json,
            project,
        } => show_need(handle, *json, project.as_deref())?,
        NeedCommand::Dump(command) => dump_work_items(command, "needs", "need", "Vivi Need Dump")?,
        NeedCommand::Absorb(command) => {
            crate::local_mailspace_command::absorb_record("need", command)?;
        }
        NeedCommand::Done {
            handle,
            for_identity,
            note,
            project,
        } => move_item(
            handle,
            for_identity,
            note.as_ref(),
            project.as_deref(),
            "done",
            "need done",
            "done",
        )?,
        NeedCommand::Reopen {
            handle,
            for_identity,
            note,
            project,
        } => move_item(
            handle,
            for_identity,
            note.as_ref(),
            project.as_deref(),
            "needs",
            "need reopen",
            "reopened",
        )?,
        NeedCommand::Bind {
            handle,
            units,
            project,
        } => {
            let mailspace = Mailspace::discover(project.as_deref())?;
            mailspace.backlog_bind_units(handle, units)?;
            println!("bound {handle}");
        }
        NeedCommand::Mark(command) => mark_item(command, "need", true)?,
        NeedCommand::Unmark(command) => mark_item(command, "need", false)?,
    }
    Ok(())
}

pub(crate) fn handle_want_command(command: &WantCommand) -> Result<(), VivariumError> {
    match command {
        WantCommand::Send(command) => send_local_item(
            &command.send,
            "wants",
            "want",
            "created",
            &command.depends_on,
        )?,
        WantCommand::Watch(command) => {
            crate::local_mailspace_command::run_watch(&command.common, "want")?;
        }
        WantCommand::List {
            for_identity,
            from,
            to,
            status,
            repo,
            lane,
            marks,
            sort,
            json,
            project,
        } => list_wants(
            for_identity.as_deref(),
            from.as_deref(),
            to.as_deref(),
            status,
            WantListOptions {
                repo: repo.clone(),
                lane: lane.clone(),
                sort: sort.clone(),
            },
            marks,
            *json,
            project.as_deref(),
        )?,
        WantCommand::Show {
            handle,
            json,
            project,
        } => show_local_message(handle, *json, project.as_deref())?,
        WantCommand::Dump(command) => dump_work_items(command, "wants", "want", "Vivi Want Dump")?,
        WantCommand::Absorb(command) => {
            crate::local_mailspace_command::absorb_record("want", command)?;
        }
        WantCommand::SetPriority { .. } => set_want_priority(command)?,
        WantCommand::Promote {
            handle,
            for_identity,
            note,
            project,
        } => promote_want(handle, for_identity, note.as_ref(), project.as_deref())?,
        WantCommand::Done {
            handle,
            for_identity,
            note,
            project,
        } => close_want_done(handle, for_identity, note.as_ref(), project.as_deref())?,
        WantCommand::Drop {
            handle,
            for_identity,
            note,
            project,
        } => close_want_drop(handle, for_identity, note.as_ref(), project.as_deref())?,
        WantCommand::Mark(command) => mark_item(command, "want", true)?,
        WantCommand::Unmark(command) => mark_item(command, "want", false)?,
    }
    Ok(())
}

fn mark_item(command: &vivi::cli::MarkCommand, kind: &str, add: bool) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(command.project.as_deref())?;
    let writes = mailspace.set_marks(
        &command.for_identity,
        &command.handle,
        kind,
        add,
        &command.slugs,
    )?;
    for write in writes {
        print_mark(&write.handle, &write.slug, add, write.changed);
    }
    Ok(())
}

fn print_mark(handle: &str, slug: &str, add: bool, changed: bool) {
    if add && changed {
        println!("marked {handle} {slug}");
    } else if add {
        println!("{handle} already marked {slug}");
    } else if changed {
        println!("unmarked {handle} {slug}");
    } else {
        println!("{handle} already unmarked {slug}");
    }
}

fn promote_want(
    handle: &str,
    for_identity: &str,
    note: Option<&String>,
    project: Option<&std::path::Path>,
) -> Result<(), VivariumError> {
    move_item(
        handle,
        for_identity,
        note,
        project,
        "needs",
        "want promote",
        "promoted",
    )
}

fn set_want_priority(command: &WantCommand) -> Result<(), VivariumError> {
    let WantCommand::SetPriority {
        handle,
        for_identity,
        priority,
        rank,
        repo,
        lane,
        blocks_claim,
        reason,
        project,
    } = command
    else {
        unreachable!();
    };
    let mailspace = Mailspace::discover(project.as_deref())?;
    let handle = mailspace.set_want_metadata(
        for_identity,
        handle,
        WantMetadataUpdate {
            priority: priority.clone(),
            rank: *rank,
            repo: repo.clone(),
            lane: lane.clone(),
            blocks_claim: blocks_claim.clone(),
            reason: reason.clone(),
        },
    )?;
    println!("updated {handle}");
    Ok(())
}

fn list_wants(
    for_identity: Option<&str>,
    from: Option<&str>,
    to: Option<&str>,
    status: &WantStatus,
    options: WantListOptions,
    marks: &[String],
    json: bool,
    project: Option<&std::path::Path>,
) -> Result<(), VivariumError> {
    vivi::mailspace::validate_marks_filter(marks)?;
    let mailspace = Mailspace::discover(project)?;
    let from = crate::local_mail_list::resolve_list_header(&mailspace, from);
    let to = crate::local_mail_list::resolve_list_header(&mailspace, to);
    let records = mailspace
        .list_wants_with_metadata(for_identity, want_status_roles(status), options)?
        .into_iter()
        .filter(|record| {
            crate::local_mail_list::headers_match(
                &record.from,
                &record.to,
                "",
                from.as_ref(),
                to.as_ref(),
            ) && vivi::mailspace::marks_include(&record.marks, marks)
        })
        .collect::<Vec<_>>();
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&records)
                .map_err(|e| VivariumError::Other(format!("failed to encode JSON: {e}")))?
        );
        return Ok(());
    }
    if records.is_empty() {
        println!("  no wants");
        return Ok(());
    }
    println!("  handle  status  priority  rank  repo  lane  subject");
    for item in &records {
        println!(
            "  {}  {}  {}  {}  {}  {}  {}",
            item.handle,
            item.status,
            item.metadata.get("priority").map_or("-", String::as_str),
            item.metadata.get("rank").map_or("-", String::as_str),
            item.metadata.get("repo").map_or("-", String::as_str),
            item.metadata.get("lane").map_or("-", String::as_str),
            item.subject
        );
    }
    Ok(())
}

fn close_want(
    handle: &str,
    for_identity: &str,
    note: Option<&String>,
    project: Option<&std::path::Path>,
    command: &str,
    verb: &str,
) -> Result<(), VivariumError> {
    move_item(handle, for_identity, note, project, "done", command, verb)
}

fn close_want_done(
    handle: &str,
    for_identity: &str,
    note: Option<&String>,
    project: Option<&std::path::Path>,
) -> Result<(), VivariumError> {
    close_want(handle, for_identity, note, project, "want done", "done")
}

fn close_want_drop(
    handle: &str,
    for_identity: &str,
    note: Option<&String>,
    project: Option<&std::path::Path>,
) -> Result<(), VivariumError> {
    close_want(handle, for_identity, note, project, "want drop", "dropped")
}

pub(crate) fn dump_tasks(command: &TaskDumpCommand) -> Result<(), VivariumError> {
    dump_work_items(command, "tasks", "task", "Vivi Task Dump")
}

fn dump_work_items(
    command: &TaskDumpCommand,
    open_role: &str,
    kind: &str,
    title: &str,
) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(command.project.as_deref())?;
    let records = mailspace.dump_tasks(work_dump_request(command, open_role, kind))?;
    crate::local_mailspace_dump::write_dump(
        title,
        &records,
        command.json,
        command.output.as_deref(),
        command.confirm_large,
    )
}

fn send_local_item(
    command: &LocalSendCommand,
    role: &str,
    kind: &str,
    verb: &str,
    depends_on: &[String],
) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(command.project.as_deref())?;
    let body =
        vivi::mailspace::read_body_input(command.body.as_deref(), command.body_file.as_deref())?;
    // Wants are parked by design; only task/need bodies feed step clauses.
    let lacks_clause = matches!(kind, "task" | "need")
        && !vivi::mailspace::body_has_labeled_clause(&body, "done_when");
    let result = mailspace.send(SendRequest {
        from: command.from.clone(),
        to: command.to.clone(),
        cc: command.cc.clone(),
        bcc: command.bcc.clone(),
        subject: command.subject.clone(),
        body,
        role: role.into(),
        kind: Some(kind.into()),
        reply_to: command.reply_to.clone(),
        depends_on: depends_on.to_vec(),
    })?;
    for delivered in &result.delivered {
        println!("{verb} {} {}", delivered.identity, delivered.handle);
    }
    println!("sent {}", result.sent);
    mailspace.announce_remote(&result.remote);
    if lacks_clause {
        eprintln!(
            "note: body declares no 'done_when:' clause; vivi step will report this {kind} as \
             no_done_when"
        );
    }
    Ok(())
}

fn move_item(
    handle: &str,
    for_identity: &str,
    note: Option<&String>,
    project: Option<&std::path::Path>,
    role: &str,
    command: &str,
    verb: &str,
) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    let handle = mailspace.move_item(
        for_identity,
        handle,
        role,
        note.map(String::as_str),
        command,
        None,
    )?;
    println!("{verb} {handle}");
    Ok(())
}

fn show_local_message(
    handle: &str,
    json: bool,
    project: Option<&std::path::Path>,
) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    vivi::mailspace::print_thread(&mailspace, handle, false, 50, 50, json)
}

/// Need show: the thread plus a bound-units block, so a need's lowering
/// state (which units exist, which have settled) is answerable by reading.
fn show_need(
    handle: &str,
    json: bool,
    project: Option<&std::path::Path>,
) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    vivi::mailspace::print_thread(&mailspace, handle, false, 50, 50, json)?;
    if json {
        return Ok(());
    }
    let units = mailspace.backlog_bound_units(handle)?;
    if units.is_empty() {
        return Ok(());
    }
    let rendered = units
        .iter()
        .map(|unit| format!("{}:{}", unit.handle, unit.state))
        .collect::<Vec<_>>()
        .join("  ");
    println!("  units    {rendered}");
    Ok(())
}

fn status_roles<'a>(status: &TaskStatus, open_role: &'a str) -> Vec<&'a str> {
    match status {
        TaskStatus::Open => vec![open_role],
        TaskStatus::Done => vec!["done"],
        TaskStatus::All => vec![open_role, "done"],
    }
}

fn want_status_roles(status: &WantStatus) -> &'static [&'static str] {
    match status {
        WantStatus::Open => &["wants"],
        WantStatus::Done => &["done"],
        WantStatus::All => &["wants", "done"],
    }
}

fn work_dump_request(command: &TaskDumpCommand, open_role: &str, kind: &str) -> TaskDumpRequest {
    TaskDumpRequest {
        status: match command.status {
            TaskDumpStatusArg::Open => vivi::mailspace::TaskDumpStatus::Open,
            TaskDumpStatusArg::Done => vivi::mailspace::TaskDumpStatus::Done,
            TaskDumpStatusArg::All => vivi::mailspace::TaskDumpStatus::All,
        },
        open_role: open_role.into(),
        kind: kind.into(),
        filters: task_dump_filters(command),
    }
}

fn task_dump_filters(command: &TaskDumpCommand) -> DumpFilters {
    DumpFilters {
        for_identity: command.for_identity.clone(),
        from: command.from.clone(),
        to: command.to.clone(),
        participant: command.participant.clone(),
        subject: command.subject.clone(),
        body: command.body.clone(),
        since: command.since.clone(),
        before: command.before.clone(),
        ..Default::default()
    }
}
