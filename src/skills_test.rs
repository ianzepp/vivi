use std::fs;
use std::path::{Path, PathBuf};

use tempfile::tempdir;

use crate::skills::{self, Skill, SkillCopyStatus, catalog};

#[test]
fn catalog_has_the_three_shipped_skills_at_crate_version() {
    let names: Vec<_> = catalog().iter().map(|skill| skill.name).collect();
    assert_eq!(names, ["vivi", "tugboat", "speedboat"]);
    for skill in catalog() {
        assert_eq!(skill.version, env!("CARGO_PKG_VERSION"));
        assert!(!skill.description.is_empty(), "{}", skill.name);
        assert!(
            skill.file_names().contains(&"SKILL.md"),
            "{} missing SKILL.md",
            skill.name
        );
    }
}

#[test]
fn embedded_bytes_match_the_skills_tree() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("skills");
    let mut disk = Vec::new();
    collect_files(&root, &root, &mut disk);
    disk.sort();

    let mut embedded = Vec::new();
    for skill in catalog() {
        for path in skill.file_names() {
            let rel = format!("{}/{}", skill.name, path);
            let on_disk = fs::read_to_string(root.join(&rel)).unwrap();
            let shown = skills::show(skill.name, Some(path)).unwrap();
            assert_eq!(shown.contents, on_disk, "{rel}");
            embedded.push(rel);
        }
    }
    embedded.sort();
    assert_eq!(embedded, disk);
}

