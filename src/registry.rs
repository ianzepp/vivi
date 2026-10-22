//! Host project registry shared by the pump supervisor and servers.
//!
//! The registry is one TOML file, `<config_dir>/vivi/pump-service.toml`
//! (`VIVI_CONFIG_DIR` overrides `<config_dir>`). Each registered project root
//! carries two independent flags: `pump` (the pump supervisor runs a pump for
//! it) and `serve` (a server hosts it).
//!
//! The file is written as `[[project]]` tables:
//!
//! ```toml
//! [[project]]
//! root = "/work/app"
//! pump = true
//! serve = false
//! ```
//!
//! The earlier format, a bare `projects = ["/path", ...]` list, is still read
//! and means `pump = true, serve = false` for each path. Any write migrates
//! the file to the table format.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::VivariumError;

const CONFIG_DIRECTORY: &str = "vivi";
const CONFIG_FILENAME: &str = "pump-service.toml";
const ENV_CONFIG_DIR: &str = "VIVI_CONFIG_DIR";

/// What the host does with a registered project.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Flags {
    /// The pump supervisor runs a pump for the project.
    pub pump: bool,
    /// A server hosts the project.
    pub serve: bool,
}

impl Default for Flags {
    /// A new registration is pumped and not served.
    fn default() -> Self {
        Self {
            pump: true,
            serve: false,
        }
    }
}

/// One registered project.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    /// Project root that owns `.vivi/`.
    pub root: PathBuf,
    /// Pump and serve flags for the root.
    pub flags: Flags,
}

/// The parsed registry, keyed by project root.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Registry {
    projects: BTreeMap<PathBuf, Flags>,
}

/// Cheap change marker: equal fingerprints mean the file content is unchanged.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Fingerprint(Option<[u8; 32]>);

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FileFormat {
    /// Legacy format: bare roots, meaning pump on and serve off.
    #[serde(default, skip_serializing)]
    projects: BTreeSet<PathBuf>,
    #[serde(default, rename = "project")]
    entries: Vec<FileEntry>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FileEntry {
    root: PathBuf,
    #[serde(default = "default_pump")]
    pump: bool,
    #[serde(default)]
    serve: bool,
}

fn default_pump() -> bool {
    true
}

impl Registry {
    /// Every entry, ordered by root.
    #[must_use]
    pub fn entries(&self) -> Vec<Entry> {
        self.projects
            .iter()
            .map(|(root, flags)| Entry {
                root: root.clone(),
                flags: *flags,
            })
            .collect()
    }

    /// Flags for one root, if registered.
    #[must_use]
    pub fn get(&self, root: &Path) -> Option<Flags> {
        self.projects.get(root).copied()
    }

    /// Every registered root regardless of flags.
    #[must_use]
    pub fn roots(&self) -> BTreeSet<PathBuf> {
        self.projects.keys().cloned().collect()
    }

    /// Roots with `pump = true`: the set the pump supervisor runs.
    #[must_use]
    pub fn pumped(&self) -> BTreeSet<PathBuf> {
        self.filtered(|flags| flags.pump)
    }

    /// Roots with `serve = true`: the set a server hosts.
    #[must_use]
    pub fn served(&self) -> BTreeSet<PathBuf> {
        self.filtered(|flags| flags.serve)
    }

    fn filtered(&self, keep: impl Fn(&Flags) -> bool) -> BTreeSet<PathBuf> {
        self.projects
            .iter()
            .filter(|(_, flags)| keep(flags))
            .map(|(root, _)| root.clone())
            .collect()
    }

    /// Register a root with `flags` unless it is already registered.
    /// Returns true when the root was newly added; an existing entry is untouched.
    pub fn add(&mut self, root: PathBuf, flags: Flags) -> bool {
        if self.projects.contains_key(&root) {
            return false;
        }
        self.projects.insert(root, flags);
        true
    }

    /// Set the flags for a root, registering it if needed.
    /// Returns true when the registry changed.
    pub fn set(&mut self, root: PathBuf, flags: Flags) -> bool {
        self.projects.insert(root, flags) != Some(flags)
    }

    /// Unregister a root. Returns true when it was registered.
    pub fn remove(&mut self, root: &Path) -> bool {
        self.projects.remove(root).is_some()
    }

    fn parse(content: &str) -> Result<Self, toml::de::Error> {
        let file: FileFormat = toml::from_str(content)?;
        let mut registry = Self::default();
        for root in file.projects {
            registry.projects.insert(root, Flags::default());
        }
        for entry in file.entries {
            let flags = Flags {
                pump: entry.pump,
                serve: entry.serve,
            };
            registry.projects.insert(entry.root, flags);
        }
        Ok(registry)
    }

    fn render(&self) -> Result<String, VivariumError> {
        let file = FileFormat {
            projects: BTreeSet::new(),
            entries: self
                .projects
                .iter()
                .map(|(root, flags)| FileEntry {
                    root: root.clone(),
                    pump: flags.pump,
                    serve: flags.serve,
                })
                .collect(),
        };
        toml::to_string_pretty(&file)
            .map_err(|error| VivariumError::Config(format!("could not encode registry: {error}")))
    }
}

/// Path of the registry file.
///
/// # Errors
/// Fails when no config directory can be found.
pub fn path() -> Result<PathBuf, VivariumError> {
    let root = match std::env::var(ENV_CONFIG_DIR) {
        Ok(value) if !value.is_empty() => PathBuf::from(value),
        Ok(_) | Err(_) => dirs::config_dir().ok_or_else(|| {
            VivariumError::Config("could not find the user's config directory".into())
        })?,
    };
    Ok(root.join(CONFIG_DIRECTORY).join(CONFIG_FILENAME))
}

/// Load the registry; a missing file is an empty registry.
///
/// # Errors
/// Fails on an unreadable or invalid file.
pub fn load() -> Result<Registry, VivariumError> {
    load_at(&path()?)
}

/// Load the registry from an explicit path.
///
/// # Errors
/// Fails on an unreadable or invalid file.
pub fn load_at(path: &Path) -> Result<Registry, VivariumError> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Registry::default());
        }
        Err(error) => return Err(error.into()),
    };
    Registry::parse(&content)
        .map_err(|error| VivariumError::Config(format!("invalid {}: {error}", path.display())))
}

