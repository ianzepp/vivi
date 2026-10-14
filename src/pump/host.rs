//! The command-line seam: the Host trait, its real and fake implementations.

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::pump::config::PumpConfig;

/// The outcome of running one command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmdOutput {
    /// The exit code; 124 marks a timeout.
    pub code: i32,
    /// Stdout then stderr, decoded as lossy UTF-8.
    pub text: String,
}

/// The command-line seam every pump module goes through.
pub trait Host: Send + Sync {
    /// Current wall-clock time in epoch seconds.
    fn now(&self) -> u64;
    /// Run `cmd` as `bash -c "<cmd>"` (the caller already put `shell_prefix`
    /// in front), with an optional working directory and a timeout in seconds.
    ///
    /// On timeout the whole process group is killed, the code becomes 124 and
    /// the text is the output so far plus `"\n[timeout after Ns]"`.
    fn run(&self, cmd: &str, cwd: Option<&Path>, timeout_secs: u64) -> CmdOutput;
    /// Start `cmd` detached in its own process group, stdin from `stdin` when
    /// given, stdout and stderr appended to `out`. Never waits.
    fn spawn(&self, cmd: &str, cwd: Option<&Path>, stdin: Option<&Path>, out: &Path);
    /// Append one log line.
    fn log(&self, line: &str);
}

/// Real host: commands run through `bash`, logs land in `<home>/log.txt`.
pub struct RealHost {
    log_path: PathBuf,
}

impl RealHost {
    /// A host that logs to `<home>/log.txt`.
    #[must_use]
    pub fn new(home: &Path) -> Self {
        RealHost {
            log_path: home.join("log.txt"),
        }
    }
}

impl Host for RealHost {
    fn now(&self) -> u64 {
        epoch_now()
    }

    fn run(&self, cmd: &str, cwd: Option<&Path>, timeout_secs: u64) -> CmdOutput {
        run_command(cmd, cwd, timeout_secs)
    }

    fn spawn(&self, cmd: &str, cwd: Option<&Path>, stdin: Option<&Path>, out: &Path) {
        spawn_detached(cmd, cwd, stdin, out);
    }

    fn log(&self, line: &str) {
        append_line(&self.log_path, &format!("{} {line}", hhmmss(epoch_now())));
    }
}

/// Shared context: config, host, and the paths/commands the pump uses.
#[derive(Clone)]
pub struct Ctx {
    /// The `[pump]` configuration.
    pub cfg: Arc<PumpConfig>,
    /// The command-line seam.
    pub host: Arc<dyn Host>,
    /// The project root.
    pub project: PathBuf,
    /// The pump home directory.
    pub home: PathBuf,
    /// The repository checkout directory.
    pub repo: PathBuf,
    /// The `vivi` executable to call.
    pub vivi: String,
    /// Prepended to every shell command (`""` means none).
    pub shell_prefix: String,
}

impl Ctx {
    /// Build a context from explicit parts; [`Ctx::new`] derives the tail
    /// from the config.
    #[allow(clippy::too_many_arguments)] // matches the shared pump interface
    #[must_use]
    pub fn with_parts(
        cfg: PumpConfig,
        host: Arc<dyn Host>,
        project: PathBuf,
        vivi: String,
        shell_prefix: String,
        home: PathBuf,
        repo: PathBuf,
    ) -> Ctx {
        Ctx {
            cfg: Arc::new(cfg),
            host,
            project,
            home,
            repo,
            vivi,
            shell_prefix,
        }
    }

    /// Build a context from the config alone: the shell prefix, home, and
    /// repo derive from `cfg` and `project`.
    #[must_use]
    pub fn new(cfg: PumpConfig, host: Arc<dyn Host>, project: PathBuf, vivi: String) -> Ctx {
        let shell_prefix = cfg.shell_prefix.clone();
        let home = cfg.home_dir(&project);
        let repo = cfg.repo_dir(&project);
        Ctx::with_parts(cfg, host, project, vivi, shell_prefix, home, repo)
    }

    /// Run `cmd` through the host with `shell_prefix` applied:
    /// `"<prefix>; <cmd>"`, or just `<cmd>` when the prefix is empty.
    #[must_use]
    pub fn sh(&self, cmd: &str, cwd: Option<&Path>, timeout: u64) -> CmdOutput {
        let full = if self.shell_prefix.is_empty() {
            cmd.to_string()
        } else {
            format!("{}; {cmd}", self.shell_prefix)
        };
        self.host.run(&full, cwd, timeout)
    }

