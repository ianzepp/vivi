use std::path::Path;

use vivi::VivariumError;
use vivi::cli::{
    AbsorbCommand, RuleCommand, RuleRetireCommand, RuleSaveCommand, RuleSupersedeCommand,
};
use vivi::mailspace::{Mailspace, RuleDetail, RuleRecord};

pub(crate) fn handle_rule_command(command: &RuleCommand) -> Result<(), VivariumError> {
    match command {
        RuleCommand::Save(command) => save_rule(command),
        RuleCommand::Supersede(command) => supersede_rule(command),
        RuleCommand::Retire(command) => retire_rule(command),
        RuleCommand::Absorb(command) => absorb_rule(command),
        RuleCommand::List {
            for_identity,
            all,
            json,
            project,
        } => list_rules(for_identity.as_deref(), *all, *json, project.as_deref()),
        RuleCommand::Search {
            query,
            for_identity,
            subject,
            all,
            json,
            project,
        } => search_rules(
            query,
            for_identity.as_deref(),
            *subject,
            *all,
            *json,
            project.as_deref(),
        ),
        RuleCommand::Show {
            handle,
            json,
            project,
        } => show_rule(handle, *json, project.as_deref()),
    }
}

fn save_rule(command: &RuleSaveCommand) -> Result<(), VivariumError> {
    let body =
        vivi::mailspace::read_body_input(command.body.as_deref(), command.body_file.as_deref())?;
    let handle = Mailspace::discover(command.project.as_deref())?.save_rule(
        &command.for_identity,
        &command.subject,
        &body,
        command.local,
    )?;
    println!("saved {handle}");
    Ok(())
}

fn supersede_rule(command: &RuleSupersedeCommand) -> Result<(), VivariumError> {
    let body =
        vivi::mailspace::read_body_input(command.body.as_deref(), command.body_file.as_deref())?;
    let written = Mailspace::discover(command.project.as_deref())?.supersede_rule(
        &command.for_identity,
        &command.handle,
        &command.subject,
        &body,
        command.local,
    )?;
    println!("superseded {} with {}", written.previous, written.handle);
    Ok(())
}

fn retire_rule(command: &RuleRetireCommand) -> Result<(), VivariumError> {
    let change = Mailspace::discover(command.project.as_deref())?.retire_rule(
        &command.for_identity,
        &command.handle,
        command.note.as_deref(),
    )?;
    if change.changed {
        println!("retired {}", change.handle);
    } else {
        println!("{} already retired", change.handle);
    }
    Ok(())
}

fn absorb_rule(command: &AbsorbCommand) -> Result<(), VivariumError> {
    let handle = Mailspace::discover(command.project.as_deref())?.absorb_rule(
        &command.for_identity,
        &command.handle,
        command.note.as_deref(),
    )?;
    println!("absorbed {handle}");
    Ok(())
}

fn list_rules(
    author: Option<&str>,
    history: bool,
    json: bool,
    project: Option<&Path>,
) -> Result<(), VivariumError> {
    let rules = Mailspace::discover(project)?.list_rules(author, history)?;
    print_rules(&rules, json)
}

fn search_rules(
    query: &str,
    author: Option<&str>,
    subject_only: bool,
    history: bool,
    json: bool,
    project: Option<&Path>,
) -> Result<(), VivariumError> {
    let rules = Mailspace::discover(project)?.search_rules(query, author, subject_only, history)?;
    print_rules(&rules, json)
}

fn show_rule(handle: &str, json: bool, project: Option<&Path>) -> Result<(), VivariumError> {
    let detail = Mailspace::discover(project)?.show_rule(handle)?;
    if json {
        return print_json(&detail);
    }
    print_rule(&detail);
    Ok(())
}

fn print_rules(rules: &[RuleRecord], json: bool) -> Result<(), VivariumError> {
    if json {
        return print_json(&rules);
    }
    if rules.is_empty() {
        println!("  no rules");
        return Ok(());
    }
    println!("handle    scope    state       author  subject");
    for rule in rules {
        println!(
            "{}  {:<7} {:<10} {}  {}",
            rule.handle, rule.scope, rule.state, rule.author, rule.subject
        );
    }
    Ok(())
}

fn print_rule(detail: &RuleDetail) {
    let rule = &detail.rule;
    println!("scope: {}", rule.scope);
    println!("author: {}", rule.author);
    println!("state: {}", rule.state);
    if let Some(successor) = &rule.successor {
        println!("superseded by: {successor}");
    }
    println!();
    println!("Subject: {}", rule.subject);
    println!();
    println!("{}", detail.body);
}

fn print_json(value: &impl serde::Serialize) -> Result<(), VivariumError> {
    let encoded = serde_json::to_string_pretty(value)
        .map_err(|error| VivariumError::Other(format!("failed to encode JSON: {error}")))?;
    println!("{encoded}");
    Ok(())
}
