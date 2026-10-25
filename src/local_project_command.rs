use vivi::VivariumError;
use vivi::cli::{
    ProjectCommand, ProjectCreateCommand, ProjectDeleteCommand, ProjectLaneAddCommand,
    ProjectLaneCommand,
};
use vivi::project_create::lane_add::{self, LaneAdd, LaneAdded};
use vivi::project_create::{
    self, ProjectCreate, ProjectCreated, ProjectDelete, ProjectDeleted, Readiness,
};

pub(crate) fn handle(command: &ProjectCommand) -> Result<(), VivariumError> {
    match command {
        ProjectCommand::Create(command) => create(command),
        ProjectCommand::Delete(command) => delete(command),
        ProjectCommand::Lane { command } => lane(command),
    }
}

fn create(command: &ProjectCreateCommand) -> Result<(), VivariumError> {
    let mut request = ProjectCreate::new(
        &command.name,
        &command.projects_root,
        &command.lanes_root,
        &command.git_url_prefix,
    );
    request.target.clone_from(&command.target);
    request.pool.clone_from(&command.pool);
    request.branch.clone_from(&command.branch);
    request.pump = !command.no_pump;
    request.serve = !command.no_serve;
    request.initial_commit = command.initial_commit;
    request.source.clone_from(&command.source);
    let created = project_create::create(&request)?;
    if command.json {
        return print_json(&created);
    }
    print_created(&created);
    Ok(())
}

fn delete(command: &ProjectDeleteCommand) -> Result<(), VivariumError> {
    let request = ProjectDelete {
        name: command.name.clone(),
        projects_root: command.projects_root.clone(),
        lanes_root: command.lanes_root.clone(),
        confirm: command.confirm.clone(),
    };
    let deleted = project_create::delete::delete(&request)?;
    if command.json {
        return print_json(&deleted);
    }
    print_deleted(&deleted);
    Ok(())
}

fn lane(command: &ProjectLaneCommand) -> Result<(), VivariumError> {
    match command {
        ProjectLaneCommand::Add(command) => lane_add(command),
    }
}

fn lane_add(command: &ProjectLaneAddCommand) -> Result<(), VivariumError> {
    let mut request = LaneAdd::new(
        &command.project_root,
        &command.target_id,
        &command.lanes_root,
    );
    request.git_url_prefix.clone_from(&command.git_url_prefix);
    request.seed = command.seed;
    request.source.clone_from(&command.source);
    let added = lane_add::add(&request)?;
    if command.json {
        return print_json(&added);
    }
    print_lane_added(&added);
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
    println!("target    {}", created.target);
    println!("git url   {}", created.git_url);
    println!("repo      {}", created.repo_path.display());
    println!("checkout  {}", created.checkout_path.display());
    println!("mailspace {}", created.mailspace_id);
    if let Some(commit) = &created.initial_commit {
        println!("commit    {commit}");
    }
    if let Some(tip) = &created.imported {
        println!("imported  {tip}");
    }
    print_readiness(&created.readiness);
}

fn print_lane_added(added: &LaneAdded) {
    println!("target    {}", added.target_id);
    println!("repo      {}", added.repo_path.display());
    println!("checkout  {}", added.checkout_path.display());
    println!("git url   {}", added.git_url);
    if let Some(commit) = &added.initial_commit {
        println!("commit    {commit}");
    }
    if let Some(tip) = &added.imported {
        println!("imported  {tip}");
    }
}

fn print_deleted(deleted: &ProjectDeleted) {
    println!("deleted   {}", deleted.name);
    if deleted.unregistered {
        println!("unregistered");
    }
    for path in [&deleted.project_root, &deleted.lanes_dir]
        .into_iter()
        .flatten()
    {
        println!("removed   {}", path.display());
    }
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
