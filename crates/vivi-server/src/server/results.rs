//! Result-run tokens and `POST /v1/results/<run token>`.
//!
//! A dispatched job is given one job token. `POST /v1/results/open/<job
//! token>` opens one run per suite and answers a one-run token. Rows are then
//! posted to `POST /v1/results/<run token>` as JSON lines; an empty body or
//! `{"close":true}` closes that run. Token records sit beside the job records
//! in `<data_dir>/results/`; the rows themselves live in the project's own
//! `results.db`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Json;
use axum::extract::{Path as AxPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use vivi_core::results::{ResultRow, ResultsStore, Run, validate_row};

use super::AppState;

const JOB_PREFIX: &str = "vivirj_";
const RUN_PREFIX: &str = "vivir_";
const TOKEN_HEX: usize = 64;
const MAX_SUITE_CHARS: usize = 128;
const MAX_FIELD_CHARS: usize = 512;

/// Serializes every read-modify-write of a token file.
static LOCK: Mutex<()> = Mutex::new(());

/// The metadata a dispatched job opens its runs with.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunJob {
    pub project: String,
    pub role: String,
    pub target: String,
    pub commit: String,
    pub host: String,
    #[serde(default)]
    pub job: Option<String>,
    #[serde(default)]
    pub task: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JobToken {
    root: PathBuf,
    metadata: RunJob,
    created_at: u64,
    runs: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RunToken {
    root: PathBuf,
    job_token: String,
    run_id: String,
    closed: bool,
}

/// Why a result submission was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    NotFound(String),
    Closed(String),
    Invalid(String),
    Storage(String),
}

impl Refusal {
    fn into_response(self) -> Response {
        let (status, reason) = match self {
            Refusal::NotFound(reason) => (StatusCode::NOT_FOUND, reason),
            Refusal::Closed(reason) => (StatusCode::CONFLICT, reason),
            Refusal::Invalid(reason) => (StatusCode::UNPROCESSABLE_ENTITY, reason),
            Refusal::Storage(reason) => (StatusCode::INTERNAL_SERVER_ERROR, reason),
        };
        (status, format!("rejected: {reason}\n")).into_response()
    }
}

fn results_dir(state: &AppState) -> PathBuf {
    state.data_dir.join("results")
}

fn token_path(state: &AppState, token: &str) -> PathBuf {
    results_dir(state).join(format!("{token}.json"))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

fn random_hex(bytes: usize) -> Result<String, String> {
    let mut raw = vec![0u8; bytes];
    getrandom::fill(&mut raw).map_err(|_| "no OS entropy".to_owned())?;
    Ok(hex::encode(raw))
}

fn valid_token(token: &str, prefix: &str) -> bool {
    token.strip_prefix(prefix).is_some_and(|hex| {
        hex.len() == TOKEN_HEX && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

fn read_record<T: DeserializeOwned>(path: &Path) -> Option<T> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(_) => return None,
    };
    match serde_json::from_slice(&bytes) {
        Ok(value) => Some(value),
        Err(_) => None,
    }
}

fn write_record<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let dir = path.parent().ok_or("the token path has no parent")?;
    fs::create_dir_all(dir).map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| format!("cannot encode {}: {error}", path.display()))?;
    let temp = path.with_extension("json.tmp");
    fs::write(&temp, bytes).map_err(|error| format!("cannot write {}: {error}", temp.display()))?;
    fs::rename(&temp, path).map_err(|error| format!("cannot write {}: {error}", path.display()))
}

fn field(value: &str, name: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("{name} is empty"));
    }
    if value.chars().count() > MAX_FIELD_CHARS {
        return Err(format!(
            "{name} is longer than {MAX_FIELD_CHARS} characters"
        ));
    }
    Ok(())
}

fn validate_job(job: &RunJob) -> Result<(), String> {
    field(&job.project, "project")?;
    field(&job.role, "role")?;
    field(&job.target, "target")?;
    field(&job.host, "host")?;
    let full_sha = (40..=64).contains(&job.commit.len())
        && job.commit.bytes().all(|byte| byte.is_ascii_hexdigit());
    if !full_sha {
        return Err(String::from("commit must be a full SHA"));
    }
    Ok(())
}

fn validate_suite(suite: &str) -> Result<(), String> {
    let suite = suite.trim();
    if suite.is_empty() {
        return Err(String::from("suite is empty"));
    }
    if suite.chars().count() > MAX_SUITE_CHARS {
        return Err(format!("suite is longer than {MAX_SUITE_CHARS} characters"));
    }
    if suite.chars().any(char::is_control) {
        return Err(String::from("suite contains a control character"));
    }
    Ok(())
}

/// Open a job token for `root` and `metadata`.
///
/// # Errors
/// Returns the reason the metadata is unusable or the token cannot be stored.
pub fn open_job(state: &AppState, root: &Path, metadata: &RunJob) -> Result<String, String> {
    validate_job(metadata)?;
    let token = format!("{JOB_PREFIX}{}", random_hex(32)?);
    let record = JobToken {
        root: root.to_path_buf(),
        metadata: metadata.clone(),
        created_at: now(),
        runs: BTreeMap::new(),
    };
    let _one = LOCK.lock().unwrap_or_else(|error| error.into_inner());
    write_record(&token_path(state, &token), &record)?;
    Ok(token)
}

