//! The `rungs` argument of `create_project`: parsed and validated before the
//! project is created, written into `[pump.rungs]` after, with the readiness
//! recomputed because the library judged it before the write.

use std::path::Path;

use serde_json::Value;
use vivi::mailspace::Mailspace;
use vivi::project_create::{MissingItem, ProjectCreated, Readiness};
use vivi::pump::config::{RUNG_NAMES, RungsConfig};
use vivi::pump::readiness;

use crate::mcp::tools::ToolError;

/// The rung commands to seed, in `RUNG_NAMES` order.
#[derive(Debug, Default)]
pub(super) struct Rungs(Vec<(&'static str, String)>);

impl Rungs {
    /// Parse the `rungs` argument. Absent or null is no rungs; anything that
    /// is not an object of known rung names to non-blank strings is refused.
    pub(super) fn parse(arguments: &Value) -> Result<Self, ToolError> {
        let object = match arguments.get("rungs") {
            None | Some(Value::Null) => return Ok(Self::default()),
            Some(Value::Object(object)) => object,
            Some(_) => {
                return Err(ToolError::new(
                    "rungs must be an object of rung commands".to_owned(),
                ));
            }
        };
        let allowed = RUNG_NAMES.join(", ");
        if let Some(key) = object.keys().find(|k| !RUNG_NAMES.contains(&k.as_str())) {
            return Err(ToolError::new(format!(
                "unknown rung '{key}'; expected one of {allowed}"
            )));
        }
        let mut parsed = Vec::new();
        for name in RUNG_NAMES {
            let Some(value) = object.get(name) else {
                continue;
            };
            match value.as_str() {
                Some(command) if !command.trim().is_empty() => {
                    parsed.push((name, command.to_owned()));
                }
                _ => {
                    return Err(ToolError::new(format!(
                        "rung '{name}' must be a non-blank command string"
                    )));
                }
            }
        }
        Ok(Self(parsed))
    }

    /// Write the rungs into the created project and, when any was written,
    /// recompute `created.readiness`. Every rung is attempted. There is no
    /// rollback: the error says the project exists.
    pub(super) fn seed(&self, created: &mut ProjectCreated) -> Result<(), String> {
        if self.0.is_empty() {
            return Ok(());
        }
        let errors: Vec<String> = self
            .0
            .iter()
            .filter_map(|(name, command)| {
                RungsConfig::set(&created.project_root, name, command, None)
                    .err()
                    .map(|e| format!("{name}: {e}"))
            })
            .collect();
        let readiness = assess(&created.project_root);
        let mut problems = errors;
        match readiness {
            Ok(fresh) => created.readiness = fresh,
            Err(e) => problems.push(format!("readiness not recomputed: {e}")),
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "the project was created but its rungs were not fully written ({}); \
                 nothing is rolled back, so set them with the project's own tools",
                problems.join("; ")
            ))
        }
    }
}

/// Pump readiness of the project at `root` as it stands now; the same
/// mapping as the library's own summary.
fn assess(root: &Path) -> Result<Readiness, String> {
    let mailspace = Mailspace::open_root(root.to_path_buf()).map_err(|e| e.to_string())?;
    let issues = readiness::assess(&mailspace);
    Ok(Readiness {
        ready: readiness::is_ready(&issues),
        missing: vivi::routing::not_ready(&issues)
            .map(|issue| MissingItem {
                item: issue.item.clone().unwrap_or_default(),
                detail: issue.message.clone(),
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_failed_write_after_creation_names_the_partial_create() {
        let gone = std::env::temp_dir().join(format!("vivi-server-no-such-{}", std::process::id()));
        let mut created = ProjectCreated {
            project_root: gone,
            git_url: String::new(),
            repo_path: std::path::PathBuf::new(),
            mailspace_id: String::new(),
            initial_commit: None,
            readiness: Readiness {
                ready: true,
                missing: Vec::new(),
            },
        };
        let Ok(rungs) = Rungs::parse(&json!({"rungs": {"need": "make need"}})) else {
            panic!("rungs must parse");
        };
        let error = rungs.seed(&mut created).unwrap_err();
        assert!(error.contains("project was created"), "{error}");
        assert!(error.contains("need:"), "{error}");
        assert!(error.contains("rolled back"), "{error}");
    }
}
