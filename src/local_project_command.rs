use vivi::VivariumError;
use vivi::cli::{ProjectCommand, ProjectCreateCommand};
use vivi::project_create::{self, Half, ProjectCreate, ProjectCreated, Readiness, SplitCreated};

pub(crate) fn handle(command: &ProjectCommand) -> Result<(), VivariumError> {
    match command {
        ProjectCommand::Create(command) => create(command),
    }
}

fn create(command: &ProjectCreateCommand) -> Result<(), VivariumError> {
    let mut request = ProjectCreate::new(
        &command.name,
        &command.projects_root,
        &command.lanes_root,
        &command.git_url_prefix,
    );
    request.pool.clone_from(&command.pool);
    request.branch.clone_from(&command.branch);
    request.pump = !command.no_pump;
    request.serve = !command.no_serve;
    request.initial_commit = command.initial_commit;
    let split = project_create::create_split(&request)?;
    if let Some(created) = split.full() {
        print(&created, command.json)?;
        return Ok(());
    }
    print_split(&split, command.json)
}

fn print(created: &ProjectCreated, json: bool) -> Result<(), VivariumError> {
    if json {
        print_json(created)?;
    } else {
        print_created(created);
    }
    Ok(())
}

fn print_split(split: &SplitCreated, json: bool) -> Result<(), VivariumError> {
    if json {
        return print_json(split);
    }
    if let Some(lane) = &split.lane {
        println!("repo      {}", lane.repo_path.display());
        if let Some(commit) = &lane.initial_commit {
            println!("commit    {commit}");
        }
    }
    if let Some(project) = &split.project {
        println!("created   {}", project.project_root.display());
        println!("git url   {}", project.git_url);
        println!("mailspace {}", project.mailspace_id);
        print_readiness(&project.readiness);
    }
    if let Some(remaining) = &split.remaining {
        let half = match remaining.half {
            Half::Lanes => "lanes",
            Half::Projects => "projects",
        };
        println!(
            "remaining {half} half: no write permission on {}",
            remaining.root.display()
        );
        println!("  run, as a user who can write it:");
        println!("  {}", remaining.command);
    }
    Ok(())
}

fn print_json(value: &impl serde::Serialize) -> Result<(), VivariumError> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|error| VivariumError::Other(format!("failed to encode JSON: {error}")))?;
    println!("{text}");
    Ok(())
}

fn print_created(created: &ProjectCreated) {
    println!("created   {}", created.project_root.display());
    println!("git url   {}", created.git_url);
    println!("repo      {}", created.repo_path.display());
    println!("mailspace {}", created.mailspace_id);
    if let Some(commit) = &created.initial_commit {
        println!("commit    {commit}");
    }
    print_readiness(&created.readiness);
}

fn print_readiness(readiness: &Readiness) {
    if readiness.ready {
        println!("ready     yes");
        return;
    }
    println!(
        "ready     no: {} item(s) missing (the pump holds until they are supplied)",
        readiness.missing.len()
    );
    for item in &readiness.missing {
        println!("  - {}: {}", item.item, item.detail);
    }
}
