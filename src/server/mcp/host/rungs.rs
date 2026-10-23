//! The `rungs` argument of `create_project`: parsed and validated before the
//! project is created, written into `[pump.rungs]` after, with the readiness
//! recomputed because the library judged it before the write.

use std::path::Path;

use serde_json::Value;
use vivi::VivariumError;
use vivi::mailspace::{Mailspace, config_edit};
use vivi::project_create::{MissingItem, ProjectCreated, Readiness};
use vivi::pump::config::{RUNG_NAMES, RungsConfig};
use vivi::pump::readiness;

use crate::mcp::tools::ToolError;

/// The rung commands, timeout and pools to seed; commands in `RUNG_NAMES`
/// order.
#[derive(Debug, Default)]
pub(super) struct Rungs {
    commands: Vec<(&'static str, String)>,
    timeout: Option<i64>,
    pools: Option<Vec<String>>,
}

impl Rungs {
    /// Parse the `rungs` argument. Absent or null is no rungs; anything that
    /// is not an object of known rung names to non-blank strings, plus an
    /// optional `timeout` and `pools`, is refused.
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
        let known = |k: &str| RUNG_NAMES.contains(&k) || k == "timeout" || k == "pools";
        if let Some(key) = object.keys().find(|k| !known(k)) {
            return Err(ToolError::new(format!(
                "unknown rung '{key}'; expected one of {allowed}, timeout, pools"
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
        Ok(Self {
            commands: parsed,
            timeout: parse_timeout(object.get("timeout"))?,
            pools: parse_pools(object.get("pools"))?,
        })
    }

    fn is_empty(&self) -> bool {
        self.commands.is_empty() && self.timeout.is_none() && self.pools.is_none()
    }

    /// Write the rungs into the created project and, when any was written,
    /// recompute `created.readiness`. Every rung is attempted. There is no
    /// rollback: the error says the project exists.
    pub(super) fn seed(&self, created: &mut ProjectCreated) -> Result<(), String> {
        if self.is_empty() {
            return Ok(());
        }
        let mut errors: Vec<String> = self
            .commands
            .iter()
            .filter_map(|(name, command)| {
                RungsConfig::set(&created.project_root, name, command, None)
                    .err()
                    .map(|e| format!("{name}: {e}"))
            })
            .collect();
        if let Err(e) = self.write_settings(&created.project_root) {
            errors.push(format!("timeout/pools: {e}"));
        }
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

impl Rungs {
    /// Write `timeout` and `pools` into `[pump.rungs]` of the project's
    /// config, format-preserving and under the library's config lock.
    fn write_settings(&self, root: &Path) -> Result<(), String> {
        if self.timeout.is_none() && self.pools.is_none() {
            return Ok(());
        }
        let path = root.join(".vivi").join("mailspace.toml");
        config_edit::apply(&path, |doc| {
            let pump = implicit_table(doc.as_table_mut(), "pump")?;
            let rungs = implicit_table(pump, "rungs")?;
            if let Some(timeout) = self.timeout {
                rungs["timeout"] = toml_edit::value(timeout);
            }
            if let Some(pools) = &self.pools {
                let array: toml_edit::Array = pools.iter().map(String::as_str).collect();
                rungs["pools"] = toml_edit::value(array);
            }
            Ok(())
        })
        .map_err(|e| e.to_string())
    }
}

/// The child table `key`, created implicit if missing.
fn implicit_table<'a>(
    table: &'a mut toml_edit::Table,
    key: &str,
) -> Result<&'a mut toml_edit::Table, VivariumError> {
    let item = table.entry(key).or_insert_with(|| {
        let mut created = toml_edit::Table::new();
        created.set_implicit(true);
        toml_edit::Item::Table(created)
    });
    item.as_table_mut()
        .ok_or_else(|| VivariumError::Config(format!("`{key}` must be a table")))
}

fn parse_timeout(value: Option<&Value>) -> Result<Option<i64>, ToolError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(v) => match v.as_i64() {
            Some(seconds) if seconds >= 1 => Ok(Some(seconds)),
            _ => Err(ToolError::new(
                "rungs.timeout must be a whole number of seconds, at least 1".to_owned(),
            )),
        },
    }
}

fn parse_pools(value: Option<&Value>) -> Result<Option<Vec<String>>, ToolError> {
    let bad = || ToolError::new("rungs.pools must be an array of non-empty pool names".to_owned());
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| match item.as_str() {
                Some(name) if !name.trim().is_empty() => Ok(name.to_owned()),
                _ => Err(bad()),
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        Some(_) => Err(bad()),
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
