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
         <key>ThrottleInterval</key><integer>10</integer>\n\
         <key>ExitTimeOut</key><integer>0</integer>\n\
         <key>StandardOutPath</key><string>{out}</string>\n\
         <key>StandardErrorPath</key><string>{err}</string>\n\
         </dict></plist>\n"
    )
}

/// systemctl --user steps applied after writing the unit file, in order.
///
/// The final `restart` both starts an inactive unit and replaces a running
/// one, so it subsumes the old `enable --now` and a rewritten unit takes
/// effect on every install instead of leaving a stale process running.
#[must_use]
pub fn systemd_apply_steps() -> Vec<Vec<&'static str>> {
    vec![
        vec!["--user", "daemon-reload"],
        vec!["--user", "enable", SYSTEMD_UNIT],
        vec!["--user", "restart", SYSTEMD_UNIT],
    ]
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
         StartLimitIntervalSec=60\n\
         StartLimitBurst=5\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart={bin} proxy run{extra}\n\
         Restart=on-failure\n\
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
    let executable = service_executable()?;
    let path_env =
        std::env::var("PATH").unwrap_or_else(|_| String::from("/usr/bin:/bin:/usr/sbin:/sbin"));
    let logs = paths::default_log_dir()?;
    fs::create_dir_all(&logs)?;
    let plist = launch_agent_plist(&executable, &logs, &path_env, explicit);
    let temporary = path.with_extension("plist.tmp");
    fs::write(&temporary, plist)?;
    let was_loaded = match bootout_if_loaded(&domain, &path) {
        Ok(loaded) => loaded,
        Err(error) => {
            drop(fs::remove_file(&temporary));
            return Err(error);
        }
    };
    fs::rename(temporary, &path)?;
    launchctl(&["bootstrap", &domain, &path.to_string_lossy()])?;
    if was_loaded {
        launchctl(&["kickstart", "-k", &format!("{domain}/{LAUNCHD_LABEL}")])?;
    }
    let pid = wait_for_launchd_pid(&domain);
    match pid {
        Some(pid) => println!("installed and started {} (pid {pid})", path.display()),
        None => println!("installed and started {} (pid unknown)", path.display()),
    }
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
    let executable = service_executable()?;
    fs::write(&path, systemd_unit(&executable, explicit))?;
    for step in systemd_apply_steps() {
        systemctl(&step)?;
    }
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

/// Unload the agent when it is loaded, reporting whether it was.
///
/// `true` means the service was detected as loaded and a `bootout` ran, so
/// the caller must force a fresh process after re-bootstrapping.
#[cfg(target_os = "macos")]
fn bootout_if_loaded(domain: &str, path: &Path) -> Result<bool, VivariumError> {
    let target = format!("{domain}/{LAUNCHD_LABEL}");
    if launchctl(&["print", &target]).is_ok() {
        launchctl(&["bootout", domain, &path.to_string_lossy()])?;
        return Ok(true);
    }
    Ok(false)
}

/// Parse the top-level `pid = <n>` line from `launchctl print` output.
///
/// `launchctl print` emits that line only while the job is running; nested
/// dictionaries come after it, so the first match is the job's own PID.
#[must_use]
pub fn parse_launchd_pid(output: &str) -> Option<u32> {
    let raw = output
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("pid = "))?;
    match raw.trim().parse() {
        Ok(pid) => Some(pid),
        Err(_) => None,
    }
}

#[cfg(target_os = "macos")]
fn wait_for_launchd_pid(domain: &str) -> Option<u32> {
    let target = format!("{domain}/{LAUNCHD_LABEL}");
    for _ in 0..10 {
        if let Ok(output) = launchctl(&["print", &target]) {
            if let Some(pid) = parse_launchd_pid(&output) {
                return Some(pid);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    None
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

/// Binary path written into the user service unit.
///
/// `current_exe` follows the managed `vivi` symlink to `vivi-<version>`.
/// Prefer the stable sibling symlink so `vivi update` is picked up on the
/// next process start without rewriting the unit.
///
/// # Errors
///
/// Returns an I/O error when the running executable cannot be resolved.
pub fn service_executable() -> Result<PathBuf, VivariumError> {
    Ok(prefer_stable_link(&std::env::current_exe()?))
}

/// If `exe` is a versioned `vivi-<semver>` next to a `vivi` symlink, use
/// the symlink. Cargo-installed and ad-hoc binaries are left as-is.
#[must_use]
pub fn prefer_stable_link(exe: &Path) -> PathBuf {
    let Some(name) = exe.file_name().and_then(|name| name.to_str()) else {
        return exe.to_path_buf();
    };
    if !is_versioned_vivi(name) {
        return exe.to_path_buf();
    }
    let Some(parent) = exe.parent() else {
        return exe.to_path_buf();
    };
    let link = parent.join("vivi");
    if is_symlink(&link) {
        link
    } else {
        exe.to_path_buf()
    }
}

fn is_versioned_vivi(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("vivi-") else {
        return false;
    };
    !rest.is_empty() && rest.chars().all(|ch| ch.is_ascii_digit() || ch == '.')
}

fn is_symlink(path: &Path) -> bool {
    match fs::symlink_metadata(path) {
        Ok(meta) => meta.file_type().is_symlink(),
        Err(_) => false,
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

#[cfg(test)]
#[path = "install_test.rs"]
mod install_test;