#[test]
fn show_defaults_to_skill_md_and_rejects_escapes() {
    let shown = skills::show("vivi", None).unwrap();
    assert_eq!(shown.file, "SKILL.md");
    assert!(shown.contents.contains("# Vivi"));

    let flow = skills::show("vivi", Some("references/work-graph-flow.md")).unwrap();
    assert!(flow.contents.contains("Work-graph"));

    let contract = skills::show("vivi", Some("references/pump-contract.md")).unwrap();
    assert!(contract.contents.starts_with("# Pump contract"));
    let protocols = skills::show("vivi", Some("references/pump-protocols.md")).unwrap();
    assert!(protocols.contents.contains("Pump protocols"));

    let err = skills::show("nope", None).unwrap_err().to_string();
    assert!(err.contains("unknown skill 'nope'"), "{err}");
    assert!(err.contains("vivi"), "{err}");

    let err = skills::show("vivi", Some("../Cargo.toml"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("inside the skill"), "{err}");

    let err = skills::show("vivi", Some("missing.md"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("unknown file 'missing.md'"), "{err}");
    assert!(err.contains("SKILL.md"), "{err}");
}

#[test]
fn install_writes_the_layout_and_force_overwrites() {
    let dir = tempdir().unwrap();
    let dest = dir.path().join("skills");
    let first = skills::install(&dest, false).unwrap();
    assert_eq!(first.skills, 3);
    assert!(first.files >= 3);

    let skill_md = dest.join("vivi/SKILL.md");
    assert_eq!(
        fs::read_to_string(&skill_md).unwrap(),
        skills::show("vivi", None).unwrap().contents
    );
    assert!(dest.join("vivi/references/work-graph-flow.md").is_file());
    assert!(dest.join("vivi/references/pump-contract.md").is_file());
    assert!(dest.join("vivi/references/pump-protocols.md").is_file());
    assert!(dest.join("tugboat/scripts/check-task-body.py").is_file());
    assert!(dest.join("tugboat/references/charters/hand.md").is_file());
    assert!(dest.join("speedboat/agents/openai.yaml").is_file());

    fs::write(&skill_md, "stale\n").unwrap();
    let err = skills::install(&dest, false).unwrap_err().to_string();
    assert!(err.contains("already exists"), "{err}");
    assert_eq!(fs::read_to_string(&skill_md).unwrap(), "stale\n");

    skills::install(&dest, true).unwrap();
    assert_eq!(
        fs::read_to_string(&skill_md).unwrap(),
        skills::show("vivi", None).unwrap().contents
    );
}

#[test]
fn install_refuses_a_file_destination() {
    let dir = tempdir().unwrap();
    let dest = dir.path().join("not-a-dir");
    fs::write(&dest, "nope\n").unwrap();
    let err = skills::install(&dest, true).unwrap_err().to_string();
    assert!(err.contains("is a file"), "{err}");
}

fn collect_files(root: &Path, dir: &Path, files: &mut Vec<String>) {
    let mut entries: Vec<_> = fs::read_dir(dir).unwrap().map(|e| e.unwrap()).collect();
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        if path.is_dir() {
            collect_files(root, &path, files);
            continue;
        }
        if path.is_file() {
            let rel = path.strip_prefix(root).unwrap();
            files.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
}

#[test]
fn tugboat_charter_relative_links_survive_install() {
    let dir = tempdir().unwrap();
    skills::install(dir.path(), false).unwrap();
    let skill = fs::read_to_string(dir.path().join("tugboat/SKILL.md")).unwrap();
    let linked = PathBuf::from("references/charters/hand.md");
    assert!(
        skill.contains(linked.to_str().unwrap()) || skill.contains("references/charters/"),
        "tugboat skill should point at charters"
    );
    assert!(dir.path().join("tugboat").join(linked).is_file());
}

/// Every relative link in an embedded vivi skill file must name an embedded
/// file, so an installed skill carries no dead links.
#[test]
fn relative_links_in_the_vivi_skill_resolve_to_embedded_files() {
    let skill = skills::get("vivi").unwrap();
    let files = skill.file_names();
    for file in &files {
        if !file.ends_with(".md") {
            continue;
        }
        let dir = Path::new(file).parent().unwrap_or(Path::new(""));
        let text = skills::show("vivi", Some(file)).unwrap().contents;
        for target in link_targets(text) {
            if target.contains("://") || target.starts_with('#') || target.starts_with("mailto:") {
                continue;
            }
            let path = target.split('#').next().unwrap_or_default();
            let resolved = normalize(&dir.join(path));
            assert!(
                files.contains(&resolved.as_str()),
                "{file} links to {target}, which is not an embedded file"
            );
        }
    }
}

/// The `(...)` targets of inline Markdown links `[text](target)`.
fn link_targets(text: &str) -> Vec<&str> {
    let mut targets = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("](") {
        rest = &rest[at + 2..];
        if let Some(end) = rest.find(')') {
            targets.push(&rest[..end]);
        }
    }
    targets
}

fn normalize(path: &Path) -> String {
    let mut parts: Vec<String> = Vec::new();
    for part in path.components() {
        match part.as_os_str().to_string_lossy().as_ref() {
            "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other.to_string()),
        }
    }
    parts.join("/")
}

#[test]
fn copy_status_flags_a_modified_skill() {
    let dir = tempdir().unwrap();
    write_fresh_copy(dir.path());
    fs::write(dir.path().join("tugboat/SKILL.md"), "different bytes\n").unwrap();

    match skills::copy_status(dir.path()) {
        SkillCopyStatus::Stale(names) => {
            assert!(names.contains(&"tugboat".to_string()), "{names:?}");
            assert_eq!(names, ["tugboat"], "{names:?}");
        }
        other => panic!("expected stale, got {other:?}"),
    }
}

#[test]
fn copy_status_up_to_date_and_absent() {
    let dir = tempdir().unwrap();
    write_fresh_copy(dir.path());
    assert_eq!(skills::copy_status(dir.path()), SkillCopyStatus::UpToDate);

    let empty = tempdir().unwrap();
    assert_eq!(
        skills::copy_status(empty.path()),
        SkillCopyStatus::NotInstalled
    );
}

#[test]
fn catalog_matches_json_detects_change() {
    let payload: Vec<_> = catalog().iter().map(Skill::summary).collect();
    let json = serde_json::to_string(&payload).unwrap();
    assert_eq!(skills::catalog_matches_json(&json), Some(false));

    let version = env!("CARGO_PKG_VERSION");
    let bumped = json.replacen(version, "0.0.0-changed", 1);
    assert_ne!(bumped, json);
    assert_eq!(skills::catalog_matches_json(&bumped), Some(true));

    assert_eq!(skills::catalog_matches_json("not json"), None);
}

#[test]
fn recorded_targets_in_reads_lines_and_missing() {
    let config = tempdir().unwrap();
    assert!(skills::recorded_targets_in(config.path()).is_empty());

    let path = config.path().join("vivi").join("skills-targets.txt");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "/tmp/one\n\n  /tmp/two  \n").unwrap();
    assert_eq!(
        skills::recorded_targets_in(config.path()),
        vec![PathBuf::from("/tmp/one"), PathBuf::from("/tmp/two")]
    );
}

#[test]
fn record_target_appends_and_dedups() {
    let config = tempdir().unwrap();
    let first = tempdir().unwrap();
    let second = tempdir().unwrap();
    skills::record_target_in(config.path(), first.path()).unwrap();
    skills::record_target_in(config.path(), second.path()).unwrap();
    skills::record_target_in(config.path(), first.path()).unwrap();

    let path = config.path().join("vivi").join("skills-targets.txt");
    let text = fs::read_to_string(&path).unwrap();
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    for line in lines {
        assert!(Path::new(line).is_absolute(), "{line}");
        assert!(Path::new(line).is_dir(), "{line}");
    }
}

/// Lay down every catalog skill, file for file, exactly as embedded.
fn write_fresh_copy(dir: &Path) {
    for skill in catalog() {
        for file in &skill.file_names() {
            let shown = skills::show(skill.name, Some(file)).unwrap();
            let dest = dir.join(skill.name).join(file);
            fs::create_dir_all(dest.parent().unwrap()).unwrap();
            fs::write(&dest, shown.contents).unwrap();
        }
    }
}
