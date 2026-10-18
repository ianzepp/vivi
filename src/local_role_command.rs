use vivi::VivariumError;
use vivi::cli::{RoleCharterCommand, RoleCommand};
use vivi::mailspace::{Mailspace, RoleUpdate, RoleView, read_body_input};
use vivi::role_status;

pub(crate) fn handle_role_command(command: &RoleCommand) -> Result<(), VivariumError> {
    match command {
        RoleCommand::List { project, json } => list_roles(project.as_deref(), *json),
        RoleCommand::Show {
            name,
            project,
            json,
        } => show_role(name, project.as_deref(), *json),
        RoleCommand::Add { .. } => add_from(command),
        RoleCommand::Set { .. } => set_from(command),
        RoleCommand::Pool {
            name,
            pool,
            project,
        } => set_role(
            name,
            &RoleSetArgs {
                pool: Some(pool.clone()),
                ..RoleSetArgs::default()
            },
            project.as_deref(),
        ),
        RoleCommand::Rename { old, new, project } => rename_role(old, new, project.as_deref()),
        RoleCommand::Charter { command } => handle_charter(command),
        RoleCommand::Status {
            name,
            project,
            json,
        } => status_role(name, project.as_deref(), *json),
    }
}

fn set_from(command: &RoleCommand) -> Result<(), VivariumError> {
    let RoleCommand::Set { name, project, .. } = command else {
        return Err(VivariumError::Message("internal: expected role set".into()));
    };
    set_role(name, &role_set_args(command), project.as_deref())
}

fn role_set_args(command: &RoleCommand) -> RoleSetArgs {
    let RoleCommand::Set {
        kind,
        clear_kind,
        harness,
        clear_harness,
        provider,
        clear_provider,
        model,
        clear_model,
        thinking,
        clear_thinking,
        pid,
        clear_pid,
        host,
        clear_host,
        cadence,
        clear_cadence,
        status,
        labels,
        clear_labels,
        pool,
        clear_pool,
        read_only,
        clear_read_only,
        models,
        clear_models,
        ..
    } = command
    else {
        return RoleSetArgs::default();
    };
    RoleSetArgs {
        kind: kind.clone(),
        clear_kind: *clear_kind,
        harness: harness.clone(),
        clear_harness: *clear_harness,
        provider: provider.clone(),
        clear_provider: *clear_provider,
        model: model.clone(),
        clear_model: *clear_model,
        thinking: thinking.clone(),
        clear_thinking: *clear_thinking,
        pid: *pid,
        clear_pid: *clear_pid,
        host: host.clone(),
        clear_host: *clear_host,
        cadence: cadence.clone(),
        clear_cadence: *clear_cadence,
        status: status.clone(),
        labels: labels.clone(),
        clear_labels: clear_labels.clone(),
        pool: pool.clone(),
        clear_pool: *clear_pool,
        read_only: *read_only,
        clear_read_only: *clear_read_only,
        models: models.clone(),
        clear_models: *clear_models,
    }
}

fn add_from(command: &RoleCommand) -> Result<(), VivariumError> {
    let RoleCommand::Add {
        name,
        kind,
        harness,
        provider,
        model,
        thinking,
        status,
        labels,
        cadence,
        pool,
        read_only,
        models,
        project,
    } = command
    else {
        return Err(VivariumError::Message("internal: expected role add".into()));
    };
    add_role(&AddRoleArgs {
        name,
        kind: kind.as_deref(),
        harness: harness.as_deref(),
        provider: provider.as_deref(),
        model: model.as_deref(),
        thinking: thinking.as_deref(),
        status: status.as_deref(),
        labels,
        cadence: cadence.as_deref(),
        pool: pool.as_deref(),
        read_only: *read_only,
        models,
        project: project.as_deref(),
    })
}

