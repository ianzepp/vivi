use vivi::VivariumError;
use vivi::cli::{
    LocalSendCommand, NeedCommand, TaskDumpCommand, TaskDumpStatusArg, TaskStatus, WantCommand,
    WantStatus,
};
use vivi::letter::sugar::{self, Sugar};
use vivi::mailspace::{
    DeliveryResult, DumpFilters, Mailspace, PumpRecord, Recipient, SendRequest, TaskDumpRequest,
    WantListOptions, WantMetadataUpdate,
};

pub(crate) fn handle_need_command(command: &NeedCommand) -> Result<(), VivariumError> {
    match command {
        NeedCommand::Send(command) => send_need(command)?,
        NeedCommand::Watch(command) => {
            crate::local_mailspace_command::run_watch(&command.common, "need")?;
        }
        NeedCommand::List {
            for_identity,
            held,
            from,
            to,
            status,
            marks,
            json,
            project,
        } => {
            vivi::mailspace::validate_marks_filter(marks)?;
            let mailspace = Mailspace::discover(project.as_deref())?;
            let held = crate::local_work_list::held_scope(for_identity.as_deref(), *held)?;
            if let Some(scope) = &held {
                crate::local_work_list::print_held_work_lists(
                    &mailspace,
                    scope,
                    &status_roles(status, "needs"),
                    "need",
                    from.as_deref(),
                    to.as_deref(),
                    marks,
                    *json,
                )?;
            } else {
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
        }
        NeedCommand::Show {
            handle,
            json,
            latest,
            newest,
            project,
        } => show_need(handle, *json, *latest, *newest, project.as_deref())?,
        NeedCommand::Attempts {
            handle,
            json,
            project,
        } => print_need_attempts(handle, *json, project.as_deref())?,
        NeedCommand::Dump(command) => dump_work_items(command, "needs", "need", "Vivi Need Dump")?,
        NeedCommand::Absorb(command) => {
            crate::local_mailspace_command::absorb_record("need", command)?;
        }
        NeedCommand::Done {
            handle,
            for_identity,
            note,
            branch,
            commit,
            project,
        } => {
            move_item_with_close_location(
                handle,
                for_identity,
                note.as_ref(),
                branch.as_deref(),
                commit.as_deref(),
                project.as_deref(),
            )?;
            print_close_timing(project.as_deref(), handle, false);
        }
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
        NeedCommand::Assign {
            handles,
            to,
            for_identity,
            project,
        } => {
            let mailspace = Mailspace::discover(project.as_deref())?;
            let remote = match mailspace.classify_recipient(to)? {
                Recipient::Remote(address) => Some(address),
                Recipient::Local(_) => None,
            };
            for handle in handles {
                let event = mailspace.assign_need(for_identity, handle, to)?;
                println!("assigned {handle} to {to}; event {event}");
            }
            if let Some(remote) = remote {
                mailspace.announce_remote(&[remote]);
            }
        }
        NeedCommand::Withdraw {
            handles,
            for_identity,
            project,
        } => {
            let mailspace = Mailspace::discover(project.as_deref())?;
            for handle in handles {
                let event = mailspace.withdraw_need(for_identity, handle)?;
                println!("withdrawn {handle}; event {event}");
            }
        }
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
        NeedCommand::Depend(command) => depend_item(command, "need")?,
    }
    Ok(())
}

pub(crate) fn handle_want_command(command: &WantCommand) -> Result<(), VivariumError> {
    match command {
        WantCommand::Send(command) => {
            send_local_item(&command.send, "wants", "want", "created", &command.sugar())?;
        }
        WantCommand::Watch(command) => {
            crate::local_mailspace_command::run_watch(&command.common, "want")?;
        }
        WantCommand::List {
            for_identity,
            held,
            from,
            to,
            status,
            repo,
            lane,
            marks,
            sort,
            json,
            project,
        } => {
            let held = crate::local_work_list::held_scope(for_identity.as_deref(), *held)?;
            if let Some(scope) = &held {
                vivi::mailspace::validate_marks_filter(marks)?;
                let mailspace = Mailspace::discover(project.as_deref())?;
                crate::local_work_list::print_held_work_lists(
                    &mailspace,
                    scope,
                    want_status_roles(status),
                    "want",
                    from.as_deref(),
                    to.as_deref(),
                    marks,
                    *json,
                )?;
            } else {
                list_wants(
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
                )?;
            }
        }
        WantCommand::Show {
            handle,
            json,
            latest,
            project,
        } => crate::local_want_show::show_want(handle, *json, *latest, project.as_deref())?,
        WantCommand::Dump(command) => dump_work_items(command, "wants", "want", "Vivi Want Dump")?,
        WantCommand::Absorb(command) => {
            crate::local_mailspace_command::absorb_record("want", command)?;
        }
        WantCommand::SetPriority {
            handle,
            for_identity,
            priority,
            rank,
            repo,
            lane,
            blocks_claim,
            reason,
            project,
        } => set_want_priority(
            handle,
            for_identity,
            WantMetadataUpdate {
                priority: priority.clone(),
                rank: *rank,
                repo: repo.clone(),
                lane: lane.clone(),
                blocks_claim: blocks_claim.clone(),
                reason: reason.clone(),
            },
            project.as_deref(),
        )?,
        WantCommand::Done {
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
            "want done",
            "done",
        )?,
        WantCommand::Drop {
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
            "want drop",
            "dropped",
        )?,
        WantCommand::Mark(command) => mark_item(command, "want", true)?,
        WantCommand::Unmark(command) => mark_item(command, "want", false)?,
        WantCommand::Depend(command) => depend_item(command, "want")?,
    }
    Ok(())
}

/// `need|task|want depend`: add and remove prerequisites after the send,
/// one line per dependency.
pub(crate) fn depend_item(
    command: &vivi::cli::DependCommand,
    kind: &str,
) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(command.project.as_deref())?;
    let writes = mailspace.set_dependencies(
        &command.for_identity,
        &command.handle,
        kind,
        &command.add,
        &command.remove,
    )?;
    for write in writes {
        let handle = &command.handle;
        let dep = &write.handle;
        match (write.added, write.changed) {
            (true, true) => println!("{handle} now depends on {dep}"),
            (true, false) => println!("{handle} already depends on {dep}"),
            (false, true) => println!("{handle} no longer depends on {dep}"),
            (false, false) => println!("{handle} does not depend on {dep}"),
        }
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

fn set_want_priority(
    handle: &str,
    for_identity: &str,
    update: WantMetadataUpdate,
    project: Option<&std::path::Path>,
) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    let handle = mailspace.set_want_metadata(for_identity, handle, update)?;
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
        .list_wants_by_state(for_identity, want_state_filter(status), options)?
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

/// Send a need, then write any `--mark` slugs as the sending identity.
///
/// The slugs are validated before anything is sent. The need letter goes
/// first and the mark letters second, so a mark never travels ahead of its
/// need. With `--dispatch`, the filed need is chained into a pump record
/// after the marks; an empty mark list never bypasses the dispatch.
fn send_need(command: &vivi::cli::NeedSendCommand) -> Result<(), VivariumError> {
    vivi::mailspace::validate_marks_filter(&command.marks)?;
    let result = send_local_item_sent(&command.send, "needs", "need", "created", &command.sugar())?;
    if command.marks.is_empty() && !command.dispatch {
        return Ok(());
    }
    let mailspace = match Mailspace::discover(command.send.project.as_deref()) {
        Ok(mailspace) => mailspace,
        // No marks: this rediscovery happened only to dispatch.
        Err(error) if command.marks.is_empty() => {
            return Err(dispatch_boundary(error, &result.sent));
        }
        Err(error) => return Err(error),
    };
    write_send_marks(&mailspace, &command.send.from, &result, &command.marks)?;
    if command.dispatch {
        let record = dispatch_filed_need(&mailspace, &result)?;
        println!("pump record {} for need {}", record.handle, record.need);
    }
    Ok(())
}

/// The mark writes of a sent need. The sender's own copy first: it is the
/// copy whose mark letters travel. Then each local recipient's copy, which
/// is the one `need list` shows.
fn write_send_marks(
    mailspace: &Mailspace,
    from: &str,
    result: &DeliveryResult,
    marks: &[String],
) -> Result<(), VivariumError> {
    if marks.is_empty() {
        return Ok(());
    }
    let mut targets = vec![(from, result.sent.as_str())];
    targets.extend(
        result
            .delivered
            .iter()
            .map(|copy| (copy.identity.as_str(), copy.handle.as_str())),
    );
    for (identity, handle) in targets {
        let writes = mailspace.set_marks(identity, handle, "need", true, marks)?;
        for write in writes {
            print_mark(&write.handle, &write.slug, true, write.changed);
        }
    }
    Ok(())
}

/// Chain a just-filed need into a pump record (`need send --dispatch`).
///
/// This post-filing shortcut requires the pump configuration: the configured
/// `reply_from` must resolve to a role of this mailspace, and that role must
/// have received a delivered copy of the need — by name, alias, local
/// address or Cc. The pump sender and body keep their `vivi pump send`
/// defaults, and the duplicate and actionable checks stay with `pump_send`.
fn dispatch_filed_need(
    mailspace: &Mailspace,
    result: &DeliveryResult,
) -> Result<PumpRecord, VivariumError> {
    dispatch_filed_need_checked(mailspace, result)
        .map_err(|error| dispatch_boundary(error, &result.sent))
}

fn dispatch_filed_need_checked(
    mailspace: &Mailspace,
    result: &DeliveryResult,
) -> Result<PumpRecord, VivariumError> {
    let pump = mailspace.config.pump.as_ref().ok_or_else(|| {
        VivariumError::Config(String::from(
            "no [pump] table in .vivi/mailspace.toml; --dispatch needs a configured pump",
        ))
    })?;
    let reply_from = mailspace.resolve_identity(&pump.reply_from)?;
    if !result
        .delivered
        .iter()
        .any(|copy| copy.identity == reply_from)
    {
        return Err(VivariumError::Message(format!(
            "no delivered copy of the filed need for {reply_from}; the dispatch pump must be a recipient"
        )));
    }
    mailspace.pump_send(None, &result.sent, None)
}

/// Wrap a dispatch-boundary failure with the dispatch phase, the filed sent
/// handle and the underlying reason.
fn dispatch_boundary(error: VivariumError, sent: &str) -> VivariumError {
    VivariumError::Other(format!("dispatch of filed need {sent} failed: {error}"))
}

fn send_local_item(
    command: &LocalSendCommand,
    role: &str,
    kind: &str,
    verb: &str,
    sugar: &Sugar,
) -> Result<(), VivariumError> {
    send_local_item_sent(command, role, kind, verb, sugar).map(|_| ())
}

/// Send one local item and return the handle of the sender's own copy.
fn send_local_item_sent(
    command: &LocalSendCommand,
    role: &str,
    kind: &str,
    verb: &str,
    sugar: &Sugar,
) -> Result<vivi::mailspace::DeliveryResult, VivariumError> {
    let mailspace = Mailspace::discover(command.project.as_deref())?;
    let body =
        vivi::mailspace::read_body_input(command.body.as_deref(), command.body_file.as_deref())?;
    let sugared =
        sugar::apply(kind, &body, sugar).map_err(|err| VivariumError::Message(err.to_string()))?;
    let body = sugared.body;
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
    })?;
    for delivered in &result.delivered {
        println!("{verb} {} {}", delivered.identity, delivered.handle);
    }
    println!("sent {}", result.sent);
    crate::local_mailspace_command::print_send_node_lines(&result.delivered, &result.sent);
    mailspace.announce_remote(&result.remote);
    Ok(result)
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
    let moved = mailspace.move_item_outcome(
        for_identity,
        handle,
        role,
        note.map(String::as_str),
        command,
        None,
    )?;
    print_move_receipt(&moved, verb);
    Ok(())
}

