//! `vivi pool run <server-url> --job <job_id> [--detach]`: the worker half of
//! the job-ticket contract. One job in, one pushed branch and one receipt out.
//!
//! The ticket is a bearer token for exactly one job. It comes in on stdin (or
//! from the job directory's `ticket` file once detached), never through argv,
//! and reaches only git's own subprocesses, as `GIT_CONFIG_*` environment
//! variables. The harness and gate never see it.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

use vivi::cli::PoolRunCommand;
use vivi::sync::http::send_raw;

const EXIT_FAILED: i32 = 1;
const EXIT_USAGE: i32 = 2;
const EXIT_BUSY: i32 = 75;
const DEFAULT_SLOTS: u32 = 2;
const RETRY_FOR: Duration = Duration::from_secs(3600);
const RETRY_CAP: Duration = Duration::from_secs(120);
const MIN_PUSH_TIME: Duration = Duration::from_secs(120);

/// Run the command; the return value is the process exit code.
pub(crate) fn run(command: &PoolRunCommand) -> i32 {
    match run_inner(command) {
        Ok(code) => code,
        Err(Failure { code, message }) => {
            eprintln!("error: {message}");
            code
        }
    }
}

struct Failure {
    code: i32,
    message: String,
}

fn usage(message: impl Into<String>) -> Failure {
    Failure {
        code: EXIT_USAGE,
        message: message.into(),
    }
}

fn run_inner(command: &PoolRunCommand) -> Result<i32, Failure> {
    let job_id = command.job.as_str();
    if !job_id.starts_with("job_")
        || !job_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return Err(usage(format!("{job_id:?} is not a job id (job_<hex>)")));
    }
    let server = command.server_url.trim_end_matches('/').to_string();
    if !server.starts_with("http://") && !server.starts_with("https://") {
        return Err(usage(format!("{server:?} is not an http(s) server URL")));
    }
    let worker = vivi::home::vivi_home().map_err(usage)?.join("worker");
    let job_dir = worker.join("jobs").join(job_id);
    let slots = read_slots(&worker)?;

    if command.detach {
        return detach(command, &worker, &job_dir, slots);
    }
    let Some(_slot) = take_slot(&worker, slots)? else {
        eprintln!("busy: all {slots} worker slots are in use");
        return Ok(EXIT_BUSY);
    };
    make_private_dir(&job_dir)?;
    let ticket = read_ticket(&job_dir)?;
    run_job(&server, job_id, &ticket, &job_dir)
}

// ---- config, slots, ticket ------------------------------------------------

fn read_slots(worker: &Path) -> Result<u32, Failure> {
    let path = worker.join("config.toml");
    let Ok(text) = fs::read_to_string(&path) else {
        return Ok(DEFAULT_SLOTS);
    };
    let table: toml::Table = text
        .parse()
        .map_err(|e| usage(format!("{}: {e}", path.display())))?;
    match table.get("slots") {
        None => Ok(DEFAULT_SLOTS),
        Some(value) => match value.as_integer().map(u32::try_from) {
            Some(Ok(n)) if n > 0 => Ok(n),
            _ => Err(usage(format!(
                "{}: slots must be a positive integer",
                path.display()
            ))),
        },
    }
}

/// Take the first free slot lock; the returned file holds it until dropped.
fn take_slot(worker: &Path, slots: u32) -> Result<Option<File>, Failure> {
    let dir = worker.join("slots");
    fs::create_dir_all(&dir).map_err(|e| usage(format!("{}: {e}", dir.display())))?;
    for n in 0..slots {
        let path = dir.join(format!("{n}.lock"));
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|e| usage(format!("{}: {e}", path.display())))?;
        if file.try_lock().is_ok() {
            return Ok(Some(file));
        }
    }
    Ok(None)
}

fn make_private_dir(dir: &Path) -> Result<(), Failure> {
    let io = |e: std::io::Error| usage(format!("{}: {e}", dir.display()));
    fs::create_dir_all(dir).map_err(io)?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(io)
}

fn read_stdin_ticket() -> Result<String, Failure> {
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| usage(format!("cannot read the ticket from stdin: {e}")))?;
    let ticket = line.trim().to_string();
    if ticket.is_empty() {
        return Err(usage("no ticket: pass it as one line on stdin"));
    }
    Ok(ticket)
}

fn read_ticket(job_dir: &Path) -> Result<String, Failure> {
    match fs::read_to_string(job_dir.join("ticket")) {
        Ok(text) if !text.trim().is_empty() => Ok(text.trim().to_string()),
        _ => read_stdin_ticket(),
    }
}