struct AddRoleArgs<'a> {
    name: &'a str,
    kind: Option<&'a str>,
    harness: Option<&'a str>,
    provider: Option<&'a str>,
    model: Option<&'a str>,
    thinking: Option<&'a str>,
    status: Option<&'a str>,
    labels: &'a [String],
    cadence: Option<&'a str>,
    pool: Option<&'a str>,
    read_only: Option<bool>,
    models: &'a [String],
    project: Option<&'a std::path::Path>,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Default)]
struct RoleSetArgs {
    kind: Option<String>,
    clear_kind: bool,
    harness: Option<String>,
    clear_harness: bool,
    provider: Option<String>,
    clear_provider: bool,
    model: Option<String>,
    clear_model: bool,
    thinking: Option<String>,
    clear_thinking: bool,
    pid: Option<u32>,
    clear_pid: bool,
    host: Option<String>,
    clear_host: bool,
    cadence: Option<String>,
    clear_cadence: bool,
    status: Option<String>,
    labels: Vec<String>,
    clear_labels: Vec<String>,
    pool: Option<String>,
    clear_pool: bool,
    read_only: Option<bool>,
    clear_read_only: bool,
    models: Vec<String>,
    clear_models: bool,
}

fn list_roles(project: Option<&std::path::Path>, json: bool) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    let views = mailspace.list_role_views()?;
    if json {
        print_json(&views)?;
        return Ok(());
    }
    for view in &views {
        print_role_text(view, false);
    }
    Ok(())
}

fn show_role(
    name: &str,
    project: Option<&std::path::Path>,
    json: bool,
) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    let view = mailspace.role_view(name)?;
    if json {
        print_json(&view)?;
    } else {
        print_role_text(&view, true);
    }
    Ok(())
}

fn add_role(args: &AddRoleArgs<'_>) -> Result<(), VivariumError> {
    let mut mailspace = Mailspace::discover(args.project)?;
    let update = build_add_update(args);
    let address = mailspace.add_role(args.name, update)?;
    println!("added {address}");
    let role = mailspace.resolve_identity(args.name)?;
    report_adopted(&mailspace, &role)
}

/// Adopt the letters held under a role name that now exists, and say how
/// many: `role add` and `role rename` both end here.
pub(crate) fn report_adopted(mailspace: &Mailspace, role: &str) -> Result<(), VivariumError> {
    let adopted = mailspace.adopt_held_letters(role)?;
    if adopted > 0 {
        let noun = if adopted == 1 { "letter" } else { "letters" };
        println!("adopted {adopted} held {noun}");
    }
    Ok(())
}

/// Build the full-field `RoleUpdate` for a `role add`. No clears and no
/// pid/host: those are self-set live-process bindings, set later via
/// `role set`, not at roster-creation time.
fn build_add_update(args: &AddRoleArgs<'_>) -> RoleUpdate {
    RoleUpdate {
        kind: args.kind.map(|value| Some(value.to_string())),
        status: args.status.map(str::to_string),
        harness: args.harness.map(|value| Some(value.to_string())),
        provider: args.provider.map(|value| Some(value.to_string())),
        model: args.model.map(|value| Some(value.to_string())),
        thinking: args.thinking.map(|value| Some(value.to_string())),
        cadence: args.cadence.map(|value| Some(value.to_string())),
        add_labels: args.labels.iter().map(String::clone).collect(),
        pool: args.pool.map(|value| Some(value.to_string())),
        read_only: args.read_only.map(Some),
        models: (!args.models.is_empty()).then(|| args.models.to_vec()),
        ..RoleUpdate::default()
    }
}

fn set_role(
    name: &str,
    args: &RoleSetArgs,
    project: Option<&std::path::Path>,
) -> Result<(), VivariumError> {
    let update = build_role_update(args);
    if !update_has_fields(&update) {
        return Err(VivariumError::Message(
            "role set requires at least one field to change".into(),
        ));
    }
    let mut mailspace = Mailspace::discover(project)?;
    let view = mailspace.set_role(name, update)?;
    println!("updated {} ({})", view.name, view.address);
    Ok(())
}

