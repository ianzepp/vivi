//! One job: resolving what to run, rendering the harness command, and
//! packing everything the slot needs into a single archive.
//!
//! The runner and the default rules are embedded and generic. A job carries
//! the Git token in exactly one file, `token`, which only the root-owned
//! runner reads; the files the agent can see (`brief.md`, `rules.md`,
//! `harness.cmd`) never hold it.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};

use super::config::PoolCommitter;
use super::github::Secret;
use crate::error::VivariumError;

/// The generic job runner, embedded in the binary.
pub const RUNNER: &str = include_str!("job-run.sh");
/// The standing rules a worker gets when the config names none.
pub const DEFAULT_RULES: &str = include_str!("rules.md");

/// Where the agent finds its inputs inside the slot.
pub const AGENT_BRIEF: &str = "/home/hand/brief.md";
pub const AGENT_RULES: &str = "/home/hand/rules.md";
/// Where the job archive lands (config file; replaced by each dispatch).
pub const JOB_ARCHIVE: &str = "/work/job.tgz";

/// Seconds the runner may use beyond the agent's budget (fetch, commit, push).
pub const RUNNER_SLACK_SECS: u64 = 600;
/// Largest archive a dispatch will deliver.
pub const MAX_ARCHIVE_BYTES: usize = 512 * 1024;

const PLACEHOLDERS: [&str; 3] = ["model", "brief", "rules"];

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

/// Fill the harness template. `{model}` is a vetted slug; `{brief}` and
/// `{rules}` are fixed paths inside the slot.
#[must_use]
pub fn render_harness(run: &str, model: &str) -> String {
    run.replace("{model}", model)
        .replace("{brief}", AGENT_BRIEF)
        .replace("{rules}", AGENT_RULES)
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

/// Where to start and where to push, after flags and body are merged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refs {
    pub from: String,
    pub branch: String,
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
    let found = body_refs(body);
    let from = flag_from
        .map(str::to_string)
        .or(found.from)
        .ok_or_else(|| missing("from"))?;
    let branch = flag_branch
        .map(str::to_string)
        .or(found.branch)
        .ok_or_else(|| missing("branch"))?;
    if !plain_ref(&from) {
        return Err(bad_ref("from", &from));
    }
    if !plain_ref(&branch)
        || std::path::Path::new(&branch)
            .extension()
            .is_some_and(|e| e == "lock")
    {
        return Err(bad_ref("branch", &branch));
    }
    Ok(Refs { from, branch })
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
        "model \"{model}\" is not in [pool.harness].models ({list})"
    )))
}

/// Everything about a job that is known before a slot is chosen.
#[derive(Debug, Clone)]
pub struct JobPlan {
    pub job: String,
    pub task: String,
    pub subject: String,
    pub brief: String,
    pub rules: String,
    pub refs: Refs,
    pub model: String,
    pub budget: u64,
    pub depth: u32,
    pub harness_cmd: String,
    pub env_names: Vec<String>,
    pub committer: PoolCommitter,
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
            .filter(|f| matches!(f.name, "brief.md" | "rules.md" | "harness.cmd"))
            .collect()
    }
}

/// Single-quote a value for a shell-sourced file.
#[must_use]
pub fn sh_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// Lay out the files of a job for `slot`. `token` goes into the `token` file
/// and nowhere else.
#[must_use]
pub fn build_bundle(spec: &JobPlan, slot: &str, token: &Secret) -> Bundle {
    let params = [
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
    ]
    .map(|(k, v)| format!("{k}={}\n", sh_quote(&v)))
    .concat();
    let file = |name, content: &str, mode| BundleFile {
        name,
        content: content.as_bytes().to_vec(),
        mode,
    };
    Bundle {
        files: vec![
            file("run.sh", RUNNER, 0o700),
            file("params.env", &params, 0o600),
            file("token", token.expose(), 0o600),
            file("id", &spec.job, 0o600),
            file(
                "limit",
                &(spec.budget + RUNNER_SLACK_SECS).to_string(),
                0o600,
            ),
            file("brief.md", &spec.brief, 0o644),
            file("rules.md", &spec.rules, 0o644),
            file("harness.cmd", &spec.harness_cmd, 0o644),
        ],
    }
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or_default().trim().to_string()
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
