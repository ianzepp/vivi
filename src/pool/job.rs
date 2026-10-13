//! One job: resolving what to run, rendering the harness command, and
//! packing everything the slot needs into a single archive.
//!
//! The runner and the default rules are embedded and generic. A job carries
//! the Git token in exactly one file, `token`, which only the root-owned
//! runner reads; the files the agent can see (`brief.md`, `rules.md`,
//! `harness.cmd`) never hold it. A bridge grant, when the pool mints one, is
//! delivered the same way, as `bridge.key`; the archive then also carries an
//! agent-visible `mcp.json`, which names the key only by reference, and the
//! brief the worker reads is a pointer at the task on the server.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};

use super::bridge::Grant;
use super::config::PoolCommitter;
use super::github::Secret;
use crate::error::VivariumError;

/// The generic job runner, embedded in the binary.
pub const RUNNER: &str = include_str!("job-run.sh");
/// The standing rules a worker gets when the config names none.
pub const DEFAULT_RULES: &str = include_str!("rules.md");
/// Most extra refs (`--with`) a job may name.
pub const WITH_LIMIT: usize = 8;

/// Where the agent finds its inputs inside the slot.
pub const AGENT_BRIEF: &str = "/home/hand/brief.md";
pub const AGENT_RULES: &str = "/home/hand/rules.md";
/// Where the agent finds the MCP bridge configuration (a grant only).
pub const AGENT_MCP: &str = "/home/hand/mcp.json";
/// Where the job archive lands (config file; replaced by each dispatch).
pub const JOB_ARCHIVE: &str = "/work/job.tgz";

/// Seconds the runner may use beyond the agent's budget (fetch, commit, push).
pub const RUNNER_SLACK_SECS: u64 = 600;
/// Largest archive a dispatch will deliver.
pub const MAX_ARCHIVE_BYTES: usize = 512 * 1024;

const PLACEHOLDERS: [&str; 5] = ["model", "brief", "rules", "reasoning", "mcp_config"];

/// The first `{name}` in `run` that is not a known placeholder. `${NAME}`
/// shell expansions are not placeholders.
#[must_use]
pub fn unknown_placeholder(run: &str) -> Option<String> {
    let mut from = 0;
    while let Some(offset) = run[from..].find('{') {
        let open = from + offset;
        let close = run[open + 1..].find('}')?;
        let name = &run[open + 1..open + 1 + close];
        let word = !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase() || c == '_');
        let dollar = open > 0 && run.as_bytes()[open - 1] == b'$';
        if !dollar && word && !PLACEHOLDERS.contains(&name) {
            return Some(name.to_string());
        }
        from = open + 1;
    }
    None
}

/// Whether the harness template holds the `{reasoning}` placeholder (a
/// `${reasoning}` shell expansion does not count).
#[must_use]
pub fn uses_reasoning(run: &str) -> bool {
    let mut from = 0;
    while let Some(offset) = run[from..].find("{reasoning}") {
        let open = from + offset;
        let dollar = open > 0 && run.as_bytes()[open - 1] == b'$';
        if !dollar {
            return true;
        }
        from = open + 1;
    }
    false
}

/// Fill the harness template. `{model}` is a vetted slug; `{reasoning}` is
/// the chosen reasoning level, if any; `{brief}`, `{rules}` and `{mcp_config}`
/// are fixed paths inside the slot (`mcp.json` is carried only when the job
/// has a bridge grant).
#[must_use]
pub fn render_harness(run: &str, model: &str, reasoning: Option<&str>) -> String {
    run.replace("{model}", model)
        .replace("{brief}", AGENT_BRIEF)
        .replace("{rules}", AGENT_RULES)
        .replace("{reasoning}", reasoning.unwrap_or(""))
        .replace("{mcp_config}", AGENT_MCP)
}

/// `from:` and `branch:` lines found in a task body.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BodyRefs {
    pub from: Option<String>,
    pub branch: Option<String>,
}

/// Read `from: <ref>` and `branch: <ref>` lines from a task body. The first
/// of each wins; surrounding whitespace and a leading list dash are ignored.
#[must_use]
pub fn body_refs(body: &str) -> BodyRefs {
    let mut refs = BodyRefs::default();
    for line in body.lines() {
        let line = line.trim().trim_start_matches(['-', '*']).trim();
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() || value.contains(char::is_whitespace) {
            continue;
        }
        match key.trim().to_ascii_lowercase().as_str() {
            "from" if refs.from.is_none() => refs.from = Some(value.to_string()),
            "branch" if refs.branch.is_none() => refs.branch = Some(value.to_string()),
            _ => {}
        }
    }
    refs
}

/// Where to start and where to push, after flags and body are merged. A
/// read-only job has no branch: `branch` is empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refs {
    pub from: String,
    pub branch: String,
}

