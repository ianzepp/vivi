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
    let mailspace = crate::mailspace::Mailspace::discover(Some(project))?;
    let root = mailspace.root.canonicalize()?;
    let current = registry::load()?.get(&root);
    let wanted = change.apply(current.unwrap_or_default());
    if wanted.pump {
        validate_for_pump(&mailspace)?;
    }
    let changed = registry::update(|registry| Ok(registry.set(root.clone(), wanted)))?;
    let verb = match (current, changed) {
        (None, _) => "registered",
        (Some(_), true) => "updated",
        (Some(_), false) => "already registered",
    };
    println!("{verb} {} {}", root.display(), describe(wanted));
    Ok(())
}

fn validate_for_pump(mailspace: &crate::mailspace::Mailspace) -> Result<(), VivariumError> {
    let pump = mailspace.config.pump.as_ref().ok_or_else(|| {
        VivariumError::Config(format!(
            "{} has no [pump] configuration",
            mailspace.root.display()
        ))
    })?;
    let pools: Vec<String> = mailspace.config.pools.keys().cloned().collect();
    let role_pool = crate::pump::run::hand_role_pool(mailspace, pump);
    pump.validate_for(&mailspace.root, &pools, role_pool)?;
    let roster = crate::pump::run::identity_issues(mailspace, pump);
    if !roster.is_empty() {
        return Err(VivariumError::Config(roster.join("; ")));
    }
    crate::routing::require_ok(&crate::routing::validate(&mailspace.config))
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
            let shutdown = self.poll(&mut interval, &mut terminate).await;
            #[cfg(not(unix))]
            let shutdown = self.poll(&mut interval).await;
            if shutdown {
                break;
            }
        }
        stop_children(&mut self.children, &mut self.stopping).await;
        tracing::info!("pump service stopped");
        Ok(())
    }

    #[cfg(unix)]
    async fn poll(
        &mut self,
        interval: &mut tokio::time::Interval,
        terminate: &mut tokio::signal::unix::Signal,
    ) -> bool {
        let shutdown = wait_for_shutdown(interval, terminate).await;
        self.reconcile_tick(shutdown).await
    }

    #[cfg(not(unix))]
    async fn poll(&mut self, interval: &mut tokio::time::Interval) -> bool {
        let shutdown = wait_for_shutdown(interval).await;
        self.reconcile_tick(shutdown).await
    }

    async fn reconcile_tick(&mut self, shutdown: bool) -> bool {
        if shutdown {
            return true;
        }
        if let Err(error) = reconcile(
            &mut self.children,
            &mut self.retry_after,
            &mut self.observed_modes,
            &mut self.stopping,
        )
        .await
        {
            tracing::error!(%error, "pump service reconciliation failed");
        }
        false
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

async fn reconcile(
    children: &mut BTreeMap<PathBuf, Child>,
    retry_after: &mut BTreeMap<PathBuf, Instant>,
    observed_modes: &mut BTreeMap<PathBuf, super::control::PumpMode>,
    stopping: &mut BTreeMap<PathBuf, tokio::task::JoinHandle<()>>,
) -> Result<(), VivariumError> {
    let desired = registry::load()?.pumped();
    reap_stopping(stopping).await;
    stop_removed(&desired, children, retry_after, stopping);
    reap_exited(children, retry_after);
    start_missing(&desired, children, retry_after, observed_modes, stopping).await;
    Ok(())
}

fn stop_removed(
    desired: &BTreeSet<PathBuf>,
    children: &mut BTreeMap<PathBuf, Child>,
    retry_after: &mut BTreeMap<PathBuf, Instant>,
    stopping: &mut BTreeMap<PathBuf, tokio::task::JoinHandle<()>>,
) {
    let removed: Vec<PathBuf> = children
        .keys()
        .filter(|project| !desired.contains(*project))
        .cloned()
        .collect();
    for project in removed {
        if let Some(child) = children.remove(&project) {
            let path = project.clone();
            stopping.insert(
                project.clone(),
                tokio::spawn(async move {
                    stop_pump(&path, child).await;
                    tracing::info!(project = %path.display(), "stopped unregistered pump");
                }),
            );
        }
        retry_after.remove(&project);
    }
}

async fn reap_stopping(stopping: &mut BTreeMap<PathBuf, tokio::task::JoinHandle<()>>) {
    let done: Vec<PathBuf> = stopping
        .iter()
        .filter(|(_, task)| task.is_finished())
        .map(|(project, _)| project.clone())
        .collect();
    for project in done {
        if let Some(task) = stopping.remove(&project) {
            if let Err(error) = task.await {
                tracing::warn!(project = %project.display(), %error, "pump stop task failed");
            }
        }
    }
}

fn reap_exited(
    children: &mut BTreeMap<PathBuf, Child>,
    retry_after: &mut BTreeMap<PathBuf, Instant>,
) {
    let projects: Vec<PathBuf> = children.keys().cloned().collect();
    for project in projects {
        let status = match children.get_mut(&project).map(Child::try_wait) {
            Some(Ok(Some(status))) => Some(status.to_string()),
            Some(Ok(None)) | None => None,
            Some(Err(error)) => {
                tracing::warn!(project = %project.display(), %error, "could not inspect pump process");
                Some(String::from("unknown exit status"))
            }
        };
        if let Some(status) = status {
            children.remove(&project);
            retry_after.insert(project.clone(), Instant::now() + RESTART_DELAY);
            tracing::warn!(project = %project.display(), ?status, "project pump exited; restart delayed");
        }
    }
}

async fn start_missing(
    desired: &BTreeSet<PathBuf>,
    children: &mut BTreeMap<PathBuf, Child>,
    retry_after: &mut BTreeMap<PathBuf, Instant>,
    observed_modes: &mut BTreeMap<PathBuf, super::control::PumpMode>,
    stopping: &BTreeMap<PathBuf, tokio::task::JoinHandle<()>>,
) {
    let executable = match std::env::current_exe() {
        Ok(path) => path,
        Err(error) => {
            tracing::error!(%error, "could not locate vivi executable");
            return;
        }
    };
    for project in desired {
        if children.contains_key(project)
            || stopping.contains_key(project)
            || retry_after
                .get(project)
                .is_some_and(|until| *until > Instant::now())
        {
            continue;
        }
        let (home, control) = match super::control::initialize_project(project) {
            Ok(project) => project,
            Err(error) => {
                tracing::warn!(project = %project.display(), %error, "could not read project pump control state");
                retry_after.insert(project.clone(), Instant::now() + RESTART_DELAY);
                continue;
            }
        };
        if observed_modes.insert(project.clone(), control.mode) != Some(control.mode) {
            retry_after.remove(project);
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
                children.insert(project.clone(), child);
                retry_after.remove(project);
            }
            Err(error) => {
                tracing::warn!(project = %project.display(), %error, "could not start project pump");
                retry_after.insert(project.clone(), Instant::now() + RESTART_DELAY);
            }
        }
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

async fn stop_children(
    children: &mut BTreeMap<PathBuf, Child>,
    stopping: &mut BTreeMap<PathBuf, tokio::task::JoinHandle<()>>,
) {
    for (project, child) in std::mem::take(children) {
        let path = project.clone();
        stopping.insert(
            project,
            tokio::spawn(async move { stop_pump(&path, child).await }),
        );
    }
    for (project, task) in std::mem::take(stopping) {
        if let Err(error) = task.await {
            tracing::warn!(project = %project.display(), %error, "pump stop task failed");
        }
    }
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
