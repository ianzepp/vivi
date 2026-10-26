//! Shared fixture for the measurement tests in `server_measure_index.rs`:
//! seeding isolated hosted roots with thousands of letters, and the child
//! writer processes that run beside a measurement.
//!
//! Every run is isolated: mailspace roots live under a scratch base
//! directory, the hosted data directory sits beside them, and child writer
//! processes get their own `VIVI_CONFIG_DIR` (see `src/registry.rs`), so no
//! run touches the user's registry, config, or home. Writers use the vivi
//! library's own mailspace APIs against the mailspace root only; they never
//! share the measurement's server process or its `ProjectStore`.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use vivi::error::VivariumError;
use vivi::mailspace::{Mailspace, SendRequest};

/// The off-host address the seed and the writers send to.
pub const OFF_HOST: &str = "mind@0badc0de.local";

/// The identity the seed and the writers send from; the one whose moves
/// close and reopen work.
pub const AUTHOR: &str = "mind";

/// The local identity the seed and the writers address on the host.
pub const HOLDER: &str = "hand";

/// The test in this binary a parent process launches as a child writer.
pub const WRITER_ENTRY: &str = "writer_child_run";

/// Environment the parent uses to hand a writer its schedule.
pub const ROOT_ENV: &str = "VIVI_WRITER_ROOT";
pub const TICKS_ENV: &str = "VIVI_WRITER_TICKS";
pub const TICK_MS_ENV: &str = "VIVI_WRITER_TICK_MS";
pub const MODE_ENV: &str = "VIVI_WRITER_MODE";
pub const RECEIPT_ENV: &str = "VIVI_WRITER_RECEIPT";
pub const NAME_ENV: &str = "VIVI_WRITER_NAME";

/// Where a child process keeps its own registry state.
pub const CONFIG_DIR_ENV: &str = "VIVI_CONFIG_DIR";

/// The default writer schedule: a handful of ticks, one every few seconds,
/// so an unconfigured run is bounded.
pub const DEFAULT_TICKS: usize = 4;
pub const DEFAULT_TICK_MS: u64 = 2_000;

/// Wall time of `work`, printed with `label`.
pub fn timed<T>(label: &str, work: impl FnOnce() -> T) -> (T, Duration) {
    let start = Instant::now();
    let out = work();
    let took = start.elapsed();
    println!("{label}: {:.1} ms", took.as_secs_f64() * 1000.0);
    (out, took)
}

