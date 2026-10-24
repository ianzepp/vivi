//! `lane_create`: the projects half of `vivi project lane add`.
//!
//! The server runs as the projects owner and must not create bare
//! repositories under the lanes root. [`add_split`](lane_add::add_split)
//! performs the lanes half whenever that root is writable, so when this
//! process can write it the tool drops write permission for the library
//! call and restores it afterwards. A missing bare repository is refused
//! before that call. The refusal carries the same `vivi project lane add`
//! command the library prints for the lanes owner, plus pump readiness.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use vivi::project_create::lane_add::{self, LaneAdd, LaneAdded};
use vivi::project_create::{Half, Readiness, RemainingHalf};
use vivi::pump::config::valid_target_id;

use crate::AppState;

use super::super::tools::{ToolError, opt_str, req_str};
use super::{ENV_LANES_ROOT, ENV_PROJECTS_ROOT, HostConfig};

/// A failed `lane_create`. `summary` is the audit line; `body` is the
/// tool result (`isError`).
pub(crate) struct LaneFailure {
    pub summary: String,
    pub body: Value,
}

impl LaneFailure {
    fn plain(message: impl Into<String>) -> Self {
        let summary = message.into();
        Self {
            body: json!({ "error": summary }),
            summary,
        }
    }
}

impl From<ToolError> for LaneFailure {
    fn from(err: ToolError) -> Self {
        Self::plain(err.message)
    }
}

/// Attach one existing lane. The `/v1/mcp` host tool.
pub(crate) async fn lane_create(state: &AppState, arguments: &Value) -> Result<Value, LaneFailure> {
    let host = state.host.clone();
    let arguments = arguments.clone();
    tokio::task::spawn_blocking(move || attach(&host, &arguments))
        .await
        .map_err(|err| LaneFailure::plain(format!("lane_create task failed: {err}")))?
}

fn attach(host: &HostConfig, arguments: &Value) -> Result<Value, LaneFailure> {
    let request = request(host, arguments)?;
    let project = existing_dir(&request.project_root, "project root")?;
    let lanes_dir = existing_dir(&request.lanes_root, "lanes root")?;
    let repo = lanes_dir.join(format!("{}.git", request.target_id));
    if !repo.is_dir() {
        return refuse_missing(&request, &project, &lanes_dir, &repo);
    }
    let added = attach_existing(&request, &lanes_dir)?;
    let readiness = readiness_of(&project)?;
    encode(&added, &readiness, &project)
}

fn request(host: &HostConfig, arguments: &Value) -> Result<LaneAdd, LaneFailure> {
    let name = req_str(arguments, "name")?;
    vivi::project_create::check_name(name).map_err(|err| LaneFailure::plain(err.to_string()))?;
    let target = req_str(arguments, "target")?;
    if !valid_target_id(target) {
        return Err(LaneFailure::plain(format!(
            "target id {target:?} must use lowercase letters, digits and hyphens"
        )));
    }
    let projects_root = configured(host, "projects root", ENV_PROJECTS_ROOT, |h| {
        h.projects_root.clone()
    })?;
    let lanes_root = configured(host, "lanes root", ENV_LANES_ROOT, |h| h.lanes_root.clone())?;
    let mut request = LaneAdd::new(projects_root.join(name), target, lanes_root);
    request.git_url_prefix = match opt_str(arguments, "git_url_prefix")? {
        Some(prefix) => Some(prefix.to_owned()),
        None => host.git_url_prefix.clone(),
    };
    request.seed = super::opt_bool(arguments, "seed", false)?;
    Ok(request)
}

fn configured(
    host: &HostConfig,
    what: &str,
    var: &str,
    get: impl Fn(&HostConfig) -> Option<PathBuf>,
) -> Result<PathBuf, LaneFailure> {
    get(host).ok_or_else(|| {
        LaneFailure::plain(format!(
            "lane_create is not configured on this host: {what} ({var}) is not set"
        ))
    })
}

fn existing_dir(path: &Path, what: &str) -> Result<PathBuf, LaneFailure> {
    match path.canonicalize() {
        Ok(dir) if dir.is_dir() => Ok(dir),
        _ => Err(LaneFailure::plain(format!(
            "{what} {} is not an existing directory",
            path.display()
        ))),
    }
}

/// The bare repository is not there. Do not call [`add_split`]: on a
/// writable lanes root it would create the repository.
fn refuse_missing(
    request: &LaneAdd,
    project: &Path,
    lanes_dir: &Path,
    repo: &Path,
) -> Result<Value, LaneFailure> {
    let readiness = readiness_of(project)?;
    let message = format!(
        "lane repository {} does not exist; the lanes half must run first",
        repo.display()
    );
    let remaining = RemainingHalf {
        half: Half::Lanes,
        root: lanes_dir.to_path_buf(),
        command: lanes_owner_command(request),
    };
    Err(LaneFailure {
        summary: message.clone(),
        body: json!({
            "error": message,
            "name": request.project_root.file_name().and_then(|n| n.to_str()),
            "remaining": remaining,
            "readiness": readiness,
        }),
    })
}