    /// Run the `vivi` CLI against this project:
    /// `<vivi> <args> --project <quoted project>`.
    #[must_use]
    pub fn vivi(&self, args: &str, timeout: u64) -> CmdOutput {
        let project = shell_quote(&self.project.to_string_lossy());
        self.sh(
            &format!("{} {args} --project {project}", self.vivi),
            None,
            timeout,
        )
    }

    /// Run `git <args>` with the repository as working directory.
    #[must_use]
    pub fn git(&self, args: &str, timeout: u64) -> CmdOutput {
        self.sh(&format!("git {args}"), Some(&self.repo), timeout)
    }

    /// Append a log line via the host.
    pub fn log(&self, line: &str) {
        self.host.log(line);
    }

    /// Current epoch seconds via the host.
    #[must_use]
    pub fn now(&self) -> u64 {
        self.host.now()
    }

    /// Record an exception: append `HH:MM:SS kind unit text` (newlines folded
    /// to `" | "`, text cut at 600 chars) to `<home>/exceptions.txt` and log
    /// `EXCEPTION kind unit`.
    pub fn exception(&self, kind: &str, unit: &str, text: &str) {
        let folded = text.replace('\n', " | ");
        let cut: String = folded.chars().take(600).collect();
        let stamped = format!("{} {kind} {unit} {cut}", hhmmss(self.host.now()));
        append_line(&self.home.join("exceptions.txt"), &stamped);
        self.log(&format!("EXCEPTION {kind} {unit}"));
    }

    /// True when the flag file `<home>/<name>` exists.
    #[must_use]
    pub fn flag(&self, name: &str) -> bool {
        self.home.join(name).exists()
    }
}

