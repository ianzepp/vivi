//! Best-effort copy of a sibling bare repo's owning group and ACL.
//!
//! Hub lanes (`/mnt/vivi/lanes`) are `git:git` with named ACL entries and no
//! default ACL, so `git init --bare --shared=group` does not inherit them.
//! The sibling is the lexicographically first other `<name>.git` directory
//! in the lanes root whose ACL `getfacl` can read (byte order of the file
//! name). Only that directory is copied, not its contents. Off Linux, or
//! when the tools are missing, no sibling is readable, or the copy itself
//! fails, the result is one warning and the lane still exists.

use std::fs;
use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
use std::io::ErrorKind;
#[cfg(target_os = "linux")]
use std::process::{Command, ExitStatus, Stdio};

/// Whether this process can copy an ACL.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum HostKind {
    /// `getfacl` / `setfacl` are not used off Linux.
    /// Constructed only off Linux; the Linux build still matches on it.
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    NotLinux,
    /// Linux. `tools` is false when `getfacl`, `setfacl`, or `chgrp` cannot be started.
    /// Constructed only on Linux; other builds still match on it.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Linux { tools: bool },
}

/// What the pure decision picked, before any copy runs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum AclIntent {
    /// Copy the group and ACL from this sibling.
    Copy { sibling: PathBuf },
    /// Do not copy. `warning` is the one line to report.
    Skip { warning: String },
}

/// What the copy did. Never a failure of lane creation.
#[derive(Clone, Debug, Eq, PartialEq)]
#[must_use = "a skipped ACL copy is a warning the caller reports"]
pub(super) enum AclCopy {
    /// Group and ACL were copied from this sibling.
    Copied { sibling: PathBuf },
    /// Nothing was copied. `warning` is one line.
    Skipped { warning: String },
}

/// Off Linux: the tools are not part of the platform.
pub(super) const NOT_LINUX: &str =
    "warning: lane ACL copy skipped: getfacl and setfacl are Linux-only";

/// Linux, but one of the three programs could not be started.
pub(super) const TOOLS_MISSING: &str =
    "warning: lane ACL copy skipped: getfacl, setfacl, or chgrp is not on PATH";

/// This host's ACL capability. Off Linux this does not look for the tools.
#[must_use]
pub(super) fn host_kind() -> HostKind {
    #[cfg(target_os = "linux")]
    {
        if tool_on_path("getfacl") && tool_on_path("setfacl") && tool_on_path("chgrp") {
            HostKind::Linux { tools: true }
        } else {
            HostKind::Linux { tools: false }
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        HostKind::NotLinux
    }
}

/// Pick a sibling, or a skip warning. `listing` is the bare-repo directories
/// under the lanes root other than the new repo; this function sorts them.
/// `readable` is consulted only when the host is Linux and the tools exist.
#[must_use]
pub(super) fn plan(
    host: HostKind,
    lanes_root: &Path,
    listing: Result<Vec<PathBuf>, String>,
    readable: &dyn Fn(&Path) -> bool,
) -> AclIntent {
    match host {
        HostKind::NotLinux => skip(NOT_LINUX),
        HostKind::Linux { tools: false } => skip(TOOLS_MISSING),
        HostKind::Linux { tools: true } => plan_linux(lanes_root, listing, readable),
    }
}

/// The first candidate for which `readable` is true. Callers pass candidates
/// already in lexicographic order; [`plan`] sorts before it calls this.
#[must_use]
pub(super) fn choose_sibling<'a>(
    candidates: &'a [PathBuf],
    readable: &dyn Fn(&Path) -> bool,
) -> Option<&'a Path> {
    candidates
        .iter()
        .find(|candidate| readable(candidate.as_path()))
        .map(PathBuf::as_path)
}

/// Other bare-repo directories directly under `lanes_root`. Not sorted.
/// The new `repo` is excluded by file name.
///
/// # Errors
/// A string when the directory cannot be read. Callers turn that into a skip.
pub(super) fn list_siblings(lanes_root: &Path, repo: &Path) -> Result<Vec<PathBuf>, String> {
    let mut siblings = Vec::new();
    let entries = fs::read_dir(lanes_root).map_err(|error| error.to_string())?;
    for entry in entries {
        let path = entry.map_err(|error| error.to_string())?.path();
        if other_bare_repo(&path, repo) {
            siblings.push(path);
        }
    }
    Ok(siblings)
}

