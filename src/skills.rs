//! Agent skills embedded in this binary.
//!
//! The catalog is the `skills/` tree at compile time. `vivi --version` and
//! every skill's `version` field are the same crate version, so an agent
//! running an installed binary reads text that matches that CLI.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::Serialize;

use crate::VivariumError;

include!(concat!(env!("OUT_DIR"), "/embedded_skills.rs"));

const VERSION: &str = env!("CARGO_PKG_VERSION");
const SKILL_MD: &str = "SKILL.md";

/// One embedded skill: `SKILL.md` plus the supporting files it links to.
#[derive(Debug)]
pub struct Skill {
    /// Directory name (`vivi`, `tugboat`, `speedboat`).
    pub name: &'static str,
    /// Crate version of the binary that embedded this tree.
    pub version: &'static str,
    /// One-line description from the `SKILL.md` frontmatter.
    pub description: String,
    files: Vec<SkillFile>,
}

#[derive(Debug, Clone, Copy)]
struct SkillFile {
    path: &'static str,
    contents: &'static str,
}

/// Compact listing used by `vivi skills list --json`.
#[derive(Debug, Serialize)]
pub struct SkillSummary {
    /// Skill directory name.
    pub name: &'static str,
    /// Binary version.
    pub version: &'static str,
    /// Frontmatter description.
    pub description: String,
    /// Paths relative to the skill root, including `SKILL.md`.
    pub files: Vec<&'static str>,
}

/// A file chosen by `vivi skills show`.
#[derive(Debug)]
pub struct SkillShow {
    /// Skill the file belongs to.
    pub skill: &'static Skill,
    /// Path relative to the skill root.
    pub file: &'static str,
    /// Exact embedded bytes, as UTF-8.
    pub contents: &'static str,
}

/// Receipt from `vivi skills install`.
#[derive(Debug)]
pub struct InstallReport {
    /// Directory the skill trees were written into.
    pub dir: PathBuf,
    /// Number of skills written.
    pub skills: usize,
    /// Number of files written.
    pub files: usize,
}

/// Whether an on-disk copy still matches the embedded catalog.
#[derive(Debug, PartialEq, Eq)]
pub enum SkillCopyStatus {
    /// No catalog skill was found in the directory.
    NotInstalled,
    /// Every present skill matches the embedded bytes.
    UpToDate,
    /// Names of present skills with a missing or modified file.
    Stale(Vec<String>),
}

/// Embedded skills, `vivi` first, then the rest in repository order.
#[must_use]
pub fn catalog() -> &'static [Skill] {
    static CATALOG: OnceLock<Vec<Skill>> = OnceLock::new();
    CATALOG.get_or_init(build_catalog)
}

/// Look up one skill by directory name.
///
/// # Errors
///
/// Returns an error naming the known skills when `name` is not in the catalog.
pub fn get(name: &str) -> Result<&'static Skill, VivariumError> {
    catalog()
        .iter()
        .find(|skill| skill.name == name)
        .ok_or_else(|| {
            let known = catalog()
                .iter()
                .map(|skill| skill.name)
                .collect::<Vec<_>>()
                .join(", ");
            VivariumError::Other(format!("unknown skill '{name}' ({known})"))
        })
}

/// Load `SKILL.md` or one supporting file.
///
/// # Errors
///
/// Returns an error when the skill or the relative file path is unknown, or
/// when `file` escapes the skill root.
pub fn show(name: &str, file: Option<&str>) -> Result<SkillShow, VivariumError> {
    let skill = get(name)?;
    let requested = match file {
        Some(path) => normalize_file_path(path)?,
        None => SKILL_MD.to_string(),
    };
    skill
        .file(&requested)
        .ok_or_else(|| unknown_file(name, &requested, skill))
}

/// Write every embedded skill into `dir`, keeping the repository layout.
///
/// # Errors
///
/// Returns an error when `dir` is a file, a target file already exists and
/// `force` is false, or a write fails.
pub fn install(dir: &Path, force: bool) -> Result<InstallReport, VivariumError> {
    if dir.is_file() {
        return Err(VivariumError::Other(format!("{} is a file", dir.display())));
    }
    let skills = catalog();
    if !force {
        refuse_existing(dir, skills)?;
    }
    let mut files = 0;
    for skill in skills {
        files += write_skill(dir, skill)?;
    }
    Ok(InstallReport {
        dir: dir.to_path_buf(),
        skills: skills.len(),
        files,
    })
}

/// Compare an installed skill directory against the embedded catalog.
///
/// A skill counts as present when its `SKILL.md` exists; skills absent from
/// `dir` are skipped, and extra files on disk are ignored. Only bytes matter,
/// not file modes.
#[must_use]
pub fn copy_status(dir: &Path) -> SkillCopyStatus {
    let mut present = 0;
    let mut stale = Vec::new();
    for skill in catalog() {
        let root = dir.join(skill.name);
        if !root.join(SKILL_MD).is_file() {
            continue;
        }
        present += 1;
        if skill.files.iter().any(|file| differs(&root, file)) {
            stale.push(skill.name.to_string());
        }
    }
    if present == 0 {
        return SkillCopyStatus::NotInstalled;
    }
    if stale.is_empty() {
        SkillCopyStatus::UpToDate
    } else {
        SkillCopyStatus::Stale(stale)
    }
}

