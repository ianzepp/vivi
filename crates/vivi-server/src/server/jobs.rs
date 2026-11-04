//! Job tickets: a bounded, per-job credential for a worker machine.
//!
//! A host creates a job with the `job_create` tool and hands the returned
//! `vivijt_` ticket to a worker. The ticket reads the job manifest
//! (`GET /v1/jobs/<id>`), fetches the target's lane, pushes exactly one
//! branch, and posts one receipt (`POST /v1/jobs/<id>/receipt`). The server
//! stores only the ticket's SHA-256 digest, in `<data_dir>/jobs/<id>.json`.

use std::path::PathBuf;
use std::sync::Mutex;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path as AxPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::server::AppState;
use crate::server::auth::{constant_time_eq, rfc3339};
use crate::server::mcp::host::hosted_entry;
use crate::server::mcp::live::target_git::lane_for;
use crate::server::projects_api::presented_bearer;
use crate::server::rooms::error_response;
use vivi_core::mailspace::Mailspace;

pub const TICKET_PREFIX: &str = "vivijt_";
const DEFAULT_TIMEOUT_S: u64 = 1800;
/// Results may be posted late; the ticket outlives the job's own timeout.
const GRACE_S: i64 = 86_400;

/// Serializes every read-modify-write of a job file.
static LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Committer {
    pub name: String,
    pub email: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub job_id: String,
    pub project: String,
    pub project_id: String,
    pub target: String,
    pub git_path: String,
    pub base: String,
    pub branch: String,
    pub task: String,
    pub harness: String,
    pub gate: Option<String>,
    pub timeout_s: u64,
    pub committer: Committer,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRecord {
    pub manifest: Manifest,
    pub ticket_digest: String,
    pub created_at: String,
    pub expires_at: String,
    pub receipt: Option<Value>,
}

impl JobRecord {
    fn expired(&self) -> bool {
        DateTime::parse_from_rfc3339(&self.expires_at)
            .map_or(true, |at| at.with_timezone(&Utc) < Utc::now())
    }

    /// `done` once a receipt is in, else `expired` past the deadline, else `open`.
    pub fn status(&self) -> &'static str {
        if self.receipt.is_some() {
            "done"
        } else if self.expired() {
            "expired"
        } else {
            "open"
        }
    }

    /// Whether the ticket may still reach the git route.
    pub fn live(&self) -> bool {
        self.status() == "open"
    }

    /// Whether `name` (a hosted name or project id) is this job's project.
    pub fn is_project(&self, name: &str) -> bool {
        self.manifest.project == name || self.manifest.project_id == name
    }

    fn view(&self) -> Value {
        json!({
            "job_id": self.manifest.job_id,
            "manifest": self.manifest,
            "status": self.status(),
            "created_at": self.created_at,
            "expires_at": self.expires_at,
            "receipt": self.receipt,
        })
    }

    fn summary(&self) -> Value {
        json!({
            "job_id": self.manifest.job_id,
            "project": self.manifest.project,
            "target": self.manifest.target,
            "branch": self.manifest.branch,
            "status": self.status(),
            "created_at": self.created_at,
            "expires_at": self.expires_at,
            "ok": self.receipt.as_ref().and_then(|r| r.get("ok")).cloned(),
        })
    }
}

fn jobs_dir(state: &AppState) -> PathBuf {
    state.data_dir.join("jobs")
}

fn valid_job_id(id: &str) -> bool {
    id.strip_prefix("job_")
        .is_some_and(|hex| hex.len() == 16 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn digest(ticket: &str) -> String {
    hex::encode(Sha256::digest(ticket.as_bytes()))
}

fn random_hex(bytes: usize) -> Result<String, String> {
    let mut raw = vec![0u8; bytes];
    getrandom::fill(&mut raw).map_err(|_| "no OS entropy".to_owned())?;
    Ok(hex::encode(raw))
}

fn load(state: &AppState, job_id: &str) -> Option<JobRecord> {
    if !valid_job_id(job_id) {
        return None;
    }
    read_record(&jobs_dir(state).join(format!("{job_id}.json")))
}

/// One job file. A missing file is no job; an unreadable or corrupt one is
/// also no job, but is logged so a damaged record is not silent.
fn read_record(path: &std::path::Path) -> Option<JobRecord> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            tracing::warn!("cannot read job file {}: {e}", path.display());
            return None;
        }
    };
    match serde_json::from_slice(&bytes) {
        Ok(record) => Some(record),
        Err(e) => {
            tracing::warn!("job file {} is not a job record: {e}", path.display());
            None
        }
    }
}

/// Atomic write, mode 600.
fn store(state: &AppState, record: &JobRecord) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let dir = jobs_dir(state);
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("cannot restrict {}: {e}", dir.display()))?;
    let id = &record.manifest.job_id;
    let tmp = dir.join(format!(".{id}.tmp"));
    let bytes = serde_json::to_vec_pretty(record).map_err(|e| e.to_string())?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, dir.join(format!("{id}.json")))
        .map_err(|e| format!("cannot write the job file: {e}"))
}

