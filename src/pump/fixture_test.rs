//! A real mailspace project for the pump tests that act on pump records:
//! the `operator` and `mind` identities, a `[pump]` table, a pump context
//! whose project and home are the project's own, and needs to send records
//! for.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use tempfile::{TempDir, tempdir};

use crate::mailspace::{Mailspace, SendRequest};
use crate::pump::config::PumpConfig;
use crate::pump::host::{CmdOutput, Ctx, FakeHost, Host, test_ctx};

/// The fake clock's start.
pub(crate) const T0: u64 = 1_000_000;

pub(crate) struct Project {
    pub dir: TempDir,
    pub mailspace: Mailspace,
    pub ctx: Ctx,
    pub host: Arc<FakeHost>,
}

impl Project {
    /// The project root.
    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    /// The pump home `vivi pump` resolves for this project.
    pub fn home(&self) -> PathBuf {
        self.dir.path().join(".vivi").join("pump")
    }

    /// Send a need to the pump identity and return its handle.
    pub fn need(&self, subject: &str) -> String {
        send_need(&self.mailspace, subject, "mind")
    }

    /// A pump context over the project, home at [`Project::home`].
    pub fn pump_ctx(&self) -> Ctx {
        let mut ctx = self.ctx.clone();
        ctx.home = self.home();
        fs::create_dir_all(&ctx.home).unwrap();
        ctx
    }
}

/// A fresh project. The pump's home is created lazily by the code under test.
pub(crate) fn project() -> Project {
    let dir = tempdir().unwrap();
    let mut mailspace = Mailspace::init(Some(dir.path())).unwrap();
    mailspace.add_identity("operator").unwrap();
    mailspace.add_identity("mind").unwrap();
    mailspace.add_identity("elsewhere").unwrap();
    let toml = dir.path().join(".vivi").join("mailspace.toml");
    let mut text = fs::read_to_string(&toml).unwrap();
    text.push_str("\n[pump]\nbranch = \"next\"\n");
    fs::write(&toml, text).unwrap();
    let (mut ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    host.set_now(T0);
    ctx.project = dir.path().to_path_buf();
    ctx.home = dir.path().join(".vivi").join("pump");
    ctx.mailspace = String::from("fixture");
    Project {
        dir,
        mailspace,
        ctx,
        host,
    }
}

/// Send a need from `operator` to `to` and return its handle.
pub(crate) fn send_need(mailspace: &Mailspace, subject: &str, to: &str) -> String {
    mailspace
        .send(SendRequest {
            from: "operator".into(),
            to: vec![to.into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.into(),
            body: crate::letter::fixture::need_body(&["it works"], ""),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle
}

/// A [`FakeHost`] that watches `pool dispatch` calls: how many were in flight
/// at once, which threads ran them, and the order they finished in. A call
/// whose command holds `fail` exits 1. While `want` is above 1, calls wait
/// (3-second deadline, so a serial regression fails rather than hangs) until
/// `want` of them are in flight together; once that has happened, later calls
/// pass straight through. A call whose command holds `last` waits until
/// `last_after` other calls have finished.
pub(crate) struct ProbeHost {
    pub inner: Arc<FakeHost>,
    pub live: AtomicUsize,
    pub peak: AtomicUsize,
    pub threads: Mutex<Vec<std::thread::ThreadId>>,
    pub finished: Mutex<Vec<String>>,
    pub want: usize,
    pub met: AtomicBool,
    pub gate: Mutex<()>,
    pub signal: Condvar,
    pub fail: Option<String>,
    pub last: Option<String>,
    pub last_after: usize,
}

impl ProbeHost {
    pub fn new(inner: &Arc<FakeHost>) -> Self {
        Self {
            inner: inner.clone(),
            live: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            threads: Mutex::new(Vec::new()),
            finished: Mutex::new(Vec::new()),
            want: 1,
            met: AtomicBool::new(false),
            gate: Mutex::new(()),
            signal: Condvar::new(),
            fail: None,
            last: None,
            last_after: 0,
        }
    }

    fn wait_until(&self, done: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut held = self.gate.lock().unwrap();
        while !done() {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                break;
            };
            let (guard, _) = self.signal.wait_timeout(held, remaining).unwrap();
            held = guard;
        }
    }
}

impl Host for ProbeHost {
    fn now(&self) -> u64 {
        self.inner.now()
    }

    fn run(&self, cmd: &str, cwd: Option<&Path>, timeout_secs: u64) -> CmdOutput {
        if !cmd.contains("pool dispatch") {
            return self.inner.run(cmd, cwd, timeout_secs);
        }
        self.threads
            .lock()
            .unwrap()
            .push(std::thread::current().id());
        let now = self.live.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(now, Ordering::SeqCst);
        if self.want > 1 {
            if now >= self.want {
                self.met.store(true, Ordering::SeqCst);
            }
            drop(self.gate.lock().unwrap());
            self.signal.notify_all();
            self.wait_until(|| self.met.load(Ordering::SeqCst));
        }
        if self.last.as_deref().is_some_and(|frag| cmd.contains(frag)) {
            self.wait_until(|| self.finished.lock().unwrap().len() >= self.last_after);
        }
        let out = if self.fail.as_deref().is_some_and(|frag| cmd.contains(frag)) {
            CmdOutput {
                code: 1,
                text: String::from("remote refused"),
            }
        } else {
            self.inner.run(cmd, cwd, timeout_secs)
        };
        self.finished.lock().unwrap().push(cmd.to_owned());
        self.live.fetch_sub(1, Ordering::SeqCst);
        drop(self.gate.lock().unwrap());
        self.signal.notify_all();
        out
    }

    fn spawn(
        &self,
        cmd: &str,
        cwd: Option<&Path>,
        stdin: Option<&Path>,
        out: &Path,
    ) -> Option<u32> {
        self.inner.spawn(cmd, cwd, stdin, out)
    }

    fn log(&self, line: &str) {
        self.inner.log(line)
    }
}