/// Open one run for `suite` under a job token, or return the still-open run
/// already opened for that suite.
///
/// # Errors
/// Returns the refusal when the job token is unknown or the run cannot be
/// opened.
pub fn open_run_for_job(state: &AppState, job_token: &str, suite: &str) -> Result<String, Refusal> {
    if !valid_token(job_token, JOB_PREFIX) {
        return Err(Refusal::NotFound(String::from("unknown job token")));
    }
    validate_suite(suite).map_err(Refusal::Invalid)?;
    let suite = suite.trim();
    let _one = LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let path = token_path(state, job_token);
    let mut job: JobToken =
        read_record(&path).ok_or_else(|| Refusal::NotFound(String::from("unknown job token")))?;
    if let Some(existing) = job.runs.get(suite).cloned() {
        if read_record::<RunToken>(&token_path(state, &existing)).is_some_and(|run| !run.closed) {
            return Ok(existing);
        }
        job.runs.remove(suite);
    }
    let token = format!("{RUN_PREFIX}{}", random_hex(32).map_err(Refusal::Storage)?);
    let run = Run {
        id: token.clone(),
        project: job.metadata.project.clone(),
        role: job.metadata.role.clone(),
        target: job.metadata.target.clone(),
        suite: suite.to_owned(),
        commit: job.metadata.commit.clone(),
        started_at: now(),
        finished_at: None,
        host: job.metadata.host.clone(),
        job: job.metadata.job.clone(),
        task: job.metadata.task.clone(),
    };
    let mut store = ResultsStore::open_project(&job.root)
        .map_err(|error| Refusal::Storage(error.to_string()))?;
    store
        .open_run(&run)
        .map_err(|error| Refusal::Storage(error.to_string()))?;
    let record = RunToken {
        root: job.root.clone(),
        job_token: job_token.to_owned(),
        run_id: token.clone(),
        closed: false,
    };
    write_record(&token_path(state, &token), &record).map_err(Refusal::Storage)?;
    job.runs.insert(suite.to_owned(), token.clone());
    write_record(&path, &job).map_err(Refusal::Storage)?;
    Ok(token)
}

fn close_request(body: &str) -> bool {
    let body = body.trim();
    if body.is_empty() {
        return true;
    }
    matches!(
        serde_json::from_str::<Value>(body),
        Ok(Value::Object(map))
            if map.len() == 1 && map.get("close") == Some(&Value::Bool(true))
    )
}

fn parse_rows(body: &str) -> Result<Vec<ResultRow>, Refusal> {
    let mut rows = Vec::new();
    for (index, line) in body.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let row: ResultRow = serde_json::from_str(line)
            .map_err(|error| Refusal::Invalid(format!("line {}: {error}", index + 1)))?;
        let row = validate_row(row)
            .map_err(|reason| Refusal::Invalid(format!("line {}: {reason}", index + 1)))?;
        rows.push(row);
    }
    if rows.is_empty() {
        return Err(Refusal::Invalid(String::from("no rows")));
    }
    Ok(rows)
}

/// Submit JSON-line rows to a run, or close it. `Ok` is the text the client
/// prints.
///
/// # Errors
/// Returns the refusal naming an unknown or closed token, an invalid row, or
/// a storage failure.
pub fn submit(state: &AppState, run_token: &str, body: &str) -> Result<String, Refusal> {
    if !valid_token(run_token, RUN_PREFIX) {
        return Err(Refusal::NotFound(String::from("unknown run token")));
    }
    let _one = LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let path = token_path(state, run_token);
    let mut record: RunToken =
        read_record(&path).ok_or_else(|| Refusal::NotFound(String::from("unknown run token")))?;
    if record.closed {
        return Err(Refusal::Closed(String::from("the run is closed")));
    }
    let mut store = ResultsStore::open_project(&record.root)
        .map_err(|error| Refusal::Storage(error.to_string()))?;
    if close_request(body) {
        store
            .close_run(&record.run_id, now())
            .map_err(|error| Refusal::Storage(error.to_string()))?;
        record.closed = true;
        write_record(&path, &record).map_err(Refusal::Storage)?;
        return Ok(String::from("accepted: closed\n"));
    }
    let rows = parse_rows(body)?;
    let accepted = store
        .insert_rows(&record.run_id, &rows)
        .map_err(|error| Refusal::Invalid(error.to_string()))?;
    Ok(format!("accepted: {accepted} rows\n"))
}

/// `POST /v1/results/open/{job_token}`: open one run for a suite.
pub async fn open_handler(
    State(state): State<AppState>,
    AxPath(token): AxPath<String>,
    headers: HeaderMap,
) -> Response {
    let suite = headers
        .get("x-vivi-suite")
        .and_then(|value| match value.to_str() {
            Ok(text) => Some(text),
            Err(_) => None,
        })
        .unwrap_or("default")
        .to_owned();
    match tokio::task::spawn_blocking(move || open_run_for_job(&state, &token, &suite)).await {
        Ok(Ok(token)) => Json(json!({ "token": token })).into_response(),
        Ok(Err(refusal)) => refusal.into_response(),
        Err(_) => Refusal::Storage(String::from("the run-open task failed")).into_response(),
    }
}

/// `POST /v1/results/{run_token}`: JSON lines of rows, or a close request.
pub async fn submit_handler(
    State(state): State<AppState>,
    AxPath(token): AxPath<String>,
    body: String,
) -> Response {
    match tokio::task::spawn_blocking(move || submit(&state, &token, &body)).await {
        Ok(Ok(text)) => (StatusCode::OK, text).into_response(),
        Ok(Err(refusal)) => refusal.into_response(),
        Err(_) => Refusal::Storage(String::from("the result-submit task failed")).into_response(),
    }
}

/// The host's name for a run, when the caller does not supply one.
#[must_use]
pub fn local_host() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("HOST"))
        .unwrap_or_else(|_| String::from("unknown"))
}