/// Write the registry atomically in the table format.
///
/// # Errors
/// Fails when the file cannot be written.
pub fn save(registry: &Registry) -> Result<(), VivariumError> {
    save_at(&path()?, registry)
}

/// Write the registry atomically to an explicit path.
///
/// # Errors
/// Fails when the file cannot be written.
pub fn save_at(path: &Path, registry: &Registry) -> Result<(), VivariumError> {
    let parent = path
        .parent()
        .ok_or_else(|| VivariumError::Config("registry has no parent directory".into()))?;
    fs::create_dir_all(parent)?;
    let temporary = path.with_extension("toml.tmp");
    fs::write(&temporary, registry.render()?)?;
    fs::rename(temporary, path)?;
    Ok(())
}

/// Load, change, and save the registry under the registration lock.
/// The file is rewritten only when `change` returns `Ok`.
///
/// # Errors
/// Fails when another writer holds the lock, on I/O errors, or when
/// `change` fails.
pub fn update<T>(
    change: impl FnOnce(&mut Registry) -> Result<T, VivariumError>,
) -> Result<T, VivariumError> {
    update_at(&path()?, change)
}

/// [`update`] on an explicit registry path.
///
/// # Errors
/// As [`update`].
pub fn update_at<T>(
    file: &Path,
    change: impl FnOnce(&mut Registry) -> Result<T, VivariumError>,
) -> Result<T, VivariumError> {
    let _lock = lock_file(
        file.with_extension("config.lock"),
        "another Vivi registry update is running",
    )?;
    let mut registry = load_at(file)?;
    let value = change(&mut registry)?;
    save_at(file, &registry)?;
    Ok(value)
}

/// Change marker for a watcher: compare against the previous value and reload
/// when it differs. A missing file has the default fingerprint.
///
/// # Errors
/// Fails when the file exists but cannot be read.
pub fn fingerprint() -> Result<Fingerprint, VivariumError> {
    fingerprint_at(&path()?)
}

/// Change marker for an explicit path.
///
/// # Errors
/// Fails when the file exists but cannot be read.
pub fn fingerprint_at(path: &Path) -> Result<Fingerprint, VivariumError> {
    match fs::read(path) {
        Ok(bytes) => Ok(Fingerprint(Some(Sha256::digest(bytes).into()))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Fingerprint::default()),
        Err(error) => Err(error.into()),
    }
}

/// Take a non-blocking exclusive lock on `path`, creating it if needed.
pub(crate) fn lock_file(path: PathBuf, message: &str) -> Result<File, VivariumError> {
    let parent = path
        .parent()
        .ok_or_else(|| VivariumError::Config("lock file has no parent directory".into()))?;
    fs::create_dir_all(parent)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)?;
    file.try_lock()
        .map_err(|_| VivariumError::Message(message.to_string()))?;
    Ok(file)
}

#[cfg(test)]
#[path = "registry_test.rs"]
mod tests;
