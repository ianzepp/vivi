//! `target_log` and `target_show`: read a target's lane on this host, so a
//! land can be checked over MCP instead of by reading the repository on the
//! host. Read-only and operator-only, like the other pump views.
//!
//! The lane is found from the target's `remote_url`: a URL that starts with
//! `HOST_GIT_URL_PREFIX` is the path after the prefix under
//! `HOST_LANES_ROOT`; a `file://` URL or an absolute path is read as it is.
//! A target without a lane on this host is refused.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};
use vivi::mailspace::Mailspace;
use vivi::pump::config::PumpConfig;

use crate::identities::IdentityRecord;
use crate::mcp::host::HostConfig;
use crate::mcp::tools::{ToolError, opt_str, opt_usize, req_str};
use crate::safety::RateLimiter;

use super::lib_error;
use super::pump::{acting_key, authorize};

const DEFAULT_LIMIT: usize = 20;
const MAX_LIMIT: usize = 200;
/// The most patch text one `target_show` answers; the rest is cut.
const PATCH_CAP: usize = 64 * 1024;
const FIELD: char = '\u{1f}';

pub(in crate::mcp) fn target_log(
    _limiter: &RateLimiter,
    _project_id: &str,
    root: &Path,
    caller: &IdentityRecord,
    acting: Option<&str>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let requested = acting_key("target_log", "read a target's history", acting)?;
    authorize("target_log", root, caller, requested)?;
    let lane = Lane::resolve(root, opt_str(arguments, "target")?)?;
    let limit = opt_usize(arguments, "limit", DEFAULT_LIMIT, MAX_LIMIT)?;
    let format = format!("--format=%H{FIELD}%P{FIELD}%an{FIELD}%aI{FIELD}%s");
    let text = lane.git(&["log", &format, &format!("-n{limit}"), &lane.branch])?;
    let commits: Vec<Value> = text.lines().map(commit_json).collect();
    Ok(json!({
        "target": lane.target,
        "branch": lane.branch,
        "lane": lane.path,
        "commits": commits,
    }))
}

pub(in crate::mcp) fn target_show(
    _limiter: &RateLimiter,
    _project_id: &str,
    root: &Path,
    caller: &IdentityRecord,
    acting: Option<&str>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let requested = acting_key("target_show", "read a target's commit", acting)?;
    authorize("target_show", root, caller, requested)?;
    let lane = Lane::resolve(root, opt_str(arguments, "target")?)?;
    let rev = req_str(arguments, "commit")?;
    if rev.is_empty() || rev.starts_with('-') || rev.chars().any(char::is_whitespace) {
        return Err(ToolError::new(format!("commit {rev:?} is not a revision")));
    }
    let format = format!("--format=%H{FIELD}%P{FIELD}%an{FIELD}%aI{FIELD}%s");
    let head = lane.git(&["show", "--no-patch", &format, rev])?;
    let stat = lane.git(&["show", "--format=", "--stat", rev])?;
    let patch = lane.git(&["show", "--format=", "--patch", rev])?;
    let truncated = patch.len() > PATCH_CAP;
    let mut cut = PATCH_CAP.min(patch.len());
    while !patch.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut out = commit_json(head.lines().next().unwrap_or_default());
    if let Some(object) = out.as_object_mut() {
        object.insert("target".into(), json!(lane.target));
        object.insert("stat".into(), json!(stat.trim_end()));
        object.insert("patch".into(), json!(&patch[..cut]));
        object.insert("patch_truncated".into(), json!(truncated));
    }
    Ok(out)
}

fn commit_json(line: &str) -> Value {
    let mut parts = line.split(FIELD);
    let mut next = || parts.next().unwrap_or_default().to_owned();
    let (commit, parents, author, date, subject) = (next(), next(), next(), next(), next());
    json!({
        "commit": commit,
        "parents": parents.split_whitespace().collect::<Vec<_>>(),
        "author": author,
        "date": date,
        "subject": subject,
    })
}

/// One target's lane on this host and its main branch.
struct Lane {
    target: String,
    branch: String,
    path: PathBuf,
}

impl Lane {
    /// `target`, or the project's primary target when none is named.
    fn resolve(root: &Path, target: Option<&str>) -> Result<Self, ToolError> {
        let mailspace = Mailspace::open_root(root.to_path_buf()).map_err(lib_error)?;
        let cfg: PumpConfig = mailspace
            .config
            .pump
            .ok_or_else(|| ToolError::new("the project has no [pump] table"))?;
        let target = match target {
            Some(target) => target.to_owned(),
            None => cfg.primary_target().ok_or_else(|| {
                ToolError::new("the project has several targets and no `default`; name one")
            })?,
        };
        let configured = cfg.target_configs();
        let spec = configured
            .get(&target)
            .ok_or_else(|| ToolError::new(format!("unknown target {target:?}")))?;
        let url = spec.remote_url.as_deref().ok_or_else(|| {
            ToolError::new(format!(
                "target {target:?} has no remote_url naming its lane"
            ))
        })?;
        let path = lane_path(url, &HostConfig::from_env()).ok_or_else(|| {
            ToolError::new(format!("target {target:?}: lane {url} is not on this host"))
        })?;
        Ok(Self {
            branch: spec.main.clone(),
            target,
            path,
        })
    }

    fn git(&self, args: &[&str]) -> Result<String, ToolError> {
        let output = Command::new("git")
            .arg("--git-dir")
            .arg(&self.path)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|err| ToolError::new(format!("git could not run: {err}")))?;
        if !output.status.success() {
            return Err(ToolError::new(format!(
                "git {} on target {} failed: {}",
                args.first().copied().unwrap_or_default(),
                self.target,
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

/// The local path of the lane `url` names, when it is on this host.
fn lane_path(url: &str, host: &HostConfig) -> Option<PathBuf> {
    if let (Some(prefix), Some(lanes)) = (&host.git_url_prefix, &host.lanes_root)
        && let Some(rest) = url.strip_prefix(prefix.as_str())
        && !rest.split('/').any(|part| part.is_empty() || part == "..")
    {
        return Some(lanes.join(rest));
    }
    if let Some(path) = url.strip_prefix("file://") {
        return Some(PathBuf::from(path));
    }
    Path::new(url).is_absolute().then(|| PathBuf::from(url))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host() -> HostConfig {
        HostConfig {
            projects_root: None,
            lanes_root: Some(PathBuf::from("/mnt/vivi/lanes")),
            git_url_prefix: Some("git@hub:".into()),
            default_pool: None,
        }
    }

    #[test]
    fn prefixed_urls_map_under_the_lanes_root() {
        assert_eq!(
            lane_path("git@hub:faberlang/radix.git", &host()),
            Some(PathBuf::from("/mnt/vivi/lanes/faberlang/radix.git"))
        );
        assert_eq!(lane_path("git@hub:../etc.git", &host()), None);
        assert_eq!(lane_path("git@elsewhere:x/y.git", &host()), None);
    }

    #[test]
    fn file_urls_and_absolute_paths_are_read_as_they_are() {
        assert_eq!(
            lane_path("file:///srv/lanes/a/b.git", &host()),
            Some(PathBuf::from("/srv/lanes/a/b.git"))
        );
        assert_eq!(
            lane_path("/srv/lanes/a/b.git", &HostConfig::default()),
            Some(PathBuf::from("/srv/lanes/a/b.git"))
        );
    }
}