/// Quote `s` for POSIX shells, like Python's `shlex.quote`.
#[must_use]
pub fn shell_quote(s: &str) -> String {
    if s.is_empty() {
        return String::from("''");
    }
    if s.chars().all(is_safe_shell_char) {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// Split `s` like Python's `shlex.split`; unbalanced quotes fall back to a
/// plain whitespace split.
#[must_use]
pub fn shell_split(s: &str) -> Vec<String> {
    match try_shell_split(s) {
        Some(words) => words,
        None => s.split_whitespace().map(String::from).collect(),
    }
}

/// `true` for characters that need no quoting.
fn is_safe_shell_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric()
        || matches!(
            ch,
            '_' | '@' | '%' | '+' | '=' | ':' | ',' | '.' | '/' | '-'
        )
}

/// A shlex-style splitter; `None` when quotes are unbalanced.
fn try_shell_split(s: &str) -> Option<Vec<String>> {
    let mut words: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for ch in s.chars() {
        if escaped {
            cur.push(ch);
            escaped = false;
        } else if quote == Some('\'') {
            if ch == '\'' {
                quote = None;
            } else {
                cur.push(ch);
            }
        } else if quote.is_some() {
            match ch {
                '"' => quote = None,
                '\\' => escaped = true,
                _ => cur.push(ch),
            }
        } else {
            match ch {
                '\'' | '"' => {
                    quote = Some(ch);
                    in_word = true;
                }
                '\\' => {
                    escaped = true;
                    in_word = true;
                }
                c if c.is_whitespace() => {
                    if in_word {
                        words.push(std::mem::take(&mut cur));
                        in_word = false;
                    }
                }
                c => {
                    cur.push(c);
                    in_word = true;
                }
            }
        }
    }
    if quote.is_some() {
        return None;
    }
    if escaped {
        cur.push('\\');
    }
    if in_word {
        words.push(cur);
    }
    Some(words)
}

/// In-memory state behind [`FakeHost`].
#[derive(Default)]
struct FakeState {
    once: VecDeque<(String, i32, String)>,
    rules: Vec<(String, i32, String)>,
    commands: Vec<String>,
    spawned: Vec<String>,
    logs: Vec<String>,
}

/// Fake host: the test seam every pump unit tests through.
pub struct FakeHost {
    clock: AtomicU64,
    state: Mutex<FakeState>,
}

impl FakeHost {
    /// A fake host whose clock starts at `1_000_000`.
    #[must_use]
    pub fn new() -> Arc<FakeHost> {
        Arc::new(FakeHost {
            clock: AtomicU64::new(1_000_000),
            state: Mutex::new(FakeState::default()),
        })
    }

    /// Set the clock.
    pub fn set_now(&self, t: u64) {
        self.clock.store(t, Ordering::SeqCst);
    }

    /// Advance the clock by `secs`.
    pub fn advance(&self, secs: u64) {
        self.clock.fetch_add(secs, Ordering::SeqCst);
    }

    /// Persistent rule: every later `run` whose command line contains
    /// `contains` answers with `code`/`text`. A rule with the same fragment
    /// replaces the earlier one.
    pub fn reply(&self, contains: &str, code: i32, text: &str) {
        if let Ok(mut state) = self.state.lock() {
            state.rules.retain(|(fragment, _, _)| fragment != contains);
            state
                .rules
                .push((contains.to_string(), code, text.to_string()));
        }
    }

    /// One-shot rule, consumed before persistent rules, in order.
    pub fn reply_once(&self, contains: &str, code: i32, text: &str) {
        if let Ok(mut state) = self.state.lock() {
            state
                .once
                .push_back((contains.to_string(), code, text.to_string()));
        }
    }

    /// Every `run` command line, in order.
    #[must_use]
    pub fn commands(&self) -> Vec<String> {
        self.snapshot(|state| state.commands.clone())
    }

    /// Every `spawn` command line, in order.
    #[must_use]
    pub fn spawned(&self) -> Vec<String> {
        self.snapshot(|state| state.spawned.clone())
    }

    /// Every line passed to `log`, in order.
    #[must_use]
    pub fn logs(&self) -> Vec<String> {
        self.snapshot(|state| state.logs.clone())
    }

    fn snapshot<T>(&self, read: impl FnOnce(&FakeState) -> T) -> T {
        let guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        read(&guard)
    }
}

impl Host for FakeHost {
    fn now(&self) -> u64 {
        self.clock.load(Ordering::SeqCst)
    }

    fn run(&self, cmd: &str, _cwd: Option<&Path>, _timeout_secs: u64) -> CmdOutput {
        if let Ok(mut state) = self.state.lock() {
            state.commands.push(cmd.to_string());
            let once = state
                .once
                .iter()
                .position(|(fragment, _, _)| cmd.contains(fragment.as_str()));
            if let Some(index) = once
                && let Some((_, code, text)) = state.once.remove(index)
            {
                return CmdOutput { code, text };
            }
            for (fragment, code, text) in state.rules.iter().rev() {
                if cmd.contains(fragment.as_str()) {
                    return CmdOutput {
                        code: *code,
                        text: text.clone(),
                    };
                }
            }
        }
        CmdOutput {
            code: 0,
            text: String::new(),
        }
    }

    fn spawn(&self, cmd: &str, _cwd: Option<&Path>, _stdin: Option<&Path>, _out: &Path) {
        if let Ok(mut state) = self.state.lock() {
            state.spawned.push(cmd.to_string());
        }
    }

    fn log(&self, line: &str) {
        if let Ok(mut state) = self.state.lock() {
            state.logs.push(line.to_string());
        }
    }
}

/// A [`Ctx`] rooted at `dir` with a [`FakeHost`]: home is `dir/home`, repo is
/// `dir/home/repo`, project is `dir/project`, `vivi` is `"vivi"`, and the
/// shell prefix is empty. Creates the home and repo directories.
#[must_use]
pub fn test_ctx(dir: &Path, cfg: PumpConfig) -> (Ctx, Arc<FakeHost>) {
    let home = dir.join("home");
    let repo = home.join("repo");
    drop(fs::create_dir_all(&repo));
    let host = FakeHost::new();
    let ctx = Ctx::with_parts(
        cfg,
        host.clone(),
        dir.join("project"),
        String::from("vivi"),
        String::new(),
        home,
        repo,
    );
    (ctx, host)
}

// --- real-host plumbing ------------------------------------------------

/// A byte buffer shared with a reader thread.
type SharedBuf = Arc<Mutex<Vec<u8>>>;

/// Build the `bash -c <cmd>` command for `cmd`.
fn bash_command(cmd: &str, cwd: Option<&Path>) -> Command {
    let mut command = Command::new("bash");
    command.arg("-c").arg(cmd);
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }
    command
}

/// Drain `reader` into `buf` until EOF; a full pipe can never hang the caller.
fn spawn_reader(reader: Box<dyn Read + Send>, buf: SharedBuf) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut pipe = reader;
        let mut chunk = [0_u8; 8192];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if let Ok(mut bytes) = buf.lock() {
                        bytes.extend_from_slice(&chunk[..n]);
                    }
                }
            }
        }
    })
}

