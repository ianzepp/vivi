//! User-level supervisor for independent project pump processes.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Command as StdCommand;
use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

use crate::VivariumError;
use crate::registry::{self, Flags};

const POLL_INTERVAL: Duration = Duration::from_secs(5);
const RESTART_DELAY: Duration = Duration::from_secs(15);
#[cfg(target_os = "macos")]
const CONFIG_DIRECTORY: &str = "vivi";

/// Requested flag changes for `pump service add`; `None` keeps the current
/// value (or the default for a new registration: pump on, serve off).
#[derive(Clone, Copy, Debug, Default)]
pub struct FlagChange {
    pub pump: Option<bool>,
    pub serve: Option<bool>,
}

impl FlagChange {
    fn apply(self, current: Flags) -> Flags {
        Flags {
            pump: self.pump.unwrap_or(current.pump),
            serve: self.serve.unwrap_or(current.serve),
        }
    }
}

/// Add a validated project to the host registry, or change its flags.
pub fn add(project: &Path, change: FlagChange) -> Result<(), VivariumError> {
    crate::mailspace::inherit::refuse_base(project)?;
    let mailspace = crate::mailspace::Mailspace::discover(Some(project))?;
    let root = mailspace.root.canonicalize()?;
    let current = registry::load()?.get(&root);
    let wanted = change.apply(current.unwrap_or_default());
    let not_ready = if wanted.pump {
        validate_for_pump(&mailspace)?
    } else {
        None
    };
    let changed = registry::update(|registry| Ok(registry.set(root.clone(), wanted)))?;
    let verb = match (current, changed) {
        (None, _) => "registered",
        (Some(_), true) => "updated",
        (Some(_), false) => "already registered",
    };
    println!("{verb} {} {}", root.display(), describe(wanted));
    if let Some(summary) = not_ready {
        println!("{summary} (the pump holds until these are supplied)");
    }
    Ok(())
}

/// Refuse a project whose pump config is wrong. A project that is only
/// missing items is not refused: its pump holds until they are supplied, so
/// registering a freshly initialised project is fine. Returns the not-ready
/// summary for the caller to show.
fn validate_for_pump(
    mailspace: &crate::mailspace::Mailspace,
) -> Result<Option<String>, VivariumError> {
    if mailspace.config.pump.is_none() {
        return Err(VivariumError::Config(format!(
            "{} has no [pump] configuration",
            mailspace.root.display()
        )));
    }
    let issues = super::readiness::assess(mailspace);
    crate::routing::require_ok(&issues)?;
    Ok(crate::routing::not_ready_summary(&issues))
}

fn describe(flags: Flags) -> String {
    format!("pump={} serve={}", flags.pump, flags.serve)
}

/// Remove a project from the host registry.
pub fn remove(project: &Path) -> Result<(), VivariumError> {
    let root = match project.canonicalize() {
        Ok(root) => root,
        Err(_) if project.is_absolute() => project.to_path_buf(),
        Err(_) => std::env::current_dir()?.join(project),
    };
    registry::update(|registry| {
        if registry.remove(&root) {
            Ok(())
        } else {
            Err(VivariumError::Config(format!(
                "{} is not registered with the pump service",
                root.display()
            )))
        }
    })?;
    println!("removed {}", root.display());
    Ok(())
}

/// Print registered project roots with their flags.
pub fn list() -> Result<(), VivariumError> {
    let entries = registry::load()?.entries();
    if entries.is_empty() {
        println!("no projects registered");
    }
    for entry in entries {
        println!("{}  {}", entry.root.display(), describe(entry.flags));
    }
    Ok(())
}

/// Install and start the current binary as a macOS user LaunchAgent.
#[cfg(target_os = "macos")]
pub fn install() -> Result<(), VivariumError> {
    drop(registry::load()?);
    let path = launch_agent_path()?;
    let parent = path
        .parent()
        .ok_or_else(|| VivariumError::Config("LaunchAgent path has no parent".into()))?;
    fs::create_dir_all(parent)?;
    let domain = launch_domain()?;
    let executable = std::env::current_exe()?;
    let path_env =
        std::env::var("PATH").unwrap_or_else(|_| String::from("/usr/bin:/bin:/usr/sbin:/sbin"));
    let logs = dirs::cache_dir()
        .ok_or_else(|| VivariumError::Config("could not find the user's cache directory".into()))?
        .join(CONFIG_DIRECTORY);
    fs::create_dir_all(&logs)?;
    let plist = launch_agent_plist(&executable, &logs, &path_env);
    let temporary = path.with_extension("plist.tmp");
    fs::write(&temporary, plist)?;
    if let Err(error) = bootout_if_loaded(&domain, &path) {
        drop(fs::remove_file(&temporary));
        return Err(error);
    }
    fs::rename(temporary, &path)?;
    launchctl(&["bootstrap", &domain, path.to_string_lossy().as_ref()])?;
    println!("installed and started {}", path.display());
    Ok(())
}

