//! Where each merged value came from, for `vivi config show | validate`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Serialize;
use toml::{Table, Value};

use super::is_inherited_key;
use super::merge::role_name;

/// A key path: segments, with a role's name standing in `identities`.
type KeyPath = Vec<String>;

/// The files merged and the part each supplied.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ConfigOrigins {
    /// Merged files, farthest base first, the project's own file last.
    pub files: Vec<PathBuf>,
    /// Per section (`pump`, `pools.<name>`, `identities.<role>`, ...), the
    /// files whose values won and the keys each supplied.
    pub entries: Vec<OriginEntry>,
    /// Every `unset` path read, and whether it removed anything.
    pub unset: Vec<UnsetRecord>,
    /// Ancestors that did not parse as TOML and so were ignored.
    pub skipped: Vec<(PathBuf, String)>,
}

/// One section of the merged config and who supplied its keys.
#[derive(Debug, Clone, Serialize)]
pub struct OriginEntry {
    pub section: String,
    /// `(index into files, keys)`, farthest file first.
    pub by_file: Vec<(usize, Vec<String>)>,
}

/// One `unset` path from one file.
#[derive(Debug, Clone, Serialize)]
pub struct UnsetRecord {
    pub file: PathBuf,
    pub path: String,
    pub matched: bool,
}

impl ConfigOrigins {
    /// True when at least one base took part.
    #[must_use]
    pub fn inherited(&self) -> bool {
        self.files.len() > 1
    }

    /// Findings for `vivi config validate`: unmatched `unset` paths and
    /// ancestors that were ignored because they did not parse.
    #[must_use]
    pub fn warnings(&self) -> Vec<String> {
        let unset = self.unset.iter().filter(|record| !record.matched).map(|r| {
            format!(
                "unset \"{}\" in {} matched nothing inherited",
                r.path,
                r.file.display()
            )
        });
        let skipped = self.skipped.iter().map(|(path, error)| {
            format!(
                "ancestor {} is not valid TOML and was ignored: {error}",
                path.display()
            )
        });
        unset.chain(skipped).collect()
    }

    /// The text block shown by `config show` and `config validate`; empty
    /// when no base took part.
    #[must_use]
    pub fn render(&self) -> String {
        if !self.inherited() {
            return String::new();
        }
        let mut lines = vec![String::from(
            "config files (farthest base first, project last):",
        )];
        for (index, file) in self.files.iter().enumerate() {
            let tag = if index + 1 == self.files.len() {
                "project"
            } else {
                "base"
            };
            lines.push(format!("  {} [{tag}] {}", index + 1, file.display()));
        }
        lines.push(String::from("origins:"));
        for entry in &self.entries {
            let parts: Vec<String> = entry
                .by_file
                .iter()
                .map(|(file, keys)| format!("{} ({})", file + 1, keys.join(", ")))
                .collect();
            lines.push(format!(
                "  {}: file {}",
                entry.section,
                parts.join("; file ")
            ));
        }
        for record in self.unset.iter().filter(|record| record.matched) {
            lines.push(format!(
                "  unset {} by {}",
                record.path,
                record.file.display()
            ));
        }
        lines.push(String::new());
        lines.join("\n")
    }
}

/// Accumulates which file's value is currently winning each key.
pub(super) struct Tracker {
    files: Vec<PathBuf>,
    winners: BTreeMap<KeyPath, usize>,
    unset: Vec<UnsetRecord>,
}

impl Tracker {
    pub(super) fn new(files: Vec<PathBuf>) -> Self {
        Self {
            files,
            winners: BTreeMap::new(),
            unset: Vec::new(),
        }
    }

    /// Note an `unset` path read from file `index`; when it removed values,
    /// drop their winners.
    pub(super) fn unset(&mut self, index: usize, path: String, segments: &[String], matched: bool) {
        if matched {
            self.winners.retain(|key, _| !key.starts_with(segments));
        }
        self.unset.push(UnsetRecord {
            file: self.files[index].clone(),
            path,
            matched,
        });
    }

    /// Note the values file `index` supplies, which now win.
    pub(super) fn record(&mut self, index: usize, table: &Table) {
        let mut leaves = Vec::new();
        for (key, value) in table {
            if is_inherited_key(key) {
                collect_leaves(vec![key.clone()], value, &mut leaves);
            }
        }
        for leaf in leaves {
            self.winners
                .retain(|key, _| !key.starts_with(&leaf) && !leaf.starts_with(key));
            self.winners.insert(leaf, index);
        }
    }

    pub(super) fn finish(self, skipped: Vec<(PathBuf, String)>) -> ConfigOrigins {
        let mut sections: BTreeMap<String, BTreeMap<usize, Vec<String>>> = BTreeMap::new();
        for (key, file) in &self.winners {
            let depth = if key.len() > 1 && matches!(key[0].as_str(), "pools" | "identities") {
                2
            } else {
                1
            };
            let leaf = key[depth..].join(".");
            let leaf = if leaf.is_empty() {
                "(value)".into()
            } else {
                leaf
            };
            sections
                .entry(key[..depth].join("."))
                .or_default()
                .entry(*file)
                .or_default()
                .push(leaf);
        }
        let entries = sections
            .into_iter()
            .map(|(section, files)| OriginEntry {
                section,
                by_file: files.into_iter().collect(),
            })
            .collect();
        ConfigOrigins {
            files: self.files,
            entries,
            unset: self.unset,
            skipped,
        }
    }
}

fn collect_leaves(path: KeyPath, value: &Value, out: &mut Vec<KeyPath>) {
    match value {
        Value::Table(table) => {
            for (key, inner) in table {
                let mut next = path.clone();
                next.push(key.clone());
                collect_leaves(next, inner, out);
            }
        }
        Value::Array(roles) if path == ["identities"] => {
            for role in roles {
                let (Some(name), Value::Table(table)) = (role_name(role), role) else {
                    continue;
                };
                for (key, inner) in table {
                    let mut next = path.clone();
                    next.push(name.to_owned());
                    next.push(key.clone());
                    collect_leaves(next, inner, out);
                }
            }
        }
        _ => out.push(path),
    }
}