fn move_item_with_close_location(
    handle: &str,
    for_identity: &str,
    note: Option<&String>,
    branch: Option<&str>,
    commit: Option<&str>,
    project: Option<&std::path::Path>,
) -> Result<(), VivariumError> {
    if branch.is_some() != commit.is_some() {
        return Err(VivariumError::Other(
            "--branch and --commit must be provided together".into(),
        ));
    }
    let mailspace = Mailspace::discover(project)?;
    let metadata = match (branch, commit) {
        (Some(branch), Some(commit)) => std::collections::BTreeMap::from([
            ("branch".into(), branch.to_string()),
            ("commit".into(), commit.to_string()),
        ]),
        _ => std::collections::BTreeMap::new(),
    };
    let moved = mailspace.move_item_outcome(
        for_identity,
        handle,
        "done",
        note.map(String::as_str),
        "need done",
        (!metadata.is_empty()).then_some(&metadata),
    )?;
    print_move_receipt(&moved, "done");
    Ok(())
}

#[cfg(test)]
#[path = "local_work_command_test.rs"]
mod tests;

/// The `timing:` close line `need done` and `task done` print: open-to-done,
/// attempts, summed agent time and the slowest attempt, read from the
/// closed item's thread. A missing or unreadable thread prints nothing, so
/// the close itself never fails.
pub(crate) fn print_close_timing(project: Option<&std::path::Path>, handle: &str, receipts: bool) {
    if let Some((opened, attempts)) = read_close_thread(project, handle, receipts) {
        let now = chrono::Utc::now().timestamp().unsigned_abs();
        println!(
            "{}",
            vivi::pump::attempt_read::timing_summary(opened, &attempts, now)
        );
    }
}