/// Turn a copy's `Result` into a warning. `Err` does not propagate.
#[must_use]
pub(super) fn settle(sibling: &Path, result: Result<(), String>) -> AclCopy {
    match result {
        Ok(()) => AclCopy::Copied {
            sibling: sibling.to_path_buf(),
        },
        Err(detail) => AclCopy::Skipped {
            warning: copy_failed_warning(sibling, &detail),
        },
    }
}

/// One line for a copy that did not stick. The lane is still created.
#[must_use]
pub(super) fn copy_failed_warning(sibling: &Path, detail: &str) -> String {
    let detail = flat_detail(detail);
    format!(
        "warning: lane ACL copy from {} failed: {detail}; the lane was still created",
        sibling.display()
    )
}

/// One line when the lanes root cannot be listed.
#[must_use]
pub(super) fn listing_failed_warning(detail: &str) -> String {
    let detail = flat_detail(detail);
    format!("warning: lane ACL copy skipped: could not list the lanes root: {detail}")
}

/// One line when no sibling has a readable ACL (including no siblings at all).
#[must_use]
pub(super) fn no_sibling_warning(lanes_root: &Path) -> String {
    format!(
        "warning: lane ACL copy skipped: no sibling repository under {} has a readable ACL",
        lanes_root.display()
    )
}

/// Report a skip. A successful copy is quiet.
pub(super) fn report(copy: &AclCopy) {
    if let AclCopy::Skipped { warning } = copy {
        tracing::warn!("{warning}");
    }
}

/// Copy or skip for `repo`, which is `<lanes_root>/<name>.git` and already exists.
#[must_use]
pub(super) fn apply(repo: &Path) -> AclCopy {
    let host = host_kind();
    let Some(lanes_root) = lanes_parent(repo) else {
        return AclCopy::Skipped {
            warning: listing_failed_warning("the repository path has no parent"),
        };
    };
    let listing = if needs_siblings(host) {
        list_siblings(lanes_root, repo)
    } else {
        Ok(Vec::new())
    };
    match plan(host, lanes_root, listing, &acl_readable) {
        AclIntent::Skip { warning } => AclCopy::Skipped { warning },
        AclIntent::Copy { sibling } => settle(&sibling, copy_group_and_acl(&sibling, repo)),
    }
}

/// `getfacl` comment lines and trailing effective-rights notes are not ACL
/// entries. `setfacl --set-file` would reject them.
#[cfg(any(target_os = "linux", test))]
#[must_use]
pub(super) fn clean_acl(text: &str) -> String {
    let mut cleaned = String::new();
    for line in text.lines() {
        let entry = match line.split('#').next() {
            Some(entry) => entry.trim(),
            None => continue,
        };
        if entry.is_empty() {
            continue;
        }
        cleaned.push_str(entry);
        cleaned.push('\n');
    }
    cleaned
}

fn skip(warning: &str) -> AclIntent {
    AclIntent::Skip {
        warning: warning.to_string(),
    }
}

fn plan_linux(
    lanes_root: &Path,
    listing: Result<Vec<PathBuf>, String>,
    readable: &dyn Fn(&Path) -> bool,
) -> AclIntent {
    let mut candidates = match listing {
        Ok(candidates) => candidates,
        Err(detail) => {
            return AclIntent::Skip {
                warning: listing_failed_warning(&detail),
            };
        }
    };
    candidates.sort_by(|left, right| left.file_name().cmp(&right.file_name()));
    match choose_sibling(&candidates, readable) {
        Some(sibling) => AclIntent::Copy {
            sibling: sibling.to_path_buf(),
        },
        None => AclIntent::Skip {
            warning: no_sibling_warning(lanes_root),
        },
    }
}

fn needs_siblings(host: HostKind) -> bool {
    matches!(host, HostKind::Linux { tools: true })
}

fn lanes_parent(repo: &Path) -> Option<&Path> {
    let parent = repo.parent()?;
    if parent.as_os_str().is_empty() {
        None
    } else {
        Some(parent)
    }
}

fn other_bare_repo(path: &Path, repo: &Path) -> bool {
    if path.file_name().is_none() || path.file_name() == repo.file_name() {
        return false;
    }
    path.extension().is_some_and(|ext| ext == "git") && path.is_dir()
}

fn flat_detail(detail: &str) -> String {
    let flat = detail.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        "no detail".to_string()
    } else {
        flat
    }
}