fn all(state: &AppState) -> Vec<JobRecord> {
    let Ok(entries) = std::fs::read_dir(jobs_dir(state)) else {
        return Vec::new();
    };
    let mut jobs: Vec<JobRecord> = entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| read_record(&e.path()))
        .collect();
    jobs.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    jobs
}

/// The job a `vivijt_` bearer belongs to. With `job_id`, only that job.
pub fn authenticate_ticket(
    state: &AppState,
    bearer: &str,
    job_id: Option<&str>,
) -> Option<JobRecord> {
    if !bearer.starts_with(TICKET_PREFIX) {
        return None;
    }
    let want = digest(bearer);
    let matches = |r: &JobRecord| constant_time_eq(&r.ticket_digest, &want);
    match job_id {
        Some(id) => load(state, id).filter(matches),
        None => all(state).into_iter().find(matches),
    }
}

// ------------------------------------------------------------ host tools

fn short(branch: &str) -> &str {
    branch.strip_prefix("refs/heads/").unwrap_or(branch)
}

fn valid_branch(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !name.starts_with('/')
        && !name.ends_with('/')
        && !name.ends_with(".lock")
        && !name.contains("..")
        && !name.contains("//")
        && !name.starts_with("refs/")
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'/' | b'-'))
}

fn text<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.as_str())),
        Some(_) => Err(format!("{key} must be a string")),
    }
}

fn required<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    text(args, key)?
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("{key} is required"))
}

/// The project a job is for: hosted here, not archived. With its mailspace root.
fn live_project(
    state: &AppState,
    project_arg: &str,
) -> Result<(crate::server::projects::ProjectMeta, std::path::PathBuf), String> {
    let meta = match hosted_entry(state, project_arg) {
        Some(meta) => meta,
        None => state
            .projects
            .load_meta_including_retired(project_arg)
            .map_err(|_| format!("no such project: {project_arg}"))?,
    };
    if meta.is_archived() || state.projects.is_retired(&meta.project_id) {
        return Err(format!("project {project_arg} is archived"));
    }
    let root = meta
        .mailspace_root
        .clone()
        .ok_or("the project is not hosted here")?;
    Ok((meta, root))
}

/// A job pushes a work branch, never a protected one.
fn check_work_branch(branch: &str, main: &str) -> Result<(), String> {
    if !valid_branch(branch) {
        return Err(format!("branch {branch:?} is not a usable branch name"));
    }
    if branch == "main" || branch == "vivi/main" || branch == main {
        return Err(format!(
            "branch {branch} is a protected branch; name a work branch"
        ));
    }
    Ok(())
}

/// The base to branch from. Default: the target's main; until the pump has
/// created it (a fresh lane holds only the upstream branch), the lane's
/// default branch.
fn job_base(args: &Value, lane: &std::path::Path, main: &str) -> Result<String, String> {
    Ok(match text(args, "base")?.filter(|b| !b.trim().is_empty()) {
        Some(base) => base.to_owned(),
        None if lane_has_branch(lane, main) => main.to_owned(),
        None => lane_default_branch(lane).unwrap_or_else(|| main.to_owned()),
    })
}

/// The committer: the one passed, else the project's `[pump]` committer.
fn job_committer(args: &Value, root: &std::path::Path) -> Result<Committer, String> {
    if let Some(c) = args.get("committer").filter(|c| !c.is_null()) {
        return Ok(Committer {
            name: required(c, "name")
                .map_err(|e| format!("committer.{e}"))?
                .to_owned(),
            email: required(c, "email")
                .map_err(|e| format!("committer.{e}"))?
                .to_owned(),
        });
    }
    let pump = Mailspace::open_root(root.to_path_buf())
        .map_err(|e| format!("cannot open the project mailspace: {e:?}"))?
        .config
        .pump
        .ok_or("the project has no [pump] table; pass committer")?;
    if pump.committer_name.trim().is_empty() || pump.committer_email.trim().is_empty() {
        return Err("the project's [pump] has no committer; pass committer".to_owned());
    }
    Ok(Committer {
        name: pump.committer_name,
        email: pump.committer_email,
    })
}

fn timeout_arg(args: &Value) -> Result<u64, String> {
    match args.get("timeout_s") {
        None | Some(Value::Null) => Ok(DEFAULT_TIMEOUT_S),
        Some(v) => v
            .as_u64()
            .filter(|t| *t > 0)
            .ok_or_else(|| "timeout_s must be a positive integer".to_owned()),
    }
}