/// A fresh scratch base directory for one run; anything left over goes.
pub fn scratch_base(tag: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("vivi-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    base
}

/// A format-1 need body naming `subject` as its done condition.
pub fn need_body(subject: &str) -> String {
    format!("+++\ndone_when = [\"{subject} is done\"]\n+++\n\n{subject} body\n")
}

/// A format-1 task body with empty front matter.
pub fn task_body(subject: &str) -> String {
    format!("+++\n\n+++\n\n{subject} body\n")
}

/// One letter from the author: needs travel as format-1 letters under
/// `needs`, tasks as format-1 letters under `tasks`, mail plain under
/// `inbox`. Returns the delivery so the caller can keep the handles.
pub fn send_letter(
    ms: &Mailspace,
    to: &str,
    kind: Option<&str>,
    subject: &str,
) -> vivi::mailspace::DeliveryResult {
    let role = match kind {
        Some("need") => "needs",
        Some("task") => "tasks",
        _ => "inbox",
    };
    let body = match kind {
        Some("need") => need_body(subject),
        Some("task") => task_body(subject),
        _ => format!("{subject} body\n"),
    };
    ms.send(SendRequest {
        from: AUTHOR.into(),
        to: vec![to.into()],
        cc: Vec::new(),
        bcc: Vec::new(),
        subject: subject.to_owned(),
        body,
        role: role.into(),
        kind: kind.map(str::to_owned),
        reply_to: None,
    })
    .unwrap()
}

/// The measurement mix for letter `n`: a quarter off-host needs, a quarter
/// off-host mail, half local-only needs for the host's own hand.
pub fn send_seed_letter(ms: &Mailspace, n: usize) {
    let own_hand = ms.address_for(HOLDER);
    match n % 4 {
        0 => {
            send_letter(ms, OFF_HOST, Some("need"), &format!("letter {n}"));
        }
        1 => {
            send_letter(ms, OFF_HOST, None, &format!("letter {n}"));
        }
        _ => {
            send_letter(ms, &own_hand, Some("need"), &format!("letter {n}"));
        }
    }
}

/// One writer-style churn group on an already-seeded mailspace: an
/// off-host need, an off-host letter, and two local-only letters, with
/// subjects unique per `tag`.
pub fn churn_group(ms: &Mailspace, tag: &str) {
    let own_hand = ms.address_for(HOLDER);
    let letters = [
        (OFF_HOST, Some("need"), "need"),
        (OFF_HOST, None, "mail"),
        (own_hand.as_str(), Some("need"), "local need"),
        (own_hand.as_str(), None, "local mail"),
    ];
    for (i, (to, kind, label)) in letters.into_iter().enumerate() {
        send_letter(ms, to, kind, &format!("churn {tag} {label} {i}"));
    }
}

/// An empty mailspace at `root` with the host identities and the hub sync
/// config the hosted index expects (`send = "addressed"`).
pub fn prepare_root(root: &Path, name: &str) -> Mailspace {
    let mut ms = Mailspace::init(Some(root)).unwrap();
    ms.add_identity(AUTHOR).unwrap();
    ms.add_identity(HOLDER).unwrap();
    let path = ms.config_path();
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str(&format!(
        "\n[sync]\nname = \"{name}\"\nsend = \"addressed\"\n"
    ));
    std::fs::write(&path, text).unwrap();
    Mailspace::open_root(root.to_path_buf()).unwrap()
}

/// A mailspace at `root` seeded with `letters` letters in the measurement
/// mix.
pub fn seeded_root(root: &Path, letters: usize, name: &str) -> Mailspace {
    let ms = prepare_root(root, name);
    for n in 0..letters {
        send_seed_letter(&ms, n);
    }
    ms
}

/// What a writer does each tick besides adding letters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriterMode {
    /// Letters plus lifecycle moves only: close the previous tick's local
    /// need, reopen it two ticks later.
    Letters,
    /// The writer-overhead mode: each tick lowers one unit task onto one
    /// fresh need, then closes the unit so the join completes the need.
    /// The verification-only binding updates live here, so the letters
    /// mode's folder counts stay exact.
    Overhead,
}

impl WriterMode {
    pub fn parse(raw: &str) -> Option<WriterMode> {
        match raw {
            "letters" => Some(WriterMode::Letters),
            "overhead" => Some(WriterMode::Overhead),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            WriterMode::Letters => "letters",
            WriterMode::Overhead => "overhead",
        }
    }
}

/// The fixed, recorded schedule one writer runs: `ticks` rounds `tick`
/// apart against one mailspace root, then it stops. Bounded by construction.
#[derive(Debug, Clone)]
pub struct WriterConfig {
    pub root: PathBuf,
    pub ticks: usize,
    pub tick: Duration,
    pub mode: WriterMode,
    /// Where the receipt JSON lands, when the parent wants it back.
    pub receipt: Option<PathBuf>,
    pub writer: String,
}

impl WriterConfig {
    /// The schedule from the environment; `VIVI_WRITER_ROOT` is required.
    pub fn from_env() -> Result<WriterConfig, String> {
        let root = std::env::var(ROOT_ENV).map_err(|_| format!("{ROOT_ENV} is required"))?;
        Ok(WriterConfig {
            root: root.into(),
            ticks: env_parsed(TICKS_ENV).unwrap_or(DEFAULT_TICKS),
            tick: Duration::from_millis(env_parsed(TICK_MS_ENV).unwrap_or(DEFAULT_TICK_MS)),
            mode: WriterMode::parse(&std::env::var(MODE_ENV).unwrap_or_default())
                .unwrap_or(WriterMode::Letters),
            receipt: std::env::var(RECEIPT_ENV).ok().map(PathBuf::from),
            writer: std::env::var(NAME_ENV).unwrap_or_else(|_| "writer".into()),
        })
    }

