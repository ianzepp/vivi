//! Durable operator control state for a project pump.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use crate::VivariumError;
use crate::mailspace::Mailspace;

use super::config::PumpConfig;
use super::store::{DB_FILENAME, PumpStore, SCHEMA_VERSION};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PumpMode {
    Running,
    Paused,
    Stopped,
}

impl PumpMode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Stopped => "stopped",
        }
    }

    fn parse(value: &str) -> Result<Self, VivariumError> {
        match value {
            "running" => Ok(Self::Running),
            "paused" => Ok(Self::Paused),
            "stopped" => Ok(Self::Stopped),
            other => Err(VivariumError::Message(format!(
                "unknown pump control mode {other:?}"
            ))),
        }
    }
}

impl fmt::Display for PumpMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PumpControl {
    pub mode: PumpMode,
    pub shutdown_requested: bool,
}

/// Initialize control state and migrate the old marker files once.
pub fn initialize(home: &Path) -> Result<PumpControl, VivariumError> {
    let stop = home.join("STOP");
    let pause = home.join("PAUSE");
    let legacy_mode = if stop.exists() {
        PumpMode::Stopped
    } else if pause.exists() {
        PumpMode::Paused
    } else {
        PumpMode::Running
    };
    let mut store = PumpStore::open(&home.join(DB_FILENAME))?;
    let mode = PumpMode::parse(&store.initialize_control(legacy_mode.as_str())?)?;
    remove_legacy_flag(&stop);
    remove_legacy_flag(&pause);
    Ok(PumpControl {
        mode,
        shutdown_requested: store.shutdown_requested()?,
    })
}

/// Read control state from an already-initialized pump database.
pub fn read(home: &Path) -> Result<PumpControl, VivariumError> {
    Ok(read_initialized(home)?.unwrap_or(PumpControl {
        mode: PumpMode::Running,
        shutdown_requested: false,
    }))
}

fn read_initialized(home: &Path) -> Result<Option<PumpControl>, VivariumError> {
    let path = home.join(DB_FILENAME);
    let Some(store) = PumpStore::open_read_only(&path)? else {
        return Ok(None);
    };
    let version = store.schema_version()?;
    if version > SCHEMA_VERSION {
        return Err(VivariumError::Message(format!(
            "pump.db schema {version} is newer than this binary ({SCHEMA_VERSION}); refusing to read"
        )));
    }
    if version < SCHEMA_VERSION {
        return Ok(None);
    }
    let Some(mode) = store.control_mode()? else {
        return Ok(None);
    };
    Ok(Some(PumpControl {
        mode: PumpMode::parse(&mode)?,
        shutdown_requested: store.shutdown_requested()?,
    }))
}

/// Report control mode and migrate legacy flags only when relevant state exists.
pub fn inspect(home: &Path) -> Result<PumpControl, VivariumError> {
    if home.join(DB_FILENAME).is_file() || home.join("PAUSE").exists() || home.join("STOP").exists()
    {
        initialize(home)
    } else {
        Ok(PumpControl {
            mode: PumpMode::Running,
            shutdown_requested: false,
        })
    }
}

/// Set one project's operator mode, migrating any legacy flags first.
pub fn set_project_mode(project: &Path, mode: PumpMode) -> Result<(), VivariumError> {
    let home = project_home(project)?;
    initialize(&home)?;
    let mut store = PumpStore::open(&home.join(DB_FILENAME))?;
    store.set_control_mode(mode.as_str())?;
    println!("pump {mode} for {}", project.display());
    Ok(())
}

/// Resolve the configured project pump home.
pub fn project_home(project: &Path) -> Result<PathBuf, VivariumError> {
    let mailspace = Mailspace::discover(Some(project))?;
    let config: PumpConfig = mailspace.config.pump.ok_or_else(|| {
        VivariumError::Config(String::from("no [pump] table in .vivi/mailspace.toml"))
    })?;
    Ok(config.home_dir(&mailspace.root))
}

/// Initialize one registered project's state and return its home and mode.
pub fn initialize_project(project: &Path) -> Result<(PathBuf, PumpControl), VivariumError> {
    let home = project_home(project)?;
    let control = match read_initialized(&home)? {
        Some(control) => control,
        None => initialize(&home)?,
    };
    Ok((home, control))
}

/// Request a service-owned child to stop at its next tick boundary.
pub fn request_shutdown(home: &Path, requested: bool) -> Result<(), VivariumError> {
    let mut store = PumpStore::open(&home.join(DB_FILENAME))?;
    store.set_shutdown_requested(requested)
}

fn remove_legacy_flag(path: &Path) {
    if let Err(error) = fs::remove_file(path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %path.display(), %error, "could not remove migrated pump flag");
    }
}
