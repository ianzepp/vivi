//! Embedded skills: list, show, install.

use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use clap::{Command as ClapCommand, CommandFactory};
use serde_json::Value;
use tempfile::tempdir;
use vivi::cli::Cli;
use vivi::skills;

#[test]
fn help_names_the_skills_commands() {
    let top = stdout(&vivi(&["--help"]));
    assert!(top.contains("skills"), "{top}");

    let help = stdout(&vivi(&["skills", "--help"]));
    assert!(help.contains("list"), "{help}");
    assert!(help.contains("show"), "{help}");
    assert!(help.contains("install"), "{help}");
    assert!(help.contains("embedded"), "{help}");

    let show = stdout(&vivi(&["skills", "show", "--help"]));
    assert!(show.contains("--file"), "{show}");

    let install = stdout(&vivi(&["skills", "install", "--help"]));
    assert!(install.contains("--force"), "{install}");
}

#[test]
fn list_prints_names_descriptions_and_the_binary_version() {
    let version = stdout(&vivi(&["--version"]));
    let crate_version = version
        .split_whitespace()
        .last()
        .expect("vivi --version ends with the crate version")
        .trim()
        .to_string();

    let listed = stdout(&vivi(&["skills", "list"]));
    for name in ["vivi", "tugboat", "speedboat"] {
        let line = listed
            .lines()
            .find(|line| line.starts_with(name))
            .unwrap_or_else(|| panic!("missing {name} in:\n{listed}"));
        assert!(line.contains(&crate_version), "{line}");
    }
    assert!(listed.contains("Use the Vivi CLI"), "{listed}");

    let json: Value = serde_json::from_str(&stdout(&vivi(&["skills", "list", "--json"]))).unwrap();
    let names: Vec<_> = json
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["vivi", "tugboat", "speedboat"]);
    assert_eq!(json[0]["version"], crate_version);
    assert!(
        json[0]["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|file| file == "SKILL.md")
    );
}

#[test]
fn show_prints_skill_md_and_lists_supporting_files() {
    let out = stdout(&vivi(&["skills", "show", "vivi"]));
    assert!(out.contains("skill: vivi"), "{out}");
    assert!(out.contains("files:"), "{out}");
    assert!(out.contains("  SKILL.md"), "{out}");
    assert!(out.contains("  references/work-graph-flow.md"), "{out}");
    assert!(out.contains("# Vivi"), "{out}");

    let flow = stdout(&vivi(&[
        "skills",
        "show",
        "vivi",
        "--file",
        "references/work-graph-flow.md",
    ]));
    assert!(!flow.contains("skill: vivi"), "{flow}");
    assert!(
        flow.contains("Work-graph") || flow.contains("work graph"),
        "{flow}"
    );

    let missing = vivi(&["skills", "show", "ghost"]);
    assert!(!missing.status.success());
    assert!(
        stderr(&missing).contains("unknown skill"),
        "{}",
        stderr(&missing)
    );
}

#[test]
fn install_writes_skills_and_refuses_without_force() {
    let dir = tempdir().unwrap();
    let dest = dir.path().join("agent-skills");
    let dest_s = dest.to_str().unwrap();

    let first = vivi(&["skills", "install", dest_s]);
    assert_success(&first);
    let receipt = stdout(&first);
    assert!(receipt.contains("installed 3 skills"), "{receipt}");
    assert!(receipt.contains(dest_s), "{receipt}");

    assert!(dest.join("vivi/SKILL.md").is_file());
    assert!(dest.join("tugboat/references/charters/hand.md").is_file());
    assert!(dest.join("speedboat/SKILL.md").is_file());
    assert_eq!(
        fs::read_to_string(dest.join("vivi/SKILL.md")).unwrap(),
        on_disk("skills/vivi/SKILL.md")
    );

    fs::write(dest.join("vivi/SKILL.md"), "stale\n").unwrap();
    let blocked = vivi(&["skills", "install", dest_s]);
    assert!(!blocked.status.success());
    assert!(
        stderr(&blocked).contains("already exists"),
        "{}",
        stderr(&blocked)
    );

    let forced = vivi(&["skills", "install", dest_s, "--force"]);
    assert_success(&forced);
    assert_eq!(
        fs::read_to_string(dest.join("vivi/SKILL.md")).unwrap(),
        on_disk("skills/vivi/SKILL.md")
    );
}

fn on_disk(rel: &str) -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)).unwrap()
}