    /// A short bounded run a parent test asserts against.
    pub fn short_run(root: &Path, receipt: &Path, mode: WriterMode, writer: &str) -> WriterConfig {
        WriterConfig {
            root: root.to_path_buf(),
            ticks: 2,
            tick: Duration::from_millis(150),
            mode,
            receipt: Some(receipt.to_path_buf()),
            writer: writer.to_owned(),
        }
    }

    /// How long a parent should wait for this schedule before killing the
    /// child.
    pub fn budget(&self) -> Duration {
        self.tick * (self.ticks as u32 + 2) + Duration::from_secs(20)
    }
}

fn env_parsed<T: std::str::FromStr>(name: &str) -> Option<T> {
    std::env::var(name).ok().and_then(|raw| raw.parse().ok())
}

/// One letter a writer sent, with the lifecycle and binding the schedule
/// later gave it. This is the recorded schedule a parent asserts against.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LetterRecord {
    pub tick: usize,
    pub seq: usize,
    /// `need`, `task`, or `mail`; mail mints no backlog node.
    pub kind: String,
    pub local: bool,
    /// The handle the item's backlog node is minted for: the delivered copy
    /// for local letters, the sent copy for off-host ones.
    pub item: String,
    /// The author's own handle (the sent copy), which close/reopen move.
    pub author_copy: String,
    pub closed: bool,
    pub reopened: bool,
    pub bound: bool,
    pub joined: bool,
}

/// What one writer run did: the schedule it ran and everything it touched.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriterReceipt {
    pub writer: String,
    pub mode: String,
    pub root: String,
    pub ticks_requested: usize,
    pub ticks_ran: usize,
    pub tick_ms: u64,
    pub letters_sent: usize,
    pub offhost_letters: usize,
    pub local_letters: usize,
    pub closes: usize,
    pub reopens: usize,
    pub binds: usize,
    pub joined_needs: usize,
    pub items: Vec<LetterRecord>,
}

/// The writer's running totals.
#[derive(Default)]
struct WriterState {
    seq: usize,
    offhost: usize,
    closes: usize,
    reopens: usize,
    binds: usize,
    joined_needs: usize,
    items: Vec<LetterRecord>,
    /// The item handle of the previous tick's local need: the lifecycle
    /// target the next tick closes or reopens.
    pending_need: Option<String>,
}

/// Run one writer to the end of its fixed schedule. The writer only touches
/// the mailspace at `config.root`, through the library's own APIs.
pub fn run_writer(config: &WriterConfig) -> Result<WriterReceipt, VivariumError> {
    let ms = Mailspace::open_root(config.root.clone())?;
    let own_hand = ms.address_for(HOLDER);
    let mut state = WriterState::default();
    println!(
        "writer {}: mode {} on {}: {} ticks x {} ms (fixed schedule)",
        config.writer,
        config.mode.as_str(),
        config.root.display(),
        config.ticks,
        config.tick.as_millis()
    );
    for tick in 0..config.ticks {
        match config.mode {
            WriterMode::Letters => letters_tick(&ms, &own_hand, tick, &mut state),
            WriterMode::Overhead => overhead_tick(&ms, &own_hand, tick, &mut state),
        }
        println!(
            "writer {}: tick {tick}/{}: {} letters so far, {} closes, {} reopens, {} binds",
            config.writer, config.ticks, state.seq, state.closes, state.reopens, state.binds
        );
        if tick + 1 < config.ticks {
            thread::sleep(config.tick);
        }
    }
    let receipt = WriterReceipt {
        writer: config.writer.clone(),
        mode: config.mode.as_str().to_owned(),
        root: config.root.display().to_string(),
        ticks_requested: config.ticks,
        ticks_ran: config.ticks,
        tick_ms: config.tick.as_millis() as u64,
        letters_sent: state.seq,
        offhost_letters: state.offhost,
        local_letters: state.seq - state.offhost,
        closes: state.closes,
        reopens: state.reopens,
        binds: state.binds,
        joined_needs: state.joined_needs,
        items: state.items,
    };
    println!(
        "writer {} receipt: {}",
        config.writer,
        receipt_json(&receipt)
    );
    if let Some(path) = &config.receipt {
        std::fs::write(path, receipt_json(&receipt))?;
    }
    Ok(receipt)
}