// ---- detach ---------------------------------------------------------------

fn detach(
    command: &PoolRunCommand,
    worker: &Path,
    job_dir: &Path,
    slots: u32,
) -> Result<i32, Failure> {
    // A cheap early answer: do not store a ticket for a job that cannot start.
    // The detached process takes its own lock; this one is released at once.
    let probe = take_slot(worker, slots)?;
    if probe.is_none() {
        eprintln!("busy: all {slots} worker slots are in use");
        return Ok(EXIT_BUSY);
    }
    drop(probe);
    let ticket = read_stdin_ticket()?;
    make_private_dir(job_dir)?;
    let ticket_path = job_dir.join("ticket");
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&ticket_path)
        .map_err(|e| usage(format!("{}: {e}", ticket_path.display())))?;
    file.write_all(ticket.as_bytes())
        .map_err(|e| usage(format!("{}: {e}", ticket_path.display())))?;
    drop(file);

    let log = append_log(&job_dir.join("log.txt"))
        .map_err(|e| usage(format!("cannot open the job log: {e}")))?;
    let log_err = log
        .try_clone()
        .map_err(|e| usage(format!("cannot open the job log: {e}")))?;
    let exe = std::env::current_exe().map_err(|e| usage(format!("cannot find vivi: {e}")))?;
    let args: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| a != "--detach")
        .collect();
    let mut child = Command::new(exe);
    child
        .args(&args)
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(log_err);
    // SAFETY: setsid is async-signal-safe and touches no memory of ours.
    unsafe {
        child.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = child
        .spawn()
        .map_err(|e| usage(format!("cannot start the detached job: {e}")))?;
    println!("started {} pid {}", command.job, child.id());
    Ok(0)
}

fn append_log(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

// ---- the job --------------------------------------------------------------

struct Manifest {
    branch: String,
    base: String,
    git_url: String,
    task: String,
    harness: String,
    gate: Option<String>,
    timeout: Duration,
    committer_name: String,
    committer_email: String,
}

fn bearer(ticket: &str) -> Vec<(&'static str, String)> {
    vec![("Authorization", format!("Bearer {ticket}"))]
}

fn fetch_manifest(server: &str, job_id: &str, ticket: &str) -> Result<Manifest, Failure> {
    let url = format!("{server}/v1/jobs/{job_id}");
    let response = send_raw("GET", &url, &bearer(ticket), None)
        .map_err(|e| usage(format!("cannot fetch the manifest: {e}")))?;
    if response.status != 200 {
        return Err(usage(format!(
            "manifest request refused: HTTP {} {}",
            response.status,
            String::from_utf8_lossy(&response.body)
                .chars()
                .take(200)
                .collect::<String>()
        )));
    }
    let value: Value = serde_json::from_slice(&response.body)
        .map_err(|e| usage(format!("manifest is not JSON: {e}")))?;
    let text = |key: &str| -> Result<String, Failure> {
        value
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| usage(format!("manifest has no {key}")))
    };
    if text("job_id")? != job_id {
        return Err(usage("manifest is for a different job"));
    }
    let committer = value.get("committer");
    let person = |key: &str| -> Result<String, Failure> {
        committer
            .and_then(|c| c.get(key))
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| usage(format!("manifest has no committer {key}")))
    };
    let gate = value
        .get("gate")
        .and_then(Value::as_str)
        .filter(|g| !g.trim().is_empty())
        .map(str::to_string);
    let timeout = value
        .get("timeout_s")
        .and_then(Value::as_u64)
        .unwrap_or(1800);
    Ok(Manifest {
        branch: text("branch")?,
        base: text("base")?,
        git_url: format!("{server}{}", text("git_path")?),
        task: text("task")?,
        harness: text("harness")?,
        gate,
        timeout: Duration::from_secs(timeout),
        committer_name: person("name")?,
        committer_email: person("email")?,
    })
}

struct Outcome {
    failed_step: String,
    code: i32,
    tail: String,
}

struct Job<'a> {
    manifest: &'a Manifest,
    job_id: &'a str,
    job_dir: &'a Path,
    checkout: PathBuf,
    log: PathBuf,
    started: Instant,
    steps: Map<String, Value>,
    git_env: Vec<(String, String)>,
}

