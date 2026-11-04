use vivi_core::cli::GoalCommand;
use vivi_core::mailspace::{GoalView, Mailspace};
use vivi_error::VivariumError;

pub(crate) fn handle_goal_command(command: &GoalCommand) -> Result<(), VivariumError> {
    match command {
        GoalCommand::Add {
            path,
            label,
            for_identity,
            project,
            json,
        } => {
            let view = Mailspace::discover(project.as_deref())?.goal_add(
                path,
                label.as_deref(),
                for_identity.as_deref(),
            )?;
            print_goal_result("registered", &view, *json)?;
        }
        GoalCommand::List { project, json } => {
            let goals = Mailspace::discover(project.as_deref())?.goal_list()?;
            print_goal_list(&goals, *json)?;
        }
        GoalCommand::Show {
            selector,
            project,
            json,
        } => {
            let view = Mailspace::discover(project.as_deref())?.goal_show(selector)?;
            print_goal_result("goal", &view, *json)?;
        }
        GoalCommand::Drop {
            selector,
            project,
            json,
        } => {
            let view = Mailspace::discover(project.as_deref())?.goal_drop(selector)?;
            print_goal_result("dropped", &view, *json)?;
        }
    }
    Ok(())
}

fn print_goal_result(verb: &str, view: &GoalView, json: bool) -> Result<(), VivariumError> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(view)
                .map_err(|e| VivariumError::Other(format!("failed to encode JSON: {e}")))?
        );
        return Ok(());
    }
    let label = view
        .label
        .as_deref()
        .map(|l| format!("  {l}"))
        .unwrap_or_default();
    let missing = if view.exists { "" } else { "  MISSING" };
    println!("{verb} {}  {}{label}{missing}", view.handle, view.path);
    Ok(())
}

fn print_goal_list(goals: &[GoalView], json: bool) -> Result<(), VivariumError> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(goals)
                .map_err(|e| VivariumError::Other(format!("failed to encode JSON: {e}")))?
        );
        return Ok(());
    }
    if goals.is_empty() {
        println!("  no goals");
        return Ok(());
    }
    println!("  handle  path  label  exists");
    for goal in goals {
        let label = goal.label.as_deref().unwrap_or("-");
        let exists = if goal.exists { "yes" } else { "MISSING" };
        println!("  {}  {}  {}  {}", goal.handle, goal.path, label, exists);
    }
    Ok(())
}