fn vivi(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args(args)
        .output()
        .unwrap()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "status: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        stdout(output),
        stderr(output)
    );
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn embedded_skill_examples_match_the_clap_tree() {
    let root = Cli::command();
    let mut failures = Vec::new();
    for skill in skills::catalog() {
        for file in skill.file_names() {
            if !file.ends_with(".md") {
                continue;
            }
            let shown = skills::show(skill.name, Some(file)).expect("embedded file");
            for example in extract_vivi_examples(shown.contents) {
                if let Err(reason) = validate_example(&root, &example.tokens) {
                    failures.push(format!(
                        "{}:{}: `{}` — {reason}",
                        skill.name, file, example.display
                    ));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "skill examples that are not on the current CLI:\n{}",
        failures.join("\n")
    );
}

struct SkillExample {
    display: String,
    tokens: Vec<String>,
}

fn extract_vivi_examples(markdown: &str) -> Vec<SkillExample> {
    let mut examples = Vec::new();
    let mut in_fence = false;
    let mut continued = String::new();
    for raw in markdown.lines() {
        let trimmed = raw.trim();
        if trimmed.starts_with("```") {
            if in_fence && !continued.is_empty() {
                push_example(&mut examples, &continued);
                continued.clear();
            }
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            if trimmed.ends_with('\\') {
                continued.push_str(trimmed.trim_end_matches('\\'));
                continued.push(' ');
                continue;
            }
            let line = if continued.is_empty() {
                trimmed.to_string()
            } else {
                let mut joined = continued.clone();
                joined.push_str(trimmed);
                continued.clear();
                joined
            };
            push_example(&mut examples, &line);
            continue;
        }
        for span in inline_vivi_spans(trimmed) {
            push_example(&mut examples, &span);
        }
    }
    examples
}

fn inline_vivi_spans(line: &str) -> Vec<String> {
    let mut spans = Vec::new();
    let mut rest = line;
    while let Some(start) = rest.find("`vivi") {
        let after = &rest[start + 1..];
        let Some(end) = after.find('`') else {
            break;
        };
        spans.push(after[..end].to_string());
        rest = &after[end + 1..];
    }
    spans
}

fn push_example(examples: &mut Vec<SkillExample>, line: &str) {
    let Some(tokens) = tokenize_vivi(line) else {
        return;
    };
    if tokens.is_empty() {
        return;
    }
    examples.push(SkillExample {
        display: line.trim().to_string(),
        tokens,
    });
}

fn tokenize_vivi(line: &str) -> Option<Vec<String>> {
    let stripped = strip_comment(line);
    let mut normalized = stripped.replace(['[', ']'], " ");
    normalized = normalized.replace('…', " ");
    normalized = normalized.replace("...", " ");
    let mut words = normalized.split_whitespace();
    let first = words.next()?;
    if first != "vivi" {
        return None;
    }
    Some(
        words
            .map(|word| {
                word.trim_matches(|c: char| {
                    c == '"' || c == '\'' || c == '(' || c == ')' || c == ',' || c == '.'
                })
                .to_string()
            })
            .filter(|word| !word.is_empty() && word != "|" && !word.starts_with('<'))
            .collect(),
    )
}

fn strip_comment(line: &str) -> &str {
    match line.find('#') {
        Some(idx) => &line[..idx],
        None => line,
    }
}

fn validate_example(root: &ClapCommand, tokens: &[String]) -> Result<(), String> {
    let mut cmd = root;
    let mut inherited = flag_set(root);
    let mut i = 0;
    while i < tokens.len() {
        let token = &tokens[i];
        if is_flag(token) {
            let name = flag_name(token);
            if !inherited.contains(&name) && !flag_set(cmd).contains(&name) {
                return Err(format!("unknown flag {name} on `{}`", command_path(cmd)));
            }
            i += 1;
            if i < tokens.len()
                && !is_flag(&tokens[i])
                && find_subcommand(cmd, &tokens[i]).is_none()
            {
                i += 1;
            }
            continue;
        }
        match find_subcommand(cmd, token) {
            Some(sub) => {
                inherited.extend(flag_set(cmd));
                cmd = sub;
                i += 1;
            }
            None => i += 1,
        }
    }
    Ok(())
}

fn find_subcommand<'a>(cmd: &'a ClapCommand, name: &str) -> Option<&'a ClapCommand> {
    cmd.get_subcommands().find(|sub| {
        sub.get_name() == name
            || sub.get_all_aliases().any(|alias| alias == name)
            || sub.get_visible_aliases().any(|alias| alias == name)
    })
}

fn flag_set(cmd: &ClapCommand) -> HashSet<String> {
    let mut flags = HashSet::new();
    for arg in cmd.get_arguments() {
        if let Some(short) = arg.get_short() {
            flags.insert(format!("-{short}"));
        }
        if let Some(long) = arg.get_long() {
            flags.insert(format!("--{long}"));
        }
        if let Some(aliases) = arg.get_visible_aliases() {
            for alias in aliases {
                flags.insert(format!("--{alias}"));
            }
        }
    }
    flags.insert("--force".into());
    flags.insert("--help".into());
    flags.insert("-h".into());
    flags.insert("--version".into());
    flags.insert("-V".into());
    flags
}

fn is_flag(token: &str) -> bool {
    token.starts_with("--") && token.len() > 2 || token.starts_with('-') && token.len() == 2
}

fn flag_name(token: &str) -> String {
    token
        .split_once('=')
        .map_or(token, |(name, _)| name)
        .to_string()
}

fn command_path(cmd: &ClapCommand) -> String {
    cmd.get_display_name()
        .unwrap_or_else(|| cmd.get_name())
        .to_string()
}