impl Job<'_> {
    fn remaining(&self) -> Duration {
        self.manifest.timeout.saturating_sub(self.started.elapsed())
    }

    /// Run one step to its end or its time limit, recording it. Output goes to
    /// the job log; the returned tail is only this step's last lines.
    fn step(
        &mut self,
        name: &str,
        mut command: Command,
        describe: &str,
        limit: Duration,
    ) -> Result<(), Outcome> {
        let begin = Instant::now();
        let (code, tail) = self.exec(&mut command, describe, limit);
        self.steps.insert(
            name.to_string(),
            json!({"code": code, "seconds": round1(begin.elapsed())}),
        );
        if code == 0 {
            Ok(())
        } else {
            Err(Outcome {
                failed_step: name.to_string(),
                code,
                tail,
            })
        }
    }

    fn exec(&self, command: &mut Command, describe: &str, limit: Duration) -> (i32, String) {
        let Ok(mut log) = append_log(&self.log) else {
            return (125, "cannot open the job log".into());
        };
        let start = log.seek(SeekFrom::End(0)).unwrap_or(0);
        note(&mut log, &format!("\n$ {describe}"));
        let (Ok(out), Ok(err)) = (log.try_clone(), log.try_clone()) else {
            return (125, "cannot open the job log".into());
        };
        command
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err)
            .process_group(0);
        let code = match command.spawn() {
            Err(e) => {
                note(&mut log, &format!("[cannot start: {e}]"));
                127
            }
            Ok(mut child) => wait_limited(&mut child, limit, &mut log),
        };
        (code, tail_since(&self.log, start))
    }

    fn git(&self) -> Command {
        let mut c = Command::new("git");
        c.current_dir(&self.checkout);
        c
    }

    fn git_net(&self, cwd: &Path) -> Command {
        let mut c = Command::new("git");
        c.current_dir(cwd).envs(self.git_env.iter().cloned());
        c
    }

    fn sh(&self, script: &str, task_file: &Path) -> Command {
        let mut c = Command::new("sh");
        c.arg("-c")
            .arg(script)
            .current_dir(&self.checkout)
            .env("WORK_TASK_FILE", task_file)
            .env("WORK_JOB_ID", self.job_id);
        c
    }
}

/// Append one line to the job log. The log is the worker's record, not its
/// result: a write that fails is reported on stderr and the job goes on.
fn note(log: &mut File, line: &str) {
    if let Err(e) = writeln!(log, "{line}") {
        eprintln!("warning: cannot write the job log: {e}");
    }
}

/// Wait for the child, killing its whole process group at the limit (code 124).
fn wait_limited(child: &mut std::process::Child, limit: Duration, log: &mut File) -> i32 {
    let deadline = Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.code().unwrap_or(128),
            Ok(None) => {}
            Err(_) => return 125,
        }
        if Instant::now() >= deadline {
            note(log, &format!("\n[timeout after {}s]", limit.as_secs()));
            let pid = i32::try_from(child.id()).unwrap_or(0);
            if pid > 0 {
                // SAFETY: signalling the process group of a child we started.
                unsafe { libc::kill(-pid, libc::SIGKILL) };
            }
            if let Err(e) = child.wait() {
                eprintln!("warning: cannot reap the timed-out step: {e}");
            }
            return 124;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn read_since(log: &Path, start: u64) -> std::io::Result<String> {
    let mut f = File::open(log)?;
    f.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    f.read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn tail_since(log: &Path, start: u64) -> String {
    let text = read_since(log, start).unwrap_or_else(|e| {
        eprintln!("warning: cannot read the job log tail: {e}");
        String::new()
    });
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(15)..].join("\n")
}

fn round1(d: Duration) -> f64 {
    (d.as_secs_f64() * 10.0).round() / 10.0
}

fn classify(step: &str, code: i32, tail: &str) -> &'static str {
    let text = tail.to_lowercase();
    if code == 124 {
        return "timeout";
    }
    if ["429", "rate limit", "overloaded", "throttl"]
        .iter()
        .any(|w| text.contains(w))
    {
        return "provider_busy";
    }
    match step {
        "harness" => "agent",
        "gate" => "gate_red",
        "commit" => "no_change",
        _ => "infra",
    }
}

fn hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: the buffer is valid for its length.
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if rc != 0 {
        return "unknown".into();
    }
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

/// The environment of git's network subprocesses: the ticket as an extra
/// header, and nothing from the user's or the system's git config.
fn git_env(ticket: &str) -> Vec<(String, String)> {
    vec![
        ("GIT_CONFIG_COUNT".into(), "1".into()),
        ("GIT_CONFIG_KEY_0".into(), "http.extraHeader".into()),
        (
            "GIT_CONFIG_VALUE_0".into(),
            format!("Authorization: Bearer {ticket}"),
        ),
        ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        ("GIT_TERMINAL_PROMPT".into(), "0".into()),
    ]
}