/// An extra ref a job may read (`--with`, a `with:` body line) and the commit
/// it resolved to. `commit` is empty until resolution, which happens before
/// any slot is claimed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WithRef {
    pub name: String,
    pub commit: String,
}

/// The commit or branch a job starts from: the flag, else the body line.
///
/// # Errors
/// Returns an error if the value is missing or not a plain ref name.
pub fn resolve_from(flag_from: Option<&str>, body: &str) -> Result<String, VivariumError> {
    let from = flag_from
        .map(str::to_string)
        .or(body_refs(body).from)
        .ok_or_else(|| missing("from"))?;
    if plain_ref(&from) {
        Ok(from)
    } else {
        Err(bad_ref("from", &from))
    }
}

/// Flags win over body lines; both values are required one way or the other.
///
/// # Errors
/// Returns an error if either value is missing or not a plain ref name.
pub fn resolve_refs(
    flag_from: Option<&str>,
    flag_branch: Option<&str>,
    body: &str,
) -> Result<Refs, VivariumError> {
    let from = resolve_from(flag_from, body)?;
    let branch = flag_branch
        .map(str::to_string)
        .or(body_refs(body).branch)
        .ok_or_else(|| missing("branch"))?;
    if !plain_ref(&branch)
        || std::path::Path::new(&branch)
            .extension()
            .is_some_and(|e| e == "lock")
    {
        return Err(bad_ref("branch", &branch));
    }
    Ok(Refs { from, branch })
}

/// Every `with: <ref>` line of a task body, in order: the same trimming and
/// list-dash rules as [`body_refs`], but every line counts.
#[must_use]
pub fn body_with_refs(body: &str) -> Vec<String> {
    body.lines()
        .filter_map(|line| {
            let line = line.trim().trim_start_matches(['-', '*']).trim();
            let (key, value) = line.split_once(':')?;
            let value = value.trim();
            (key.trim().eq_ignore_ascii_case("with")
                && !value.is_empty()
                && !value.contains(char::is_whitespace))
            .then(|| value.to_string())
        })
        .collect()
}

/// The extra refs a job may read: the `--with` flags when any are given, else
/// every `with: <ref>` body line in order. Duplicates are dropped keeping the
/// first, at most [`WITH_LIMIT`] are kept, and each value must be a plain ref
/// that is neither the start ref (`from`) nor the push target (`branch`).
///
/// # Errors
/// Returns an error naming the offending value when one is not a plain ref,
/// names `from` or `branch`, or when more than [`WITH_LIMIT`] remain.
pub fn resolve_with(
    flags: &[String],
    body: &str,
    from: &str,
    branch: &str,
) -> Result<Vec<String>, VivariumError> {
    let listed = body_with_refs(body);
    let values = if flags.is_empty() { &listed } else { flags };
    let mut refs: Vec<String> = Vec::new();
    for value in values {
        check_with(value, from, branch)?;
        if refs.contains(value) {
            continue;
        }
        refs.push(value.clone());
        if refs.len() > WITH_LIMIT {
            return Err(VivariumError::Message(format!(
                "at most {WITH_LIMIT} with refs are allowed ({n} given)",
                n = values
                    .iter()
                    .collect::<std::collections::HashSet<_>>()
                    .len()
            )));
        }
    }
    Ok(refs)
}

/// One extra ref must be a plain ref that is neither the start ref nor the
/// push target (both already travel with the job).
///
/// # Errors
/// Returns an error naming the value when a rule is broken.
fn check_with(value: &str, from: &str, branch: &str) -> Result<(), VivariumError> {
    if !plain_ref(value) {
        return Err(VivariumError::Message(format!(
            "with \"{value}\" is not a plain commit or branch name"
        )));
    }
    if value == from {
        return Err(VivariumError::Message(format!(
            "with \"{value}\" is the start ref (from); name a different ref"
        )));
    }
    if value == branch {
        return Err(VivariumError::Message(format!(
            "with \"{value}\" is the push target (branch); name a different ref"
        )));
    }
    Ok(())
}

fn missing(what: &str) -> VivariumError {
    VivariumError::Message(format!(
        "no {what}: pass --{what} or put a `{what}: <ref>` line in the task body"
    ))
}

fn bad_ref(what: &str, value: &str) -> VivariumError {
    VivariumError::Message(format!(
        "{what} \"{value}\" is not a plain commit or branch name"
    ))
}

/// A plain commit or branch name: letters, digits, dot, underscore, dash and
/// slash; no leading dash or slash, no `..`, no trailing slash.
#[must_use]
pub fn plain_ref(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with(['-', '/'])
        && !value.ends_with('/')
        && !value.contains("..")
        && !value.contains("//")
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'))
}