fn differs(root: &Path, file: &SkillFile) -> bool {
    match fs::read(root.join(file.path)) {
        Ok(bytes) => bytes != file.contents.as_bytes(),
        Err(_) => true,
    }
}

impl Skill {
    /// Paths relative to the skill root, sorted.
    #[must_use]
    pub fn file_names(&self) -> Vec<&'static str> {
        self.files.iter().map(|file| file.path).collect()
    }

    /// JSON listing for this skill.
    #[must_use]
    pub fn summary(&self) -> SkillSummary {
        SkillSummary {
            name: self.name,
            version: self.version,
            description: self.description.clone(),
            files: self.file_names(),
        }
    }

    fn file(&'static self, path: &str) -> Option<SkillShow> {
        self.files
            .iter()
            .find(|file| file.path == path)
            .map(|file| SkillShow {
                skill: self,
                file: file.path,
                contents: file.contents,
            })
    }
}

fn build_catalog() -> Vec<Skill> {
    let mut grouped: BTreeMap<&str, Vec<SkillFile>> = BTreeMap::new();
    for (path, contents) in EMBEDDED_FILES {
        let Some((name, rel)) = path.split_once('/') else {
            continue;
        };
        grouped.entry(name).or_default().push(SkillFile {
            path: rel,
            contents,
        });
    }
    let mut skills: Vec<Skill> = grouped
        .into_iter()
        .filter_map(|(name, files)| skill_from_files(name, files))
        .collect();
    skills.sort_by_key(|skill| skill_order(skill.name));
    skills
}

fn skill_from_files(name: &'static str, mut files: Vec<SkillFile>) -> Option<Skill> {
    files.sort_by_key(|file| file.path);
    let skill_md = files.iter().find(|file| file.path == SKILL_MD)?.contents;
    Some(Skill {
        name,
        version: VERSION,
        description: parse_frontmatter_description(skill_md),
        files,
    })
}

fn skill_order(name: &str) -> (u8, &str) {
    match name {
        "vivi" => (0, name),
        "tugboat" => (1, name),
        "speedboat" => (2, name),
        other => (3, other),
    }
}

fn parse_frontmatter_description(text: &str) -> String {
    let Some(block) = frontmatter_block(text) else {
        return String::new();
    };
    for line in block.lines() {
        let Some(value) = line.strip_prefix("description:") else {
            continue;
        };
        return value.trim().to_string();
    }
    String::new()
}

fn frontmatter_block(text: &str) -> Option<&str> {
    let rest = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))?;
    rest.split_once("\n---").map(|(block, _)| block)
}

fn normalize_file_path(path: &str) -> Result<String, VivariumError> {
    let path = path.trim().replace('\\', "/");
    if path.is_empty() || Path::new(&path).is_absolute() || escapes_skill(&path) {
        return Err(VivariumError::Other(format!(
            "file must be a path inside the skill (got {path})"
        )));
    }
    Ok(path)
}

fn escapes_skill(path: &str) -> bool {
    path.split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
}

fn unknown_file(name: &str, requested: &str, skill: &Skill) -> VivariumError {
    let mut message = format!("unknown file '{requested}' in skill '{name}'\nfiles:");
    for path in skill.file_names() {
        message.push_str("\n  ");
        message.push_str(path);
    }
    VivariumError::Other(message)
}

fn refuse_existing(dir: &Path, skills: &[Skill]) -> Result<(), VivariumError> {
    for skill in skills {
        for file in &skill.files {
            let dest = dir.join(skill.name).join(file.path);
            if dest.is_file() {
                return Err(VivariumError::Other(format!(
                    "{} already exists (pass --force to overwrite)",
                    dest.display()
                )));
            }
        }
    }
    Ok(())
}

fn write_skill(dir: &Path, skill: &Skill) -> Result<usize, VivariumError> {
    for file in &skill.files {
        let dest = dir.join(skill.name).join(file.path);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&dest, file.contents)?;
        set_script_mode(&dest, file.path)?;
    }
    Ok(skill.files.len())
}

#[cfg(unix)]
fn set_script_mode(dest: &Path, rel: &str) -> Result<(), VivariumError> {
    use std::os::unix::fs::PermissionsExt;
    if !rel.starts_with("scripts/") {
        return Ok(());
    }
    let mut perms = fs::metadata(dest)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(dest, perms)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_script_mode(_dest: &Path, _rel: &str) -> Result<(), VivariumError> {
    Ok(())
}

#[cfg(test)]
#[path = "skills_test.rs"]
mod tests;