/// The thread's open epoch and attempt fields for [`print_close_timing`],
/// or `None` when the mailspace or the thread cannot be read.
fn read_close_thread(
    project: Option<&std::path::Path>,
    handle: &str,
    receipts: bool,
) -> Option<(Option<u64>, Vec<vivi::pump::attempt_read::AttemptFields>)> {
    let Ok(mailspace) = Mailspace::discover(project) else {
        return None;
    };
    let Ok(messages) = mailspace.thread(handle, false, usize::MAX, usize::MAX) else {
        return None;
    };
    let pump_replies = if receipts {
        Vec::new()
    } else {
        // The pump posts attempt records on the pump record's thread.
        mailspace.pump_threads(handle).unwrap_or_default()
    };
    let attempts = messages
        .iter()
        .chain(pump_replies.iter())
        .filter_map(|message| {
            if receipts {
                let mut attempt = vivi::pump::attempt_read::parse_receipt(&message.body)?;
                attempt.end = vivi::pump::attempt_read::opened_epoch(&message.date);
                Some(attempt)
            } else {
                vivi::pump::attempt_read::parse_record(&message.body)
            }
        })
        .collect();
    let opened = messages
        .first()
        .and_then(|message| vivi::pump::attempt_read::opened_epoch(&message.date));
    Some((opened, attempts))
}