/// `job_create`. Blocking.
pub fn create(state: &AppState, args: &Value) -> Result<Value, String> {
    let project_arg = required(args, "project")?;
    let target = required(args, "target")?;
    let branch = required(args, "branch")?;
    let task = required(args, "task")?;
    let harness = required(args, "harness")?;
    let gate = text(args, "gate")?.filter(|g| !g.trim().is_empty());
    let timeout_s = timeout_arg(args)?;

    let (meta, root) = live_project(state, project_arg)?;
    let project =
        crate::server::mcp::server::hosted_name(&meta).unwrap_or_else(|| meta.project_id.clone());
    let (lane, main) = lane_for(&root, target)?;
    let main = short(&main).to_owned();

    check_work_branch(branch, &main)?;
    let base = job_base(args, &lane, &main)?;
    let committer = job_committer(args, &root)?;

    let job_id = format!("job_{}", random_hex(8)?);
    let ticket = format!("{TICKET_PREFIX}{}", random_hex(32)?);
    let now = Utc::now();
    let expires =
        now + Duration::seconds(i64::try_from(timeout_s.min(1 << 40)).unwrap_or(0) + GRACE_S);
    let manifest = Manifest {
        job_id: job_id.clone(),
        git_path: format!("/git/{project}/{target}.git"),
        project,
        project_id: meta.project_id.clone(),
        target: target.to_owned(),
        base,
        branch: branch.to_owned(),
        task: task.to_owned(),
        harness: harness.to_owned(),
        gate: gate.map(str::to_owned),
        timeout_s,
        committer,
    };
    let record = JobRecord {
        ticket_digest: digest(&ticket),
        created_at: rfc3339(now),
        expires_at: rfc3339(expires),
        receipt: None,
        manifest,
    };
    {
        let _one = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        store(state, &record)?;
    }
    Ok(json!({
        "job_id": job_id,
        "ticket": ticket,
        "expires_at": record.expires_at,
        "git_path": record.manifest.git_path,
        "manifest_path": format!("/v1/jobs/{job_id}"),
    }))
}

/// `job_show`. Blocking.
pub fn show(state: &AppState, args: &Value) -> Result<Value, String> {
    let id = required(args, "job_id")?;
    load(state, id)
        .map(|r| r.view())
        .ok_or_else(|| format!("no such job: {id}"))
}

/// `job_list`. Blocking.
pub fn list(state: &AppState, args: &Value) -> Result<Value, String> {
    let filter = text(args, "status")?.unwrap_or("all");
    if !matches!(filter, "open" | "done" | "all") {
        return Err("status must be open, done or all".to_owned());
    }
    let jobs: Vec<Value> = all(state)
        .iter()
        .filter(|r| filter == "all" || r.status() == filter)
        .map(JobRecord::summary)
        .collect();
    Ok(json!({ "jobs": jobs }))
}

// ------------------------------------------------------------ worker routes

fn ticket_job(state: &AppState, headers: &HeaderMap, id: &str) -> Result<JobRecord, Response> {
    let deny = || {
        error_response(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "a valid, unexpired job ticket is required",
        )
    };
    let bearer = presented_bearer(headers).ok_or_else(deny)?;
    let record = authenticate_ticket(state, bearer.trim(), Some(id)).ok_or_else(deny)?;
    if record.expired() {
        return Err(deny());
    }
    Ok(record)
}

/// `GET /v1/jobs/{id}`: the manifest.
pub async fn manifest_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    headers: HeaderMap,
) -> Response {
    match ticket_job(&state, &headers, &id) {
        Ok(record) => Json(record.manifest).into_response(),
        Err(response) => response,
    }
}

/// `POST /v1/jobs/{id}/receipt`: one receipt per job.
pub async fn receipt_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let mut record = match ticket_job(&state, &headers, &id) {
        Ok(record) => record,
        Err(response) => return response,
    };
    let receipt: Value = match serde_json::from_slice(&body) {
        Ok(v @ Value::Object(_)) if v.get("ok").is_some_and(Value::is_boolean) => v,
        _ => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_receipt",
                "the body must be a JSON object with a boolean \"ok\"",
            );
        }
    };
    let _one = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Re-read under the lock: a concurrent receipt may have won.
    if let Some(fresh) = load(&state, &id) {
        record = fresh;
    }
    if record.receipt.is_some() {
        return error_response(StatusCode::CONFLICT, "conflict", "a receipt is already in");
    }
    record.receipt = Some(receipt);
    if let Err(message) = store(&state, &record) {
        tracing::error!("job {id}: {message}");
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "storage failure",
        );
    }
    tracing::info!("job {id}: receipt accepted");
    Json(json!({ "accepted": true })).into_response()
}

fn lane_has_branch(lane: &std::path::Path, branch: &str) -> bool {
    std::process::Command::new("git")
        .arg("--git-dir")
        .arg(lane)
        .args(["rev-parse", "--verify", "--quiet"])
        .arg(format!("refs/heads/{branch}"))
        .output()
        .is_ok_and(|out| out.status.success())
}

fn lane_default_branch(lane: &std::path::Path) -> Option<String> {
    let out = match std::process::Command::new("git")
        .arg("--git-dir")
        .arg(lane)
        .args(["symbolic-ref", "--short", "HEAD"])
        .output()
    {
        Ok(out) => out,
        Err(e) => {
            tracing::warn!("cannot run git in {}: {e}", lane.display());
            return None;
        }
    };
    let name = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (out.status.success() && !name.is_empty() && lane_has_branch(lane, &name)).then_some(name)
}