fn run_job(server: &str, job_id: &str, ticket: &str, job_dir: &Path) -> Result<i32, Failure> {
    let manifest = fetch_manifest(server, job_id, ticket)?;
    let task_file = job_dir.join("task.md");
    fs::write(&task_file, &manifest.task)
        .map_err(|e| usage(format!("{}: {e}", task_file.display())))?;
    let mut job = Job {
        manifest: &manifest,
        job_id,
        job_dir,
        checkout: job_dir.join("checkout"),
        log: job_dir.join("log.txt"),
        started: Instant::now(),
        steps: Map::new(),
        git_env: git_env(ticket),
    };
    // A rerun in the same job dir starts clean.
    if job.checkout.exists() {
        fs::remove_dir_all(&job.checkout)
            .map_err(|e| usage(format!("{}: {e}", job.checkout.display())))?;
    }
    let mut commit: Option<String> = None;
    let result = pipeline(&mut job, &task_file, &mut commit);
    let seconds = round1(job.started.elapsed());
    let mut receipt = json!({
        "ok": result.is_ok(),
        "job_id": job_id,
        "branch": manifest.branch,
        "commit": commit,
        "host": hostname(),
        "seconds": seconds,
        "steps": job.steps,
        "failed_step": Value::Null,
        "class": Value::Null,
        "tail": Value::Null,
    });
    if let Err(outcome) = &result {
        receipt["failed_step"] = json!(outcome.failed_step);
        receipt["class"] = json!(classify(&outcome.failed_step, outcome.code, &outcome.tail));
        receipt["tail"] = json!(outcome.tail);
    }
    let body = serde_json::to_vec_pretty(&receipt).unwrap_or_default();
    if let Err(e) = fs::write(job_dir.join("receipt.json"), &body) {
        eprintln!("warning: cannot keep a local copy of the receipt: {e}");
    }
    println!("{}", String::from_utf8_lossy(&body));
    post_receipt(server, job_id, ticket, &body)?;
    if let Err(e) = fs::remove_file(job_dir.join("ticket")) {
        eprintln!("warning: cannot remove the stored ticket: {e}");
    }
    Ok(if result.is_ok() { 0 } else { EXIT_FAILED })
}

fn pipeline(job: &mut Job, task_file: &Path, commit: &mut Option<String>) -> Result<(), Outcome> {
    clone_step(job)?;
    branch_step(job)?;

    let harness = job.sh(&job.manifest.harness, task_file);
    let limit = job.remaining();
    let describe = format!("sh -c {:?}", job.manifest.harness);
    job.step("harness", harness, &describe, limit)?;

    commit_step(job)?;
    *commit = head_commit(job);

    if let Some(gate) = &job.manifest.gate {
        let cmd = job.sh(gate, task_file);
        let limit = job.remaining();
        job.step("gate", cmd, &format!("sh -c {gate:?}"), limit)?;
    }

    push_with_retry(job)
}

fn clone_step(job: &mut Job) -> Result<(), Outcome> {
    let url = job.manifest.git_url.clone();
    let mut clone = job.git_net(job.job_dir);
    clone.args(["clone", "-q", &url]).arg(&job.checkout);
    let limit = job.remaining();
    job.step("clone", clone, &format!("git clone {url}"), limit)
}

/// The base is a ref (taken from origin) or a commit.
fn resolve_base(job: &Job) -> String {
    let base = &job.manifest.base;
    let remote = format!("origin/{base}");
    let probe = job
        .git()
        .args([
            "rev-parse",
            "--verify",
            "-q",
            &format!("{remote}^{{commit}}"),
        ])
        .output();
    match probe {
        Ok(out) if out.status.success() => remote,
        _ => base.clone(),
    }
}

fn branch_step(job: &mut Job) -> Result<(), Outcome> {
    let base = resolve_base(job);
    let branch_name = job.manifest.branch.clone();
    let mut branch = job.git();
    branch.args(["switch", "-q", "-c", &branch_name, &base]);
    let limit = job.remaining();
    job.step(
        "branch",
        branch,
        &format!("git switch -c {branch_name} {base}"),
        limit,
    )
}

