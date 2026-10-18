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
    initialize_with_lock_policy(home, false)
}

/// Initialize control state from the pump process that already owns its lock.
pub(crate) fn initialize_for_run(home: &Path) -> Result<PumpControl, VivariumError> {
    initialize_with_lock_policy(home, true)
}

fn initialize_with_lock_policy(
    home: &Path,
    caller_holds_pump_lock: bool,
) -> Result<PumpControl, VivariumError> {
    let stop = home.join("STOP");
    let pause = home.join("PAUSE");
    let marker_mode = legacy_marker_mode(home);
    let legacy_mode = marker_mode.unwrap_or(PumpMode::Running);
    let mut store = if caller_holds_pump_lock {
        PumpStore::open_for_run(&home.join(DB_FILENAME))?
    } else {
        PumpStore::open(&home.join(DB_FILENAME))?
    };
    let mut mode = PumpMode::parse(&store.initialize_control(legacy_mode.as_str())?)?;
    if let Some(marker_mode) = marker_mode {
        store.set_control_mode(marker_mode.as_str())?;
        mode = marker_mode;
    }
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
    Ok(read_initialized(home)?.unwrap_or_else(|| legacy_control(home)))
}

/// Set one project's operator mode, migrating any legacy flags first.
pub fn set_project_mode(project: &Path, mode: PumpMode) -> Result<(), VivariumError> {
    let home = project_home(project)?;
    let lock_held = super::run::lock_is_held(&home)?;
    let version = read_schema_version(&home)?;
    if lock_held && version.is_none_or(|version| version < SCHEMA_VERSION) {
        write_legacy_mode(&home, mode)?;
        println!("pump {mode} for {}", project.display());
        return Ok(());
    }
    if !lock_held {
        initialize(&home)?;
    }
    let mut store = PumpStore::open(&home.join(DB_FILENAME))?;
    if lock_held {
        store.initialize_control(legacy_mode(&home).as_str())?;
    }
    store.set_control_mode(mode.as_str())?;
    if lock_held {
        write_legacy_mode(&home, mode)?;
    }
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

fn read_schema_version(home: &Path) -> Result<Option<i64>, VivariumError> {
    PumpStore::open_read_only(&home.join(DB_FILENAME))?
        .map(|store| store.schema_version())
        .transpose()
}

fn legacy_mode(home: &Path) -> PumpMode {
    legacy_marker_mode(home).unwrap_or(PumpMode::Running)
}

fn legacy_marker_mode(home: &Path) -> Option<PumpMode> {
    if home.join("STOP").exists() {
        Some(PumpMode::Stopped)
    } else if home.join("PAUSE").exists() {
        Some(PumpMode::Paused)
    } else {
        None
    }
}

fn legacy_control(home: &Path) -> PumpControl {
    PumpControl {
        mode: legacy_mode(home),
        shutdown_requested: false,
    }
}

fn write_legacy_mode(home: &Path, mode: PumpMode) -> Result<(), VivariumError> {
    fs::create_dir_all(home)?;
    let stop = home.join("STOP");
    let pause = home.join("PAUSE");
    match mode {
        PumpMode::Running => {
            remove_legacy_flag(&stop);
            remove_legacy_flag(&pause);
        }
        PumpMode::Paused => {
            fs::write(&pause, "")?;
            remove_legacy_flag(&stop);
        }
        PumpMode::Stopped => {
            fs::write(&stop, "")?;
            remove_legacy_flag(&pause);
        }
    }
    Ok(())
}
