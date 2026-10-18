//! User service unit generators for launchd and systemd.

use std::fs;
use std::path::{Path, PathBuf};

use super::config;
#[cfg(target_os = "macos")]
use super::paths;
use crate::VivariumError;

/// launchd label.
pub const LAUNCHD_LABEL: &str = "com.vivi.proxy";
/// systemd --user unit name.
pub const SYSTEMD_UNIT: &str = "vivi-proxy.service";

/// Install and start the user OS service.
///
/// # Errors
///
/// Returns a configuration or service-manager error.
pub fn install(explicit: Option<&Path>) -> Result<(), VivariumError> {
    drop(config::load(explicit)?);
    platform_install(explicit)
}

/// Stop and remove the user OS service.
///
/// # Errors
///
/// Returns a service-manager error.
pub fn uninstall(_explicit: Option<&Path>) -> Result<(), VivariumError> {
    platform_uninstall()
}

#[cfg(target_os = "macos")]
fn platform_install(explicit: Option<&Path>) -> Result<(), VivariumError> {
    install_launchd(explicit)
}

#[cfg(target_os = "macos")]
fn platform_uninstall() -> Result<(), VivariumError> {
    uninstall_launchd()
}

#[cfg(target_os = "linux")]
fn platform_install(explicit: Option<&Path>) -> Result<(), VivariumError> {
    install_systemd(explicit)
}