/// Print a move's one-line receipt, then one line per bound need the move
/// completed through its join, so the acceptor sees the need close.
pub(crate) fn print_move_receipt(moved: &vivi::mailspace::MoveOutcome, verb: &str) {
    println!("{}", moved.receipt(verb));
    let mut joined = moved.joined_needs.clone();
    joined.dedup();
    for need in joined {
        println!("need {need} completed (all bound units accepted)");
    }
}

/// Need show: the thread plus a bound-units block, so a need's lowering
/// state (which units exist, which have settled) is answerable by reading.
fn show_need(
    handle: &str,
    json: bool,
    latest: bool,
    newest: Option<usize>,
    project: Option<&std::path::Path>,
) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    if let Some(count) = newest {
        vivi::mailspace::print_thread_newest(&mailspace, handle, count, json)?;
        return Ok(());
    }
    if !json && !latest {
        vivi::mailspace::print_thread_outline(&mailspace.thread_outline(handle)?);
    }
    vivi::mailspace::print_thread(&mailspace, handle, false, 50, 50, json, latest)?;
    if json || latest {
        return Ok(());
    }
    let dependencies = mailspace.backlog_dependencies(handle)?;
    if !dependencies.is_empty() {
        let rendered = dependencies
            .iter()
            .map(|dep| format!("{}:{}", dep.handle, dep.state))
            .collect::<Vec<_>>()
            .join("  ");
        println!("  depends  {rendered}");
    }
    let units = mailspace.backlog_bound_units(handle)?;
    if !units.is_empty() {
        let rendered = units
            .iter()
            .map(|unit| format!("{}:{}", unit.handle, unit.state))
            .collect::<Vec<_>>()
            .join("  ");
        println!("  units    {rendered}");
    }
    let runs = vivi::pump::flow::mail::read::run_count(&mailspace, handle);
    if runs > 0 {
        println!("  runs: {runs}");
    }
    Ok(())
}