fn rename_role(
    old: &str,
    new: &str,
    project: Option<&std::path::Path>,
) -> Result<(), VivariumError> {
    let mut mailspace = Mailspace::discover(project)?;
    let address = mailspace.rename_identity(old, new)?;
    println!("renamed {old} -> {new} ({address})");
    println!("historical mail sent as {old} still resolves under {new}");
    let role = mailspace.resolve_identity(new)?;
    report_adopted(&mailspace, &role)
}

fn status_role(
    name: &str,
    project: Option<&std::path::Path>,
    json: bool,
) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    let view = mailspace.role_view(name)?;
    let status = role_status::probe(view.pid, view.host.as_deref(), view.harness.as_deref());
    let schedule = mailspace.schedule_report(&view.name)?;
    let outcome = role_status::RoleStatusOutcome {
        name: view.name.clone(),
        address: view.address.clone(),
        pid: view.pid,
        host: view.host.clone(),
        status,
        schedule,
    };
    if json {
        print_json(&outcome)?;
    } else {
        outcome.print_text();
    }
    Ok(())
}

fn handle_charter(command: &RoleCharterCommand) -> Result<(), VivariumError> {
    match command {
        RoleCharterCommand::Show {
            name,
            project,
            json,
        } => show_charter(name, project.as_deref(), *json),
        RoleCharterCommand::Set {
            name,
            body,
            body_file,
            file,
            project,
        } => {
            let path = body_file
                .as_ref()
                .or(file.as_ref())
                .map(std::path::PathBuf::as_path);
            let charter = read_body_input(body.as_deref(), path)?;
            let mut mailspace = Mailspace::discover(project.as_deref())?;
            mailspace.set_charter(name, &charter)?;
            println!("charter set for {name} ({} bytes)", charter.len());
            Ok(())
        }
    }
}

fn show_charter(
    name: &str,
    project: Option<&std::path::Path>,
    json: bool,
) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    let view = mailspace.role_view(name)?;
    if json {
        print_json(&serde_json::json!({
            "name": view.name,
            "address": view.address,
            "has_charter": view.has_charter,
            "charter": view.charter,
        }))?;
    } else if view.charter.is_empty() {
        println!("(empty charter)");
    } else {
        print!("{}", view.charter);
        if !view.charter.ends_with('\n') {
            println!();
        }
    }
    Ok(())
}

fn build_role_update(args: &RoleSetArgs) -> RoleUpdate {
    let mut update = RoleUpdate {
        add_labels: args.labels.clone(),
        clear_labels: args.clear_labels.clone(),
        ..RoleUpdate::default()
    };
    if args.clear_kind {
        update.kind = Some(None);
    } else if let Some(kind) = &args.kind {
        update.kind = Some(Some(kind.clone()));
    }
    if let Some(status) = &args.status {
        update.status = Some(status.clone());
    }
    set_optional_field(
        &mut update.harness,
        args.clear_harness,
        args.harness.as_ref(),
    );
    set_optional_field(
        &mut update.provider,
        args.clear_provider,
        args.provider.as_ref(),
    );
    set_optional_field(&mut update.model, args.clear_model, args.model.as_ref());
    set_optional_field(
        &mut update.thinking,
        args.clear_thinking,
        args.thinking.as_ref(),
    );
    set_optional_field(
        &mut update.cadence,
        args.clear_cadence,
        args.cadence.as_ref(),
    );
    apply_pid_update(
        &mut update.pid,
        &mut update.host,
        args.pid,
        args.clear_pid,
        args.host.as_ref(),
        args.clear_host,
    );
    set_optional_field(&mut update.pool, args.clear_pool, args.pool.as_ref());
    if args.clear_read_only {
        update.read_only = Some(None);
    } else if let Some(flag) = args.read_only {
        update.read_only = Some(Some(flag));
    }
    if args.clear_models {
        update.models = Some(Vec::new());
    } else if !args.models.is_empty() {
        update.models = Some(args.models.clone());
    }
    update
}

#[allow(clippy::option_option)]
fn set_optional_field(slot: &mut Option<Option<String>>, clear: bool, value: Option<&String>) {
    if clear {
        *slot = Some(None);
    } else if let Some(value) = value {
        *slot = Some(Some(value.clone()));
    }
}