fn attach_existing(request: &LaneAdd, lanes_dir: &Path) -> Result<LaneAdded, LaneFailure> {
    let _hold = hold_lanes(lanes_dir)?;
    lane_add::add_split(request).map_err(|err| LaneFailure::plain(err.to_string()))
}

fn readiness_of(project: &Path) -> Result<Readiness, LaneFailure> {
    super::rungs::assess(project).map_err(LaneFailure::plain)
}

fn encode(added: &LaneAdded, readiness: &Readiness, project: &Path) -> Result<Value, LaneFailure> {
    let mut value = serde_json::to_value(added)
        .map_err(|err| LaneFailure::plain(format!("failed to encode lane_create result: {err}")))?;
    let Some(object) = value.as_object_mut() else {
        return Err(LaneFailure::plain("failed to encode lane_create result"));
    };
    if let Some(name) = project.file_name().and_then(|name| name.to_str()) {
        object.insert("name".to_owned(), json!(name));
    }
    object.insert(
        "readiness".to_owned(),
        serde_json::to_value(readiness).map_err(|err| {
            LaneFailure::plain(format!("failed to encode lane_create readiness: {err}"))
        })?,
    );
    Ok(value)
}

/// The exact command [`lane_add::add_split`] returns in `remaining` when
/// it runs only the projects half. Kept in step with that private
/// formatter: safe shell words stay bare, anything else is single-quoted.
fn lanes_owner_command(request: &LaneAdd) -> String {
    let mut command = format!(
        "vivi project lane add {} {} --lanes-root {}",
        shell_arg(&request.project_root.display().to_string()),
        shell_arg(&request.target_id),
        shell_arg(&request.lanes_root.display().to_string()),
    );
    if let Some(prefix) = &request.git_url_prefix {
        command.push_str(&format!(" --git-url-prefix {}", shell_arg(prefix)));
    }
    if request.seed {
        command.push_str(" --seed");
    }
    command
}

fn shell_arg(arg: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "-_./:=@+%".contains(c);
    if !arg.is_empty() && arg.chars().all(safe) {
        return arg.to_string();
    }
    format!("'{}'", arg.replace('\'', "'\\''"))
}

/// Hold the lanes root read-only for the library call when this process
/// can create entries in it, so [`add_split`] takes the projects half.
/// Restored when dropped. A root this process cannot write is left alone:
/// the library's own probe (only `PermissionDenied` counts) already
/// selects that half.
fn hold_lanes(dir: &Path) -> Result<LanesHold, LaneFailure> {
    if !probe_writable(dir)? {
        return Ok(LanesHold::Untouched);
    }
    #[cfg(unix)]
    {
        let hold = LanesHold::revoke(dir)?;
        if probe_writable(dir)? {
            return Err(LaneFailure::plain(format!(
                "lane_create refuses to create under the lanes root {}; this process can still \
                 write it",
                dir.display()
            )));
        }
        return Ok(hold);
    }
    #[cfg(not(unix))]
    Err(LaneFailure::plain(format!(
        "lane_create refuses to create under the lanes root {}; this process can write it",
        dir.display()
    )))
}

/// Whether this process can create an entry in `dir`. Same rule as the
/// library: only a permission denial means no.
fn probe_writable(dir: &Path) -> Result<bool, LaneFailure> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.subsec_nanos());
    let probe = dir.join(format!(".vivi-probe-lane-{}-{nanos}", std::process::id()));
    match fs::create_dir(&probe) {
        Ok(()) => {
            fs::remove_dir(&probe).map_err(|err| {
                LaneFailure::plain(format!(
                    "lane_create could not remove its write probe {}: {err}",
                    probe.display()
                ))
            })?;
            Ok(true)
        }
        Err(err) if err.kind() == ErrorKind::PermissionDenied => Ok(false),
        Err(err) => Err(LaneFailure::plain(format!(
            "lane_create could not probe the lanes root {}: {err}",
            dir.display()
        ))),
    }
}

enum LanesHold {
    Untouched,
    #[cfg(unix)]
    Revoked {
        path: PathBuf,
        mode: u32,
    },
}

impl LanesHold {
    #[cfg(unix)]
    fn revoke(dir: &Path) -> Result<Self, LaneFailure> {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(dir)
            .map_err(|err| {
                LaneFailure::plain(format!(
                    "lane_create could not read permissions on {}: {err}",
                    dir.display()
                ))
            })?
            .permissions()
            .mode();
        let perms = fs::Permissions::from_mode(mode & !0o222);
        fs::set_permissions(dir, perms).map_err(|err| {
            LaneFailure::plain(format!(
                "lane_create refuses to create under the lanes root {}; could not drop write \
                 permission: {err}",
                dir.display()
            ))
        })?;
        Ok(Self::Revoked {
            path: dir.to_path_buf(),
            mode,
        })
    }
}

impl Drop for LanesHold {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Self::Revoked { path, mode } = self {
            use std::os::unix::fs::PermissionsExt;
            let perms = fs::Permissions::from_mode(*mode);
            let failed = fs::set_permissions(path.as_path(), perms).err();
            if let Some(err) = failed {
                tracing::error!(
                    "lane_create could not restore permissions on {}: {err}",
                    path.display()
                );
            }
        }
    }
}