/// `need attempts`: every pump attempt record on a need thread and on its
/// pump records' threads, plus the anchor's open epoch, in one read for a synced second member.
fn print_need_attempts(
    handle: &str,
    json: bool,
    project: Option<&std::path::Path>,
) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    // The anchor is the message the caller named, not whichever copy of the
    // need the thread happens to place first: a sent and a delivered copy
    // share the same date, so thread order between them is a tie-break, not
    // an identity.
    let anchor = {
        let storage = mailspace.storage()?;
        let anchor_id = storage.resolve_message_token(handle)?;
        storage
            .message_by_id(&anchor_id)?
            .ok_or_else(|| VivariumError::Message(format!("message not found: {handle}")))?
    };
    let messages = mailspace.thread(handle, false, usize::MAX, usize::MAX)?;
    // The pump posts attempt records on the pump record's thread, so the
    // need's attempts are the need thread's plus each record's thread.
    let pump_replies = mailspace.pump_threads(handle).unwrap_or_default();
    let attempts: Vec<_> = messages
        .iter()
        .chain(pump_replies.iter())
        .filter_map(|message| vivi::pump::attempt_read::parse_record(&message.body))
        .collect();
    let opened = vivi::pump::attempt_read::opened_epoch(&anchor.date);
    if json {
        print_attempts_json(&anchor.handle, &attempts, opened)?;
        return Ok(());
    }
    for attempt in &attempts {
        let seconds = attempt
            .agent_seconds
            .map_or_else(|| String::from("-"), |secs| secs.to_string());
        println!(
            "{}  {}  {}s  {}  {}/{}",
            attempt.attempt,
            attempt.outcome,
            seconds,
            attempt.model.as_deref().unwrap_or("-"),
            attempt.pool.as_deref().unwrap_or("-"),
            attempt.slot.as_deref().unwrap_or("-"),
        );
    }
    Ok(())
}

/// The `--json` shape: one object with the anchor handle, the open epoch and
/// every attempt record.
fn print_attempts_json(
    need: &str,
    attempts: &[vivi::pump::attempt_read::AttemptFields],
    opened: Option<u64>,
) -> Result<(), VivariumError> {
    let out = serde_json::json!({ "need": need, "opened": opened, "attempts": attempts });
    println!(
        "{}",
        serde_json::to_string_pretty(&out)
            .map_err(|error| VivariumError::Other(format!("failed to encode JSON: {error}")))?
    );
    Ok(())
}

fn status_roles<'a>(status: &TaskStatus, open_role: &'a str) -> Vec<&'a str> {
    match status {
        TaskStatus::Open => vec![open_role, "sent"],
        TaskStatus::Done => vec!["done"],
        TaskStatus::All => vec![open_role, "sent", "done"],
    }
}

fn want_state_filter(status: &WantStatus) -> Option<vivi::mailspace::WantState> {
    match status {
        WantStatus::Open => Some(vivi::mailspace::WantState::Open),
        WantStatus::Done => Some(vivi::mailspace::WantState::Done),
        WantStatus::All => None,
    }
}

fn want_status_roles(status: &WantStatus) -> &'static [&'static str] {
    match status {
        WantStatus::Open => &["wants", "sent"],
        WantStatus::Done => &["done"],
        WantStatus::All => &["wants", "sent", "done"],
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
        include_outbound: true,
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