/// Resolve the pid/host fields of a `role set` call into `RoleUpdate` slots.
///
/// `--pid` without `--host` defaults `host` to the local hostname, so a binding
/// is always host-complete (the PID-file invariant). `--clear-pid` also clears
/// host, since a binding without a pid has no meaningful host.
#[allow(clippy::option_option)]
fn apply_pid_update(
    pid_slot: &mut Option<Option<u32>>,
    host_slot: &mut Option<Option<String>>,
    pid: Option<u32>,
    clear_pid: bool,
    host: Option<&String>,
    clear_host: bool,
) {
    if clear_pid {
        *pid_slot = Some(None);
        *host_slot = Some(None);
        return;
    }
    if let Some(pid) = pid {
        *pid_slot = Some(Some(pid));
        if clear_host {
            *host_slot = Some(None);
        } else if let Some(host) = host {
            *host_slot = Some(Some(host.clone()));
        } else {
            *host_slot = Some(role_status::local_host_name());
        }
        return;
    }
    if clear_host {
        *host_slot = Some(None);
    } else if let Some(host) = host {
        *host_slot = Some(Some(host.clone()));
    }
}

fn update_has_fields(update: &RoleUpdate) -> bool {
    update.kind.is_some()
        || update.status.is_some()
        || update.harness.is_some()
        || update.provider.is_some()
        || update.model.is_some()
        || update.thinking.is_some()
        || update.pid.is_some()
        || update.host.is_some()
        || update.cadence.is_some()
        || update.pool.is_some()
        || update.read_only.is_some()
        || update.models.is_some()
        || !update.add_labels.is_empty()
        || !update.clear_labels.is_empty()
}

fn print_json<T: serde::Serialize>(value: &T) -> Result<(), VivariumError> {
    println!(
        "{}",
        serde_json::to_string_pretty(value)
            .map_err(|e| VivariumError::Other(format!("failed to encode JSON: {e}")))?
    );
    Ok(())
}

fn print_role_text(view: &RoleView, full: bool) {
    println!("{} {}", view.name, view.address);
    if !view.aliases.is_empty() {
        println!("  formerly: {}", view.aliases.join(", "));
    }
    println!("  kind:      {}", display_opt(view.kind.as_deref()));
    println!("  status:    {}", view.status);
    println!("  harness:   {}", display_opt(view.harness.as_deref()));
    println!("  provider:  {}", display_opt(view.provider.as_deref()));
    println!("  model:     {}", display_opt(view.model.as_deref()));
    println!("  thinking:  {}", display_opt(view.thinking.as_deref()));
    println!("  pid:       {}", display_pid(view.pid));
    println!("  host:      {}", display_opt(view.host.as_deref()));
    println!("  cadence:   {}", display_opt(view.cadence.as_deref()));
    println!("  pool:      {}", display_opt(view.pool.as_deref()));
    println!(
        "  read_only: {}",
        view.read_only
            .map_or_else(|| "(unset)".into(), |flag| flag.to_string())
    );
    println!(
        "  models:    {}",
        if view.models.is_empty() {
            "(unset)".into()
        } else {
            view.models.join(", ")
        }
    );
    if view.labels.is_empty() {
        println!("  labels:    (none)");
    } else {
        println!("  labels:    {}", view.labels.join(", "));
    }
    println!(
        "  charter:   {}",
        if view.has_charter {
            format!("{} bytes", view.charter.len())
        } else {
            "(empty)".into()
        }
    );
    if full && view.has_charter {
        println!("--- charter ---");
        print!("{}", view.charter);
        if !view.charter.ends_with('\n') {
            println!();
        }
    }
}

fn display_opt(value: Option<&str>) -> &str {
    value.unwrap_or("(unset)")
}

fn display_pid(value: Option<u32>) -> String {
    match value {
        Some(pid) => pid.to_string(),
        None => "(unset)".into(),
    }
}