fn receipt_json(receipt: &WriterReceipt) -> String {
    serde_json::to_string_pretty(receipt).unwrap()
}

/// The four-letter group: an off-host need, an off-host letter, and two
/// local-only letters. Returns the item handle of the local need, which the
/// next tick's lifecycle move targets.
fn writer_group(
    ms: &Mailspace,
    own_hand: &str,
    tick: usize,
    state: &mut WriterState,
) -> Option<String> {
    let mut local_need = None;
    for i in 0..4 {
        state.seq += 1;
        let (to, kind) = match i {
            0 => (OFF_HOST.to_string(), Some("need")),
            1 => (OFF_HOST.to_string(), None),
            2 => (own_hand.to_string(), Some("need")),
            _ => (own_hand.to_string(), None),
        };
        let subject = format!("writer letter {}", state.seq);
        let delivered = send_letter(ms, &to, kind, &subject);
        if to == OFF_HOST {
            state.offhost += 1;
        }
        let record = LetterRecord {
            tick,
            seq: state.seq,
            kind: (if kind == Some("need") { "need" } else { "mail" }).to_owned(),
            local: to != OFF_HOST,
            item: delivered
                .delivered
                .first()
                .map(|d| d.handle.clone())
                .unwrap_or_else(|| delivered.sent.clone()),
            author_copy: delivered.sent.clone(),
            closed: false,
            reopened: false,
            bound: false,
            joined: false,
        };
        if i == 2 {
            local_need = Some(record.item.clone());
        }
        state.items.push(record);
    }
    local_need
}

/// One letters-mode tick: the group, then the lifecycle move on the
/// previous tick's local need (odd ticks close it, even ticks reopen it).
fn letters_tick(ms: &Mailspace, own_hand: &str, tick: usize, state: &mut WriterState) {
    let local_need = writer_group(ms, own_hand, tick, state);
    if let Some(handle) = state.pending_need.take() {
        if tick % 2 == 1 {
            close_item(ms, &handle);
            mark_item(&mut state.items, &handle, |record| record.closed = true);
            state.closes += 1;
        } else {
            reopen_item(ms, &handle);
            mark_item(&mut state.items, &handle, |record| record.reopened = true);
            state.reopens += 1;
        }
    }
    state.pending_need = local_need;
}

/// One writer-overhead tick: a fresh need lowered onto a fresh unit task,
/// then the unit closed so the join completes the need.
fn overhead_tick(ms: &Mailspace, own_hand: &str, tick: usize, state: &mut WriterState) {
    state.seq += 1;
    let need_subject = format!("writer need {}", state.seq);
    let need = send_letter(ms, own_hand, Some("need"), &need_subject);
    let need_item = need.delivered[0].handle.clone();
    state.seq += 1;
    let unit_subject = format!("writer unit {}", state.seq);
    let unit = send_letter(ms, own_hand, Some("task"), &unit_subject);
    let unit_item = unit.delivered[0].handle.clone();
    ms.backlog_bind_units(&need_item, &[unit_item.clone()])
        .unwrap();
    close_item(ms, &unit.sent);
    state.binds += 1;
    state.joined_needs += 1;
    state.items.push(LetterRecord {
        tick,
        seq: state.seq - 1,
        kind: "need".to_owned(),
        local: true,
        item: need_item,
        author_copy: need.sent.clone(),
        closed: false,
        reopened: false,
        bound: true,
        joined: true,
    });
    state.items.push(LetterRecord {
        tick,
        seq: state.seq,
        kind: "task".to_owned(),
        local: true,
        item: unit_item,
        author_copy: unit.sent.clone(),
        closed: true,
        reopened: false,
        bound: true,
        joined: false,
    });
}

fn close_item(ms: &Mailspace, author_copy: &str) {
    ms.move_item(AUTHOR, author_copy, "done", None, "close", None)
        .unwrap();
}

fn reopen_item(ms: &Mailspace, author_copy: &str) {
    ms.move_item(AUTHOR, author_copy, "needs", None, "reopen", None)
        .unwrap();
}

fn mark_item(items: &mut [LetterRecord], item: &str, apply: impl Fn(&mut LetterRecord)) {
    if let Some(record) = items.iter_mut().find(|record| record.item == item) {
        apply(record);
    }
}