#[cfg(target_os = "linux")]
fn acl_readable(path: &Path) -> bool {
    tool_status(Command::new("getfacl").arg("--numeric").arg(path)).is_ok()
}

#[cfg(not(target_os = "linux"))]
fn acl_readable(_path: &Path) -> bool {
    false
}

#[cfg(target_os = "linux")]
fn copy_group_and_acl(sibling: &Path, repo: &Path) -> Result<(), String> {
    // The directory only. Named ACLs on the repo are not default ACLs, so
    // they do not apply to the files `git init` already created.
    let group = tool_status(
        Command::new("chgrp")
            .arg("--reference")
            .arg(sibling)
            .arg(repo),
    );
    let acl = read_acl(sibling).and_then(|text| write_acl(repo, &text));
    match (group, acl) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(group_error), Err(acl_error)) => Err(format!("{group_error}; {acl_error}")),
        (Err(group_error), Ok(())) => Err(group_error),
        (Ok(()), Err(acl_error)) => Err(acl_error),
    }
}

#[cfg(not(target_os = "linux"))]
fn copy_group_and_acl(sibling: &Path, repo: &Path) -> Result<(), String> {
    Err(format!(
        "ACL copy is Linux-only (not copying {} onto {})",
        sibling.display(),
        repo.display()
    ))
}

#[cfg(target_os = "linux")]
fn read_acl(sibling: &Path) -> Result<String, String> {
    let text = tool_output(
        Command::new("getfacl")
            .arg("--omit-header")
            .arg("--numeric")
            .arg("--no-effective")
            .arg(sibling),
    )?;
    Ok(clean_acl(&text))
}

#[cfg(target_os = "linux")]
fn write_acl(repo: &Path, acl: &str) -> Result<(), String> {
    tool_with_stdin(Command::new("setfacl").arg("--set-file=-").arg(repo), acl)
}

#[cfg(target_os = "linux")]
fn tool_on_path(program: &str) -> bool {
    match Command::new(program)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(_) => true,
        Err(_) => false,
    }
}

#[cfg(target_os = "linux")]
fn tool_status(command: &mut Command) -> Result<(), String> {
    let program = program_name(&command);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let output = command
        .output()
        .map_err(|error| spawn_message(&program, &error))?;
    status_error(&program, &output)
}

#[cfg(target_os = "linux")]
fn tool_output(command: &mut Command) -> Result<String, String> {
    let program = program_name(&command);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = command
        .output()
        .map_err(|error| spawn_message(&program, &error))?;
    status_error(&program, &output)?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(target_os = "linux")]
fn tool_with_stdin(command: &mut Command, stdin_text: &str) -> Result<(), String> {
    let program = program_name(&command);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| spawn_message(&program, &error))?;
    let written = write_stdin(child.stdin.take(), stdin_text);
    let output = child
        .wait_with_output()
        .map_err(|error| format!("{program}: {error}"))?;
    written?;
    status_error(&program, &output)
}

#[cfg(target_os = "linux")]
fn write_stdin(stdin: Option<std::process::ChildStdin>, text: &str) -> Result<(), String> {
    let Some(mut stdin) = stdin else {
        return Err("setfacl: stdin was not piped".into());
    };
    std::io::Write::write_all(&mut stdin, text.as_bytes())
        .map_err(|error| format!("setfacl: {error}"))
}

#[cfg(target_os = "linux")]
fn program_name(command: &Command) -> String {
    command.get_program().to_string_lossy().into_owned()
}

#[cfg(target_os = "linux")]
fn status_error(program: &str, output: &std::process::Output) -> Result<(), String> {
    if output.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&output.stderr);
    let detail = detail.trim();
    if detail.is_empty() {
        Err(format!("{program} exited {}", exit_label(&output.status)))
    } else {
        Err(format!(
            "{program} exited {}: {detail}",
            exit_label(&output.status)
        ))
    }
}

#[cfg(target_os = "linux")]
fn exit_label(status: &ExitStatus) -> String {
    status
        .code()
        .map_or_else(|| "from a signal".to_string(), |code| code.to_string())
}

#[cfg(target_os = "linux")]
fn spawn_message(program: &str, error: &std::io::Error) -> String {
    if error.kind() == ErrorKind::NotFound {
        format!("{program} is not on PATH")
    } else {
        format!("{program}: {error}")
    }
}