/// Check `model` against the configured slugs.
///
/// # Errors
/// Returns an error naming the allowed slugs when `model` is not one of them.
pub fn check_model(model: &str, allowed: &[String]) -> Result<(), VivariumError> {
    if allowed.iter().any(|m| m == model) {
        return Ok(());
    }
    let list = if allowed.is_empty() {
        "none configured".to_string()
    } else {
        allowed.join(", ")
    };
    Err(VivariumError::Message(format!(
        "model \"{model}\" is not in [harness].models of the pool ({list})"
    )))
}

/// What a worktree slot does with the job's branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchAction {
    /// The branch does not exist: it is made at the start commit when the
    /// job has something to commit.
    Create,
    /// The branch exists and is the one the job continues: checked out as it
    /// is.
    Continue,
    /// A read-only job: no branch is made or touched; the work stays on a
    /// detached HEAD.
    Detached,
}

impl BranchAction {
    /// The word the runner is given.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Continue => "continue",
            Self::Detached => "detached",
        }
    }
}

/// How a job starts in a worktree slot: decided before a slot is claimed
/// (see [`super::local_branch`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkout {
    pub action: BranchAction,
    /// The commit the job starts from (the tip of the branch when continuing).
    pub start: String,
}

/// Everything about a job that is known before a slot is chosen.
#[derive(Debug, Clone)]
pub struct JobPlan {
    pub job: String,
    /// Full Git address the runner fetches from and pushes to. Empty for a
    /// worktree slot, which has no remote.
    pub remote: String,
    /// Set for a worktree slot: how the job starts and what happens to the
    /// branch.
    pub checkout: Option<Checkout>,
    pub task: String,
    pub subject: String,
    pub brief: String,
    pub rules: String,
    pub refs: Refs,
    /// Extra refs the job may read; `commit` is filled before a slot is
    /// claimed (empty in a plan that is not resolved yet).
    pub with: Vec<WithRef>,
    pub model: String,
    pub budget: u64,
    pub depth: u32,
    pub harness_cmd: String,
    pub env_names: Vec<String>,
    pub committer: PoolCommitter,
    /// The job publishes nothing: no commit, no push, no branch.
    pub read_only: bool,
    /// Commands the runner records the output of before the harness starts
    /// (the pool's `facts`).
    pub facts: Vec<String>,
    /// Tracked file paths the runner puts back to the start commit before it
    /// commits (the pool's `restore`).
    pub restore: Vec<String>,
    /// The MCP bridge grant minted at dispatch when the pool's bridge is
    /// enabled: delivered only as the archive's `bridge.key` file, its
    /// server and project id also into `params.env` and into the agent's
    /// `mcp.json`.
    pub bridge: Option<Grant>,
}

/// One file inside the job archive, under `job/`.
#[derive(Debug, Clone)]
pub struct BundleFile {
    pub name: &'static str,
    pub content: Vec<u8>,
    pub mode: u32,
}

/// The files of one job.
#[derive(Debug, Clone)]
pub struct Bundle {
    pub files: Vec<BundleFile>,
}

impl Bundle {
    /// The files whose contents the agent can read or see in a process list.
    #[must_use]
    pub fn agent_visible(&self) -> Vec<&BundleFile> {
        self.files
            .iter()
            .filter(|f| matches!(f.name, "brief.md" | "rules.md" | "harness.cmd" | "mcp.json"))
            .collect()
    }
}

/// Single-quote a value for a shell-sourced file.
#[must_use]
pub fn sh_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// Lay out the files of a job for `slot`. `token` goes into the `token` file
/// and nowhere else; a minted bridge grant goes into `bridge.key` the same
/// way, with its server and project id in `params.env` and its no-secret MCP
/// configuration in the agent-visible `mcp.json`.
#[must_use]
pub fn build_bundle(spec: &JobPlan, slot: &str, token: &Secret) -> Bundle {
    let mut fields = vec![
        ("REMOTE", spec.remote.clone()),
        ("JOB", spec.job.clone()),
        ("TASK", spec.task.clone()),
        ("FROM", spec.refs.from.clone()),
        ("BRANCH", spec.refs.branch.clone()),
        ("DEPTH", spec.depth.to_string()),
        ("BUDGET", spec.budget.to_string()),
        ("MODEL", spec.model.clone()),
        ("SLOT", slot.to_string()),
        ("SUBJECT", first_line(&spec.subject)),
        ("COMMIT_NAME", spec.committer.name.clone()),
        ("COMMIT_EMAIL", spec.committer.email.clone()),
        ("ENV_NAMES", spec.env_names.join(" ")),
    ];
    if spec.read_only {
        fields.push(("READ_ONLY", "1".to_string()));
    }
    if let Some(checkout) = &spec.checkout {
        fields.push(("LOCAL_ACTION", checkout.action.word().to_string()));
        fields.push(("LOCAL_START", checkout.start.clone()));
    }
    if let Some(bridge) = &spec.bridge {
        fields.push(("BRIDGE_SERVER", bridge.server.clone()));
        fields.push(("BRIDGE_PROJECT", bridge.project_id.clone()));
    }
    let params: String = fields
        .into_iter()
        .map(|(k, v)| format!("{k}={}\n", sh_quote(&v)))
        .collect();
    let file = |name, content: &str, mode| BundleFile {
        name,
        content: content.as_bytes().to_vec(),
        mode,
    };
    let mut files = vec![
        file("run.sh", RUNNER, 0o700),
        file("params.env", &params, 0o600),
        file("token", token.expose(), 0o600),
        file("id", &spec.job, 0o600),
        file(
            "limit",
            &(spec.budget + RUNNER_SLACK_SECS).to_string(),
            0o600,
        ),
        file("brief.md", &lf(&spec.brief), 0o644),
        file("rules.md", &spec.rules, 0o644),
        file("harness.cmd", &spec.harness_cmd, 0o644),
    ];
    if let Some(bridge) = &spec.bridge {
        files.extend(bridge_files(bridge));
    }
    if !spec.facts.is_empty() {
        let list = spec.facts.join("\n") + "\n";
        files.push(file("facts.cmd", &list, 0o600));
    }
    if let Some(list) = with_list(&spec.with) {
        files.push(file("with.list", &list, 0o600));
    }
    if !spec.restore.is_empty() {
        let list = spec.restore.join("\n") + "\n";
        files.push(file("restore.list", &list, 0o600));
    }
    Bundle { files }
}