/// A failure of the commit stage before any git step is recorded.
fn stage_failure(step: &str, code: i32, tail: impl Into<String>) -> Outcome {
    Outcome {
        failed_step: step.into(),
        code,
        tail: tail.into(),
    }
}

/// Stage whatever the harness changed.
fn stage_all(job: &Job) -> Result<(), Outcome> {
    match job.git().args(["add", "-A"]).output() {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            Err(stage_failure(
                "stage",
                out.status.code().unwrap_or(1),
                format!("git add -A failed: {}", stderr.trim()),
            ))
        }
        Err(e) => Err(stage_failure(
            "stage",
            127,
            format!("cannot run git add: {e}"),
        )),
    }
}

/// Commit whatever the harness changed.
fn commit_step(job: &mut Job) -> Result<(), Outcome> {
    stage_all(job)?;
    let staged = job.git().args(["diff", "--cached", "--quiet"]).status();
    if staged.is_ok_and(|s| s.success()) {
        job.steps
            .insert("commit".into(), json!({"code": 1, "seconds": 0.0}));
        return Err(stage_failure("commit", 1, "the harness made no changes"));
    }
    let m = job.manifest;
    let subject = format!(
        "Work {}: {}",
        job.job_id,
        m.task
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(60)
            .collect::<String>()
    );
    let mut commit_cmd = job.git();
    commit_cmd
        .arg("-c")
        .arg(format!("user.name={}", m.committer_name))
        .arg("-c")
        .arg(format!("user.email={}", m.committer_email))
        .args(["commit", "-q", "-m", subject.trim_end()]);
    let limit = job.remaining();
    job.step("commit", commit_cmd, "git commit", limit)
}

fn head_commit(job: &Job) -> Option<String> {
    match job.git().args(["rev-parse", "HEAD"]).output() {
        Ok(out) => Some(String::from_utf8_lossy(&out.stdout).trim().to_string()),
        Err(e) => {
            eprintln!("warning: cannot read the committed HEAD: {e}");
            None
        }
    }
}

/// Push the branch; a transient failure is retried with backoff for up to an
/// hour, a refusal is not.
fn push_with_retry(job: &mut Job) -> Result<(), Outcome> {
    let m = job.manifest;
    let begin = Instant::now();
    let mut wait = Duration::from_secs(2);
    loop {
        let mut push = job.git_net(&job.checkout.clone());
        push.args([
            "push",
            "-q",
            "origin",
            &format!("HEAD:refs/heads/{}", m.branch),
        ]);
        let limit = job.remaining().max(MIN_PUSH_TIME);
        let result = job.step(
            "push",
            push,
            &format!("git push origin HEAD:refs/heads/{}", m.branch),
            limit,
        );
        let Err(outcome) = result else { return Ok(()) };
        let text = outcome.tail.to_lowercase();
        let refused = [
            "denied",
            "rejected",
            "refused",
            "403",
            "401",
            "not found",
            "protected",
        ]
        .iter()
        .any(|w| text.contains(w));
        if refused || begin.elapsed() + wait > RETRY_FOR {
            return Err(outcome);
        }
        std::thread::sleep(wait);
        wait = (wait * 2).min(RETRY_CAP);
    }
}

fn post_receipt(server: &str, job_id: &str, ticket: &str, body: &[u8]) -> Result<(), Failure> {
    let url = format!("{server}/v1/jobs/{job_id}/receipt");
    let mut headers = bearer(ticket);
    headers.push(("Content-Type", "application/json".into()));
    let begin = Instant::now();
    let mut wait = Duration::from_secs(2);
    loop {
        let problem = match send_raw("POST", &url, &headers, Some(body.to_vec())) {
            Ok(r) if r.status == 200 => return Ok(()),
            // Already accepted: the server has a receipt for this job.
            Ok(r) if r.status == 409 => return Ok(()),
            Ok(r) if matches!(r.status, 400 | 401 | 403 | 404) => {
                return Err(usage(format!(
                    "receipt refused: HTTP {}; kept {}",
                    r.status, "the job directory"
                )));
            }
            Ok(r) => format!("HTTP {}", r.status),
            Err(e) => e.to_string(),
        };
        if begin.elapsed() + wait > RETRY_FOR {
            return Err(Failure {
                code: EXIT_FAILED,
                message: format!(
                    "receipt not posted after an hour ({problem}); job directory kept"
                ),
            });
        }
        eprintln!(
            "receipt post failed ({problem}); retrying in {}s",
            wait.as_secs()
        );
        std::thread::sleep(wait);
        wait = (wait * 2).min(RETRY_CAP);
    }
}
