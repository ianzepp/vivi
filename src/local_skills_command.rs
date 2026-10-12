use vivi::VivariumError;
use vivi::cli::SkillsCommand;
use vivi::skills::{self, Skill};

pub(crate) fn handle_skills_command(command: &SkillsCommand) -> Result<(), VivariumError> {
    match command {
        SkillsCommand::List { json } => print_list(*json),
        SkillsCommand::Show { name, file } => print_show(name, file.as_deref()),
        SkillsCommand::Install { dir, force } => print_install(dir, *force),
    }
}

fn print_list(json: bool) -> Result<(), VivariumError> {
    let skills = skills::catalog();
    if json {
        let listings: Vec<_> = skills.iter().map(Skill::summary).collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&listings)
                .map_err(|e| VivariumError::Other(format!("failed to encode JSON: {e}")))?
        );
        return Ok(());
    }
    let width = skills
        .iter()
        .map(|skill| skill.name.len())
        .max()
        .unwrap_or(0);
    for skill in skills {
        println!(
            "{:width$}  {}  {}",
            skill.name, skill.version, skill.description
        );
    }
    Ok(())
}

fn print_show(name: &str, file: Option<&str>) -> Result<(), VivariumError> {
    let shown = skills::show(name, file)?;
    if file.is_none() {
        println!("skill: {}", shown.skill.name);
        println!("version: {}", shown.skill.version);
        println!("file: {}", shown.file);
        println!("files:");
        for path in shown.skill.file_names() {
            println!("  {path}");
        }
        println!();
    }
    print!("{}", shown.contents);
    if !shown.contents.ends_with('\n') {
        println!();
    }
    Ok(())
}

fn print_install(dir: &std::path::Path, force: bool) -> Result<(), VivariumError> {
    let report = skills::install(dir, force)?;
    println!(
        "installed {} skills, {} files -> {}",
        report.skills,
        report.files,
        report.dir.display()
    );
    Ok(())
}
