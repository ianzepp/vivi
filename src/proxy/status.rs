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

fn lock_held(path: &Path) -> bool {
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
    #[cfg(target_os = "macos")]
    if let Err(error) = print_launchd() {
        println!("service error {error}");
    }
    #[cfg(target_os = "linux")]
    print_systemd();
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    println!("service unsupported");
}

#[cfg(target_os = "macos")]
fn print_launchd() -> Result<(), VivariumError> {
    let output = std::process::Command::new("launchctl")
        .args(["print", &format!("gui/{}/{}", uid()?, LAUNCHD_LABEL)])
        .output()?;
    if output.status.success() {
        print!("{}", String::from_utf8_lossy(&output.stdout));
    } else {
        println!("service not installed ({LAUNCHD_LABEL})");
    }
    Ok(())
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
fn print_systemd() {
    let output = std::process::Command::new("systemctl")
        .args(["--user", "is-active", SYSTEMD_UNIT])
        .output();
    match output {
        Ok(output) => {
            let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if text.is_empty() {
                println!("service not installed ({SYSTEMD_UNIT})");
            } else {
                println!("service {text}");
            }
        }
        Err(_) => println!("service systemd unavailable"),
    }
    if let Ok(path) = install::unit_path() {
        println!("unit {}", path.display());
    }
}