#[cfg(target_os = "linux")]
fn platform_uninstall() -> Result<(), VivariumError> {
    uninstall_systemd()
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn platform_install(explicit: Option<&Path>) -> Result<(), VivariumError> {
    Err(VivariumError::Other(format!(
        "proxy install supports macOS LaunchAgents and Linux systemd --user (config {explicit:?})"
    )))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn platform_uninstall() -> Result<(), VivariumError> {
    Err(VivariumError::Other(
        "proxy uninstall supports macOS LaunchAgents and Linux systemd --user".into(),
    ))
}

/// Render the launchd plist used by install and tests.
#[must_use]
pub fn launch_agent_plist(
    executable: &Path,
    logs: &Path,
    path_env: &str,
    config: Option<&Path>,
) -> String {
    let bin = xml_escape(&executable.to_string_lossy());
    let out = xml_escape(&logs.join("proxy.stdout.log").to_string_lossy());
    let err = xml_escape(&logs.join("proxy.stderr.log").to_string_lossy());
    let path = xml_escape(path_env);
    let extra = match config {
        Some(config) => format!(
            "<string>--config</string><string>{}</string>",
            xml_escape(&config.to_string_lossy())
        ),
        None => String::new(),
    };
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\"><dict>\n\
         <key>Label</key><string>{LAUNCHD_LABEL}</string>\n\
         <key>ProgramArguments</key><array><string>{bin}</string><string>proxy</string><string>run</string>{extra}</array>\n\
         <key>EnvironmentVariables</key><dict><key>PATH</key><string>{path}</string></dict>\n\
         <key>RunAtLoad</key><true/><key>KeepAlive</key><true/>\n\
         <key>ExitTimeOut</key><integer>0</integer>\n\
         <key>StandardOutPath</key><string>{out}</string>\n\
         <key>StandardErrorPath</key><string>{err}</string>\n\
         </dict></plist>\n"
    )
}

/// Render the systemd --user unit. Public for tests.
#[must_use]
pub fn systemd_unit(executable: &Path, config: Option<&Path>) -> String {
    let bin = executable.display();
    let extra = match config {
        Some(config) => format!(" --config {}", config.display()),
        None => String::new(),
    };
    format!(
        "[Unit]\n\
         Description=Vivi local model proxy\n\
         After=default.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart={bin} proxy run{extra}\n\
         Restart=always\n\
         RestartSec=5\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n"
    )
}

#[cfg(target_os = "macos")]
fn install_launchd(explicit: Option<&Path>) -> Result<(), VivariumError> {
    let path = launch_agent_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let domain = launch_domain()?;
    let executable = std::env::current_exe()?;
    let path_env =
        std::env::var("PATH").unwrap_or_else(|_| String::from("/usr/bin:/bin:/usr/sbin:/sbin"));
    let logs = paths::default_log_dir()?;
    fs::create_dir_all(&logs)?;
    let plist = launch_agent_plist(&executable, &logs, &path_env, explicit);
    let temporary = path.with_extension("plist.tmp");
    fs::write(&temporary, plist)?;
    if let Err(error) = bootout_if_loaded(&domain, &path) {
        drop(fs::remove_file(&temporary));
        return Err(error);
    }
    fs::rename(temporary, &path)?;
    launchctl(&["bootstrap", &domain, &path.to_string_lossy()])?;
    println!("installed and started {}", path.display());
    Ok(())
}

#[cfg(target_os = "macos")]
fn uninstall_launchd() -> Result<(), VivariumError> {
    let path = launch_agent_path()?;
    if path.exists() {
        let domain = launch_domain()?;
        bootout_if_loaded(&domain, &path)?;
        fs::remove_file(&path)?;
    }
    println!("proxy service removed");
    Ok(())
}

#[cfg(target_os = "linux")]
fn install_systemd(explicit: Option<&Path>) -> Result<(), VivariumError> {
    let path = systemd_unit_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let executable = std::env::current_exe()?;
    fs::write(&path, systemd_unit(&executable, explicit))?;
    systemctl(&["--user", "daemon-reload"])?;
    systemctl(&["--user", "enable", "--now", SYSTEMD_UNIT])?;
    println!("installed and started {}", path.display());
    Ok(())
}

#[cfg(target_os = "linux")]
fn uninstall_systemd() -> Result<(), VivariumError> {
    let path = systemd_unit_path()?;
    drop(systemctl(&["--user", "disable", "--now", SYSTEMD_UNIT]));
    if path.exists() {
        fs::remove_file(&path)?;
    }
    drop(systemctl(&["--user", "daemon-reload"]));
    println!("proxy service removed");
    Ok(())
}

#[cfg(target_os = "macos")]
fn launch_agent_path() -> Result<PathBuf, VivariumError> {
    let home = dirs::home_dir()
        .ok_or_else(|| VivariumError::Config("could not find the user's home directory".into()))?;
    Ok(home
        .join("Library/LaunchAgents")
        .join(format!("{LAUNCHD_LABEL}.plist")))
}

#[cfg(target_os = "macos")]
fn launch_domain() -> Result<String, VivariumError> {
    let output = std::process::Command::new("id").arg("-u").output()?;
    if !output.status.success() {
        return Err(VivariumError::Other("id -u failed".into()));
    }
    let uid = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(format!("gui/{uid}"))
}

#[cfg(target_os = "macos")]
fn launchctl(args: &[&str]) -> Result<String, VivariumError> {
    run_tool("launchctl", args)
}

#[cfg(target_os = "macos")]
fn bootout_if_loaded(domain: &str, path: &Path) -> Result<(), VivariumError> {
    let target = format!("{domain}/{LAUNCHD_LABEL}");
    if launchctl(&["print", &target]).is_ok() {
        launchctl(&["bootout", domain, &path.to_string_lossy()])?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn systemd_unit_path() -> Result<PathBuf, VivariumError> {
    let root = dirs::config_dir().ok_or_else(|| {
        VivariumError::Config("could not find the user's config directory".into())
    })?;
    Ok(root.join("systemd/user").join(SYSTEMD_UNIT))
}

#[cfg(target_os = "linux")]
fn systemctl(args: &[&str]) -> Result<String, VivariumError> {
    run_tool("systemctl", args)
}

fn run_tool(tool: &str, args: &[&str]) -> Result<String, VivariumError> {
    let output = std::process::Command::new(tool).args(args).output()?;
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    if output.status.success() {
        Ok(text)
    } else {
        Err(VivariumError::Other(format!(
            "{tool} {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Path of the generated unit, when this platform writes one.
///
/// # Errors
///
/// Returns an error when the home or config directory cannot be found.
pub fn unit_path() -> Result<PathBuf, VivariumError> {
    #[cfg(target_os = "macos")]
    {
        launch_agent_path()
    }
    #[cfg(target_os = "linux")]
    {
        systemd_unit_path()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Err(VivariumError::Other(
            "no user service unit on this platform".into(),
        ))
    }
}