/// Stop and remove the macOS user LaunchAgent while retaining project registrations.
#[cfg(target_os = "macos")]
pub fn uninstall() -> Result<(), VivariumError> {
    let path = launch_agent_path()?;
    if path.exists() {
        let domain = launch_domain()?;
        bootout_if_loaded(&domain, &path)?;
        fs::remove_file(&path)?;
    }
    println!("pump service removed");
    Ok(())
}

/// Print launchd's service record on macOS.
#[cfg(target_os = "macos")]
pub fn status() -> Result<(), VivariumError> {
    let target = format!("{}/{}", launch_domain()?, SERVICE_LABEL);
    let output = launchctl(&["print", &target])?;
    print!("{output}");
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn install() -> Result<(), VivariumError> {
    Err(VivariumError::Other(
        "pump service install currently supports macOS LaunchAgents".into(),
    ))
}

#[cfg(not(target_os = "macos"))]
pub fn uninstall() -> Result<(), VivariumError> {
    Err(VivariumError::Other(
        "pump service uninstall currently supports macOS LaunchAgents".into(),
    ))
}

#[cfg(not(target_os = "macos"))]
pub fn status() -> Result<(), VivariumError> {
    Err(VivariumError::Other(
        "pump service status currently supports macOS LaunchAgents".into(),
    ))
}

#[cfg(target_os = "macos")]
const SERVICE_LABEL: &str = "com.vivi.pump-service";

#[cfg(target_os = "macos")]
fn launch_agent_path() -> Result<PathBuf, VivariumError> {
    let home = dirs::home_dir()
        .ok_or_else(|| VivariumError::Config("could not find the user's home directory".into()))?;
    Ok(home
        .join("Library/LaunchAgents")
        .join(format!("{SERVICE_LABEL}.plist")))
}

#[cfg(target_os = "macos")]
fn launch_domain() -> Result<String, VivariumError> {
    let output = StdCommand::new("id").arg("-u").output()?;
    if !output.status.success() {
        return Err(VivariumError::Other("id -u failed".into()));
    }
    let uid = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(format!("gui/{uid}"))
}

#[cfg(target_os = "macos")]
fn launchctl(args: &[&str]) -> Result<String, VivariumError> {
    let output = StdCommand::new("launchctl").args(args).output()?;
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    if output.status.success() {
        Ok(text)
    } else {
        Err(VivariumError::Other(format!(
            "launchctl {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

#[cfg(target_os = "macos")]
fn bootout_if_loaded(domain: &str, path: &Path) -> Result<(), VivariumError> {
    let target = format!("{domain}/{SERVICE_LABEL}");
    if launchctl(&["print", &target]).is_ok() {
        launchctl(&["bootout", domain, path.to_string_lossy().as_ref()])?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn launch_agent_plist(executable: &Path, logs: &Path, path_env: &str) -> String {
    let bin = xml_escape(&executable.to_string_lossy());
    let out = xml_escape(&logs.join("pump-service.stdout.log").to_string_lossy());
    let err = xml_escape(&logs.join("pump-service.stderr.log").to_string_lossy());
    let path = xml_escape(path_env);
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\"><dict>\n\
         <key>Label</key><string>{SERVICE_LABEL}</string>\n\
         <key>ProgramArguments</key><array><string>{bin}</string><string>pump</string><string>service</string><string>run</string></array>\n\
         <key>EnvironmentVariables</key><dict><key>PATH</key><string>{path}</string></dict>\n\
         <key>RunAtLoad</key><true/><key>KeepAlive</key><true/>\n\
         <key>ExitTimeOut</key><integer>0</integer>\n\
         <key>StandardOutPath</key><string>{out}</string>\n\
         <key>StandardErrorPath</key><string>{err}</string>\n\
         </dict></plist>\n"
    )
}

#[cfg(target_os = "macos")]
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Run as a foreground process under launchd, systemd, or another service manager.
pub async fn run() -> Result<(), VivariumError> {
    let _lock = take_service_lock()?;
    ServiceRuntime::default().run().await
}

/// Per-project child pump processes and the bookkeeping that governs their
/// restart. All maps are keyed by project root.
#[derive(Default)]
struct ServiceRuntime {
    children: BTreeMap<PathBuf, Child>,
    retry_after: BTreeMap<PathBuf, Instant>,
    observed_modes: BTreeMap<PathBuf, super::control::PumpMode>,
    stopping: BTreeMap<PathBuf, tokio::task::JoinHandle<()>>,
}

impl ServiceRuntime {
    async fn run(mut self) -> Result<(), VivariumError> {
        let mut interval = tokio::time::interval(POLL_INTERVAL);
        #[cfg(unix)]
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

        tracing::info!(
            "pump service started; config {}",
            registry::path()?.display()
        );
        loop {
            #[cfg(unix)]
            let shutdown = wait_for_shutdown(&mut interval, &mut terminate).await;
            #[cfg(not(unix))]
            let shutdown = wait_for_shutdown(&mut interval).await;
            if shutdown {
                break;
            }
            if let Err(error) = self.reconcile().await {
                tracing::error!(%error, "pump service reconciliation failed");
            }
        }
        self.stop_all().await;
        tracing::info!("pump service stopped");
        Ok(())
    }

    async fn reconcile(&mut self) -> Result<(), VivariumError> {
        let desired = registry::load()?.pumped();
        self.reap_stopping().await;
        self.stop_removed(&desired);
        self.reap_exited();
        self.start_missing(&desired).await;
        Ok(())
    }

    fn stop_removed(&mut self, desired: &BTreeSet<PathBuf>) {
        let removed: Vec<PathBuf> = self
            .children
            .keys()
            .filter(|project| !desired.contains(*project))
            .cloned()
            .collect();
        for project in removed {
            if let Some(child) = self.children.remove(&project) {
                let path = project.clone();
                self.stopping.insert(
                    project.clone(),
                    tokio::spawn(async move {
                        stop_pump(&path, child).await;
                        tracing::info!(project = %path.display(), "stopped unregistered pump");
                    }),
                );
            }
            self.retry_after.remove(&project);
        }
    }

    async fn reap_stopping(&mut self) {
        let done: Vec<PathBuf> = self
            .stopping
            .iter()
            .filter(|(_, task)| task.is_finished())
            .map(|(project, _)| project.clone())
            .collect();
        for project in done {
            if let Some(task) = self.stopping.remove(&project) {
                if let Err(error) = task.await {
                    tracing::warn!(project = %project.display(), %error, "pump stop task failed");
                }
            }
        }
    }

    fn reap_exited(&mut self) {
        let projects: Vec<PathBuf> = self.children.keys().cloned().collect();
        for project in projects {
            let status = match self.children.get_mut(&project).map(Child::try_wait) {
                Some(Ok(Some(status))) => Some(status.to_string()),
                Some(Ok(None)) | None => None,
                Some(Err(error)) => {
                    tracing::warn!(project = %project.display(), %error, "could not inspect pump process");
                    Some(String::from("unknown exit status"))
                }
            };
            if let Some(status) = status {
                self.children.remove(&project);
                self.retry_after
                    .insert(project.clone(), Instant::now() + RESTART_DELAY);
                tracing::warn!(project = %project.display(), ?status, "project pump exited; restart delayed");
            }
        }
    }

    async fn start_missing(&mut self, desired: &BTreeSet<PathBuf>) {
        let executable = match std::env::current_exe() {
            Ok(path) => path,
            Err(error) => {
                tracing::error!(%error, "could not locate vivi executable");
                return;
            }
        };
        for project in desired {
            if self.children.contains_key(project)
                || self.stopping.contains_key(project)
                || self
                    .retry_after
                    .get(project)
                    .is_some_and(|until| *until > Instant::now())
            {
                continue;
            }
            if let Err(error) = crate::mailspace::inherit::refuse_base(project) {
                tracing::warn!(project = %project.display(), %error, "registered root is a base config; not pumped");
                continue;
            }
            let (home, control) = match super::control::initialize_project(project) {
                Ok(project) => project,
                Err(error) => {
                    tracing::warn!(project = %project.display(), %error, "could not read project pump control state");
                    self.retry_after
                        .insert(project.clone(), Instant::now() + RESTART_DELAY);
                    continue;
                }
            };
            if self.observed_modes.insert(project.clone(), control.mode) != Some(control.mode) {
                self.retry_after.remove(project);
            }
            if control.mode == super::control::PumpMode::Stopped {
                continue;
            }
            if control.shutdown_requested && shutdown_is_pending(project, &home) {
                continue;
            }
            match spawn_pump(&executable, project).await {
                Ok(child) => {
                    tracing::info!(project = %project.display(), pid = ?child.id(), "started project pump");
                    self.children.insert(project.clone(), child);
                    self.retry_after.remove(project);
                }
                Err(error) => {
                    tracing::warn!(project = %project.display(), %error, "could not start project pump");
                    self.retry_after
                        .insert(project.clone(), Instant::now() + RESTART_DELAY);
                }
            }
        }
    }

    async fn stop_all(&mut self) {
        for (project, child) in std::mem::take(&mut self.children) {
            let path = project.clone();
            self.stopping.insert(
                project,
                tokio::spawn(async move { stop_pump(&path, child).await }),
            );
        }
        for (project, task) in std::mem::take(&mut self.stopping) {
            if let Err(error) = task.await {
                tracing::warn!(project = %project.display(), %error, "pump stop task failed");
            }
        }
    }
}

#[cfg(unix)]
async fn wait_for_shutdown(
    interval: &mut tokio::time::Interval,
    terminate: &mut tokio::signal::unix::Signal,
) -> bool {
    tokio::select! {
        _ = tokio::signal::ctrl_c() => true,
        _ = terminate.recv() => true,
        _ = interval.tick() => false,
    }
}

#[cfg(not(unix))]
async fn wait_for_shutdown(interval: &mut tokio::time::Interval) -> bool {
    tokio::select! {
        _ = tokio::signal::ctrl_c() => true,
        _ = interval.tick() => false,
    }
}

fn shutdown_is_pending(project: &Path, home: &Path) -> bool {
    match super::run::lock_is_held(home) {
        Ok(true) => true,
        Ok(false) => match super::control::request_shutdown(home, false) {
            Ok(()) => false,
            Err(error) => {
                tracing::warn!(project = %project.display(), %error, "could not clear stale shutdown request");
                true
            }
        },
        Err(error) => {
            tracing::warn!(project = %project.display(), %error, "could not inspect pump lock");
            true
        }
    }
}

async fn spawn_pump(executable: &Path, project: &Path) -> Result<Child, std::io::Error> {
    let mut child = Command::new(executable)
        .args(["pump", "run", "--service-child", "--project"])
        .arg(project)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    let Some(stdout) = child.stdout.take() else {
        return Err(std::io::Error::other("pump child stdout was not piped"));
    };
    let mut lines = BufReader::new(stdout).lines();
    let ready = tokio::time::timeout(Duration::from_secs(15), lines.next_line()).await;
    let failure = match ready {
        Ok(Ok(Some(line))) if line == "VIVI_PUMP_SERVICE_READY" => return Ok(child),
        Ok(Ok(Some(line))) => {
            std::io::Error::other(format!("unexpected pump child readiness output: {line}"))
        }
        Ok(Ok(None)) => std::io::Error::other("pump child exited before becoming ready"),
        Ok(Err(error)) => error,
        Err(_) => std::io::Error::other("pump child readiness timed out"),
    };
    drop(child.kill().await);
    drop(child.wait().await);
    Err(failure)
}

async fn stop_pump(project: &Path, mut child: Child) {
    match child.try_wait() {
        Ok(Some(_)) => return,
        Ok(None) => {}
        Err(error) => {
            tracing::warn!(project = %project.display(), %error, "could not check project pump status");
        }
    }
    let home = match super::control::project_home(project) {
        Ok(home) => home,
        Err(error) => {
            tracing::warn!(project = %project.display(), %error, "cannot locate pump database");
            force_stop(&mut child, project).await;
            return;
        }
    };
    if let Err(error) = super::control::initialize(&home)
        .and_then(|_| super::control::request_shutdown(&home, true))
    {
        tracing::warn!(project = %project.display(), %error, "cannot request graceful pump stop");
        force_stop(&mut child, project).await;
        return;
    }
    if let Err(error) = child.wait().await {
        tracing::warn!(project = %project.display(), %error, "could not wait for project pump");
    }
    if let Err(error) = super::control::request_shutdown(&home, false) {
        tracing::warn!(project = %project.display(), %error, "could not clear service shutdown request");
    }
}

async fn force_stop(child: &mut Child, project: &Path) {
    if let Err(error) = child.kill().await {
        tracing::warn!(project = %project.display(), %error, "could not stop project pump");
    }
    drop(child.wait().await);
}

fn take_service_lock() -> Result<fs::File, VivariumError> {
    registry::lock_file(
        registry::path()?.with_extension("lock"),
        "another Vivi pump service is running",
    )
}