/// The text of `with.list`: one `<ref> <commit>` line per extra ref,
/// LF-terminated; `None` when the job names none, so no file is written.
fn with_list(with: &[WithRef]) -> Option<String> {
    if with.is_empty() {
        return None;
    }
    Some(
        with.iter()
            .map(|w| format!("{} {}", w.name, w.commit))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
}

/// Line endings as LF. A stored task body has CRLF (and CR CR LF where the
/// text already had CRLF); the brief the worker reads should not. A lone CR
/// inside a line is kept.
#[must_use]
pub fn lf(text: &str) -> String {
    text.split('\n')
        .map(|line| line.trim_end_matches('\r'))
        .collect::<Vec<_>>()
        .join("\n")
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or_default().trim().to_string()
}

/// The two files a job with a bridge grant carries beyond the standard set:
/// the root-only key, and the agent-visible MCP configuration that names the
/// key only by reference.
fn bridge_files(bridge: &Grant) -> Vec<BundleFile> {
    let file = |name, content: &str, mode| BundleFile {
        name,
        content: content.as_bytes().to_vec(),
        mode,
    };
    vec![
        file("bridge.key", bridge.key.expose(), 0o600),
        file(
            "mcp.json",
            &super::bridge::mcp_config_json(&bridge.server, &bridge.project_id),
            0o644,
        ),
    ]
}

/// Pack a bundle into one gzip-compressed tar archive with a top-level
/// `job/` directory, using the system `tar`.
///
/// # Errors
/// Returns an error if `tar` is unavailable or fails, or the archive exceeds
/// [`MAX_ARCHIVE_BYTES`].
pub fn pack(bundle: &Bundle) -> Result<Vec<u8>, VivariumError> {
    let dir = tempfile::tempdir()?;
    let job = dir.path().join("job");
    std::fs::create_dir(&job)?;
    std::fs::set_permissions(&job, std::fs::Permissions::from_mode(0o700))?;
    for file in &bundle.files {
        let path = job.join(file.name);
        let mut handle = std::fs::File::create(&path)?;
        handle.write_all(&file.content)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(file.mode))?;
    }
    let output = Command::new("tar")
        .env("COPYFILE_DISABLE", "1")
        .args(["-czf", "-", "-C"])
        .arg(dir.path())
        .arg("job")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| VivariumError::Message(format!("failed to run tar: {e}")))?;
    if !output.status.success() {
        return Err(VivariumError::Message(format!(
            "tar failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    if output.stdout.len() > MAX_ARCHIVE_BYTES {
        return Err(VivariumError::Message(format!(
            "the job is {} bytes; the limit is {MAX_ARCHIVE_BYTES} (shorten the brief)",
            output.stdout.len()
        )));
    }
    Ok(output.stdout)
}

#[cfg(test)]
#[path = "job_test.rs"]
mod tests;

#[cfg(test)]
#[path = "job_runner_test.rs"]
mod runner_tests;

#[cfg(test)]
#[path = "job_runner_restore_test.rs"]
mod runner_restore_tests;

#[cfg(test)]
#[path = "job_runner_with_test.rs"]
mod with_runner_tests;

#[cfg(test)]
#[path = "job_runner_merge_test.rs"]
mod merge_runner_tests;

#[cfg(test)]
#[path = "job_runner_bridge_test.rs"]
mod runner_bridge_tests;