/// The body of the `writer_child_run` entry point. With `VIVI_WRITER_ROOT`
/// set, the schedule comes from the environment; without one, a bounded
/// self-contained run against a scratch mailspace still proves the entry
/// point works.
pub fn child_writer_main() {
    match WriterConfig::from_env() {
        Ok(config) => {
            run_writer(&config).unwrap();
        }
        Err(_) => {
            let base = scratch_base("server-measure-writer-standalone");
            let root = base.join("host");
            seeded_root(&root, 8, "hub-writer");
            let config = WriterConfig {
                root,
                ticks: 2,
                tick: Duration::from_millis(100),
                mode: WriterMode::Letters,
                receipt: None,
                writer: "standalone".into(),
            };
            run_writer(&config).unwrap();
            let _ = std::fs::remove_dir_all(&base);
        }
    }
}

/// Launch this binary's `writer_child_run` test as a child process with the
/// given schedule. The child gets its own `VIVI_CONFIG_DIR` so it never
/// touches the user's registry.
pub fn spawn_child_writer(config: &WriterConfig, config_dir: &Path) -> std::process::Child {
    let exe = std::env::current_exe().unwrap();
    std::process::Command::new(&exe)
        .args(["--exact", WRITER_ENTRY, "--ignored", "--nocapture"])
        .env(ROOT_ENV, &config.root)
        .env(TICKS_ENV, config.ticks.to_string())
        .env(TICK_MS_ENV, config.tick.as_millis().to_string())
        .env(MODE_ENV, config.mode.as_str())
        .env(NAME_ENV, &config.writer)
        .env(RECEIPT_ENV, config.receipt.clone().unwrap_or_default())
        .env(CONFIG_DIR_ENV, config_dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap()
}

/// Wait for `child` up to `budget`, killing it on overrun. Returns the
/// child's output on success.
pub fn wait_for(child: &mut std::process::Child, budget: Duration) -> Result<String, String> {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return if status.success() {
                Ok(drain(child))
            } else {
                Err(format!("child writer failed: {status}\n{}", drain(child)))
            };
        }
        if start.elapsed() > budget {
            let _ = child.kill();
            let _ = child.wait();
            return Err("child writer overran its budget".into());
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn drain(child: &mut std::process::Child) -> String {
    let mut out = String::new();
    if let Some(pipe) = child.stdout.as_mut() {
        let _ = pipe.read_to_string(&mut out);
    }
    if let Some(pipe) = child.stderr.as_mut() {
        let _ = pipe.read_to_string(&mut out);
    }
    out
}

/// Read the receipt a child writer wrote.
pub fn read_receipt(path: &Path) -> WriterReceipt {
    let text = std::fs::read_to_string(path).unwrap();
    serde_json::from_str(&text).unwrap()
}

/// The storage state one mailspace root presents: the folder totals, the
/// per-account row counts, and the backlog node states. Opened fresh so a
/// parent never shares a connection with a writer.
pub fn snapshot(root: &Path) -> StorageSnapshot {
    let ms = Mailspace::open_root(root.to_path_buf()).unwrap();
    let status = ms.status().unwrap();
    let totals = &status.totals;
    let storage = ms.storage().unwrap();
    StorageSnapshot {
        needs_open: totals.needs_open,
        inbox_unread: totals.inbox_unread,
        tasks_open: totals.tasks_open,
        done: status.identities.iter().map(|i| i.done).sum(),
        hand_rows: storage.count_messages_for_account(HOLDER).unwrap(),
        mind_rows: storage.count_messages_for_account(AUTHOR).unwrap(),
        nodes: ms
            .graph_show("backlog")
            .unwrap()
            .nodes
            .into_iter()
            .map(|node| (node.source_id, node.state))
            .collect(),
    }
}

/// What [`snapshot`] reads.
#[derive(Debug, Clone)]
pub struct StorageSnapshot {
    pub needs_open: usize,
    pub inbox_unread: usize,
    pub tasks_open: usize,
    pub done: usize,
    pub hand_rows: usize,
    pub mind_rows: usize,
    pub nodes: BTreeMap<String, String>,
}
