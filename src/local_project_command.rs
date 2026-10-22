use vivi::VivariumError;
use vivi::cli::{ProjectCommand, ProjectCreateCommand};
use vivi::project_create::{self, ProjectCreate, ProjectCreated};

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
    request.gate.clone_from(&command.gate);
    request.branch.clone_from(&command.branch);
    request.pump = !command.no_pump;
    request.serve = !command.no_serve;
    request.initial_commit = command.initial_commit;
    let created = project_create::create(&request)?;
    if command.json {
        let text = serde_json::to_string_pretty(&created)
            .map_err(|error| VivariumError::Other(format!("failed to encode JSON: {error}")))?;
        println!("{text}");
    } else {
        print_created(&created);
    }
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
    if created.readiness.ready {
        println!("ready     yes");
        return;
    }
    println!(
        "ready     no: {} item(s) missing (the pump holds until they are supplied)",
        created.readiness.missing.len()
    );
    for item in &created.readiness.missing {
        println!("  - {}: {}", item.item, item.detail);
    }
}