/// Kill the whole process group `pgid` (the negative id targets the group).
fn kill_group(pgid: u32) {
    let kill = Command::new("kill")
        .arg("-KILL")
        .arg(format!("-{pgid}"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    drop(kill);
}

/// Lossy UTF-8 snapshot of a shared buffer.
fn snapshot(buf: &SharedBuf) -> String {
    let guard = buf
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    String::from_utf8_lossy(guard.as_slice()).into_owned()
}

/// Let reader threads finish for at most `max`; unfinished ones detach (their
/// pipe is still held open by a lingering grandchild).
fn wait_readers(handles: Vec<JoinHandle<()>>, max: Duration) {
    let deadline = Instant::now() + max;
    for handle in handles {
        while !handle.is_finished() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        if handle.is_finished() {
            drop(handle.join());
        }
    }
}

/// Run `cmd` under `bash` with a timeout, draining pipes on reader threads.
fn run_command(cmd: &str, cwd: Option<&Path>, timeout_secs: u64) -> CmdOutput {
    let mut command = bash_command(cmd, cwd);
    command.stdin(Stdio::null());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    // Safety: only sets the child's own process group; no invariants involved.
    command.process_group(0);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            return CmdOutput {
                code: 127,
                text: err.to_string(),
            };
        }
    };
    let pgid = child.id();
    let out_buf: SharedBuf = Arc::new(Mutex::new(Vec::new()));
    let err_buf: SharedBuf = Arc::new(Mutex::new(Vec::new()));
    let mut handles = Vec::new();
    if let Some(pipe) = child.stdout.take() {
        handles.push(spawn_reader(Box::new(pipe), Arc::clone(&out_buf)));
    }
    if let Some(pipe) = child.stderr.take() {
        handles.push(spawn_reader(Box::new(pipe), Arc::clone(&err_buf)));
    }
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                wait_readers(handles, Duration::from_secs(5));
                return CmdOutput {
                    code: status.code().unwrap_or(1),
                    text: joined_output(&out_buf, &err_buf),
                };
            }
            Ok(None) => {}
            Err(err) => {
                wait_readers(handles, Duration::from_secs(1));
                return CmdOutput {
                    code: 127,
                    text: err.to_string(),
                };
            }
        }
        if Instant::now() >= deadline {
            kill_group(pgid);
            drop(child.wait());
            wait_readers(handles, Duration::from_secs(5));
            let mut text = joined_output(&out_buf, &err_buf);
            text.push_str("\n[timeout after ");
            text.push_str(&timeout_secs.to_string());
            text.push_str("s]");
            return CmdOutput { code: 124, text };
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// Stdout then stderr as one lossy string.
fn joined_output(out_buf: &SharedBuf, err_buf: &SharedBuf) -> String {
    let mut text = snapshot(out_buf);
    text.push_str(&snapshot(err_buf));
    text
}

/// Start `cmd` detached: own process group, stdin from `stdin` when given,
/// stdout and stderr appended to `out`. Returns without waiting; a reaper
/// thread collects the child so no zombie is left behind.
fn spawn_detached(cmd: &str, cwd: Option<&Path>, stdin: Option<&Path>, out: &Path) {
    let mut command = bash_command(cmd, cwd);
    command.stdin(match stdin {
        Some(path) => match File::open(path) {
            Ok(file) => Stdio::from(file),
            Err(_) => Stdio::null(),
        },
        None => Stdio::null(),
    });
    if let Ok(file) = OpenOptions::new().create(true).append(true).open(out) {
        let clone = file.try_clone();
        command.stdout(Stdio::from(file));
        command.stderr(match clone {
            Ok(dup) => Stdio::from(dup),
            Err(_) => Stdio::null(),
        });
    } else {
        command.stdout(Stdio::null());
        command.stderr(Stdio::null());
    }
    // Safety: only sets the child's own process group; no invariants involved.
    command.process_group(0);
    if let Ok(mut child) = command.spawn() {
        thread::spawn(move || drop(child.wait()));
    }
}

/// Append one `line` (plus newline) to `path`, creating parent directories.
fn append_line(path: &Path, line: &str) {
    if let Some(parent) = path.parent() {
        drop(fs::create_dir_all(parent));
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        drop(writeln!(file, "{line}"));
    }
}

/// Current epoch seconds.
fn epoch_now() -> u64 {
    u64::try_from(chrono::Utc::now().timestamp()).unwrap_or(0)
}

/// `HH:MM:SS` (UTC) for `secs` since the epoch.
fn hhmmss(secs: u64) -> String {
    let stamp = i64::try_from(secs).unwrap_or(0);
    chrono::DateTime::<chrono::Utc>::from_timestamp(stamp, 0)
        .map_or_else(String::new, |t| t.format("%H:%M:%S").to_string())
}
