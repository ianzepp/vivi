//! `vivi proxy status`: service manager + listener + lock.

use std::path::Path;

use super::config;
#[cfg(target_os = "macos")]
use super::install::LAUNCHD_LABEL;
#[cfg(target_os = "linux")]
use super::install::{self, SYSTEMD_UNIT};
use super::paths;
use crate::VivariumError;

/// Print service-manager status and a short listener summary.
///
/// # Errors
///
/// Returns a configuration error when `proxy.toml` cannot be read.
pub fn print(explicit: Option<&Path>) -> Result<(), VivariumError> {
    let path = paths::resolve_config_path(explicit)?;
    println!("config {}", path.display());
    match config::load(explicit) {
        Ok(config) => {
            println!("listen {}", config.listen);
            println!("log_dir {}", config.log_dir.display());
            println!("providers {}", config.providers.len());
        }
        Err(error) => println!("config_error {error}"),
    }
    let lock = paths::lock_path(&path);
    println!("lock {}", if lock_held(&lock) { "held" } else { "free" });
    print_service();
    Ok(())
}

/// True when another process holds the proxy instance lock.
///
/// A lock file that cannot be opened counts as free: nothing is running.
pub(crate) fn lock_held(path: &Path) -> bool {
    let Ok(file) = std::fs::OpenOptions::new().write(true).open(path) else {
        return false;
    };
    match file.try_lock() {
        Ok(()) => {
            drop(file.unlock());
            false
        }
        Err(_) => true,
    }
}

fn print_service() {
    match service_presence() {
        ServicePresence::Installed => println!("service installed ({})", service_name()),
        ServicePresence::Missing => println!("service not installed ({})", service_name()),
        ServicePresence::Unavailable => println!("service unavailable"),
    }
    #[cfg(target_os = "linux")]
    if let Ok(path) = install::unit_path() {
        println!("unit {}", path.display());
    }
}

/// Whether the user service manager knows about the proxy unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ServicePresence {
    /// The unit is registered with the service manager.
    Installed,
    /// The service manager runs but has no proxy unit.
    Missing,
    /// The service manager cannot be probed on this host.
    Unavailable,
}

/// Probe the platform service manager without printing anything.
pub(crate) fn service_presence() -> ServicePresence {
    #[cfg(target_os = "macos")]
    {
        launchd_presence()
    }
    #[cfg(target_os = "linux")]
    {
        systemd_presence()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        ServicePresence::Unavailable
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn service_name() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        LAUNCHD_LABEL
    }
    #[cfg(target_os = "linux")]
    {
        SYSTEMD_UNIT
    }
}

#[cfg(target_os = "macos")]
fn launchd_presence() -> ServicePresence {
    let Ok(uid) = uid() else {
        return ServicePresence::Unavailable;
    };
    let output = match std::process::Command::new("launchctl")
        .args(["print", &format!("gui/{uid}/{LAUNCHD_LABEL}")])
        .output()
    {
        Ok(output) => output,
        Err(_) => return ServicePresence::Unavailable,
    };
    if output.status.success() {
        ServicePresence::Installed
    } else {
        ServicePresence::Missing
    }
}

#[cfg(target_os = "macos")]
fn uid() -> Result<String, VivariumError> {
    let output = std::process::Command::new("id").arg("-u").output()?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(VivariumError::Other("id -u failed".into()))
    }
}

#[cfg(target_os = "linux")]
fn systemd_presence() -> ServicePresence {
    let output = match std::process::Command::new("systemctl")
        .args(["--user", "is-active", SYSTEMD_UNIT])
        .output()
    {
        Ok(output) => output,
        Err(_) => return ServicePresence::Unavailable,
    };
    if String::from_utf8_lossy(&output.stdout).trim().is_empty() {
        ServicePresence::Missing
    } else {
        ServicePresence::Installed
    }
}
