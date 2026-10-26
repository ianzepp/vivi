#![allow(dead_code)]
//! Disposable hosted-process measurement fixtures: two isolated hosted
//! mailspaces registered with an isolated server data directory, plus
//! normal-API writer helpers whose traffic is classifiable (local-only,
//! and predominantly local with periodic off-host letters), the separately
//! selectable measurement write modes over those writers, and the window
//! summary the process benchmark reports: measured counters (server CPU,
//! wall, pass latency, rows, writer work) kept distinct from the candidate
//! selector work, which is derived from actual row counts and the verified
//! full-listing selection, never measured.
//!
//! Everything lives under a fresh temp directory: the server data
//! directory, the identity registry, and each mailspace's own sync key
//! (minted under the root's `.vivi`, then admitted by `hosted::reconcile`
//! as the creator member's authentication material). Nothing here reads or
//! writes the operator registry or operator credentials. Include it from a
//! test target with
//! `#[path = "support/hosted_measure.rs"] mod hosted_measure;`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use rusqlite::Connection;
use vivi::mailspace::{Mailspace, SendRequest};
use vivi::server::hosted;
use vivi::server::identities::IdentityRegistry;
use vivi::server::projects::{ListQuery, ProjectRecord, ProjectStore, Viewer};
use vivi::sync::identity::SyncIdentity;

/// The off-host address every periodic off-host letter goes to: a
/// well-formed `role@prefix.local` in another mailspace, so the letter
/// leaves a sent copy locally and nothing else.
pub const OFF_HOST: &str = "mind@0badc0de.local";

/// Cadence of the periodic off-host letters in the mixed writer: one off
/// the host when the pass starts, then one every two seconds.
pub const OFF_HOST_EVERY: Duration = Duration::from_secs(2);

/// The measurement window a real run collects: long enough that the
/// server's own five-second indexing cadence (its unchanged default) fires
/// many times, whatever the mode writes.
pub const MEASURE_WINDOW: Duration = Duration::from_secs(120);

/// Which writes a measurement window carries. The modes are separately
/// selectable (`WindowMode::parse`): the idle window baselines a settled
/// mailspace, the local window loads delivery only, and the mixed window
/// loads the corpus shape the off-host selector work scales with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowMode {
    /// No writes at all: the server idles on a settled mailspace.
    Idle,
    /// Local-only letters to each host's own hand; nothing leaves a host.
    Local,
    /// Predominantly local letters with periodic off-host letters on the
    /// corpus's own [`OFF_HOST_EVERY`] schedule.
    Mixed,
}

impl WindowMode {
    /// The mode's name, as `WindowMode::parse` and `VIVI_MEASURE_MODE`
    /// spell it.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            WindowMode::Idle => "idle",
            WindowMode::Local => "local",
            WindowMode::Mixed => "mixed",
        }
    }

    /// The mode a name selects, or nothing for an unknown name.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "idle" => Some(WindowMode::Idle),
            "local" => Some(WindowMode::Local),
            "mixed" => Some(WindowMode::Mixed),
            _ => None,
        }
    }
}

/// The smallest corpus a real measurement run accepts. Smoke fixtures may
/// shrink the corpus they build by calling the writer helpers directly;
/// [`HostedMeasure::build_initial_corpus`] enforces this floor.
pub const MIN_MEASURE_SENDS: usize = 5_000;

/// The name each hosted mailspace registers under (`[sync] name`).
pub const HOST_MEMBER: &str = "hub";

/// One hosted mailspace the fixture registered with the isolated server.
pub struct MeasureHost {
    pub name: String,
    pub root: PathBuf,
    pub project_id: String,
    pub member: String,
    /// The mailspace's own address prefix (`role@<prefix>.local`).
    pub prefix: String,
    /// The host's own `hand` address: the local recipient for corpus traffic.
    pub hand_address: String,
}

/// One off-host letter the mixed writer sent, carrying the identities a
/// later run needs to verify the letter where it landed (the sent-copy
/// handle here, the `to` and `kind` in the hosted transport index).
pub struct OffHostLetter {
    pub handle: String,
    pub to: String,
    pub kind: Option<String>,
    pub subject: String,
}

/// What one writer pass sent, classified as it went.
pub struct Traffic {
    /// Total sends through the mailspace: `local.len() + off_host.len()`.
    pub sends: usize,
    /// Sent-copy handles of the local-only letters.
    pub local: Vec<String>,
    pub off_host: Vec<OffHostLetter>,
    pub seconds: f64,
}

impl Traffic {
    /// Guard for real measurement runs: a corpus below the minimum is a
    /// fixture mistake, not a measurement.
    pub fn enforce_minimum(&self) {
        assert!(
            self.sends >= MIN_MEASURE_SENDS,
            "a measurement run needs at least {MIN_MEASURE_SENDS} sends, got {}",
            self.sends
        );
    }
}

/// The isolated fixture: a temp base holding a server data directory
/// (registry plus the hosted projects) and two hosted mailspaces, named
/// `before` and `after`. Both start empty; fill each with the writer
/// helpers and the same deterministic traffic.
pub struct HostedMeasure {
    pub base: PathBuf,
    pub data: PathBuf,
    pub projects: ProjectStore,
    pub identities: IdentityRegistry,
    pub hosts: Vec<MeasureHost>,
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "vivi-server-measure-process-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Deterministic subject and body for send `n` of a corpus.
fn letter_text(n: usize) -> (String, String) {
    (format!("letter {n}"), format!("letter {n} body\n"))
}

/// Send one letter through the normal API and return the sent-copy handle.
/// A `need` letter carries format-1 front matter and rides the `needs`
/// role; everything else is plain mail on the `inbox` role.
fn send_one(ms: &Mailspace, to: &str, need: bool, n: usize) -> String {
    let (subject, body) = letter_text(n);
    let body = if need {
        format!("+++\ndone_when = [\"{subject} is done\"]\n+++\n\n{body}")
    } else {
        body
    };
    let result = ms
        .send(SendRequest {
            from: "mind".into(),
            to: vec![to.into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject,
            body,
            role: if need { "needs" } else { "inbox" }.into(),
            kind: need.then(|| "need".to_owned()),
            reply_to: None,
        })
        .unwrap();
    result.sent
}

/// When the writer stops: after a letter count (a bootstrap corpus), or at
/// a deadline (a measurement window).
#[derive(Clone, Copy, Debug)]
enum Stop {
    After(usize),
    At(Instant),
}

impl Stop {
    /// Whether the writer has reached this stop yet.
    fn reached(self, sent: usize) -> bool {
        match self {
            Stop::After(count) => sent >= count,
            Stop::At(deadline) => Instant::now() >= deadline,
        }
    }
}

/// The shared writer loop: `sends` letters through the normal API, local
/// traffic to `to`, and — when a schedule is given — an off-host letter
/// when the pass starts and again each time the schedule comes due. The
/// off-host kinds alternate need, mail, need, ... by count, so the mix is
/// deterministic given the fire times.
fn write_traffic(
    ms: &Mailspace,
    to: &str,
    off_host_every: Option<Duration>,
    stop: Stop,
) -> Traffic {
    let start = Instant::now();
    let mut local = Vec::new();
    let mut off_host = Vec::new();
    let mut due = off_host_every.map(|_| Instant::now());
    while !stop.reached(local.len() + off_host.len()) {
        let fire = due.is_some_and(|due| Instant::now() >= due);
        if fire {
            let need = off_host.len() % 2 == 0;
            let n = local.len() + off_host.len();
            let handle = send_one(ms, OFF_HOST, need, n);
            let (subject, _) = letter_text(n);
            off_host.push(OffHostLetter {
                handle,
                to: OFF_HOST.into(),
                kind: need.then(|| "need".to_owned()),
                subject,
            });
            due = Some(Instant::now() + off_host_every.unwrap_or_default());
        } else {
            let n = local.len() + off_host.len();
            local.push(send_one(ms, to, false, n));
        }
    }
    Traffic {
        sends: local.len() + off_host.len(),
        local,
        off_host,
        seconds: start.elapsed().as_secs_f64(),
    }
}

/// Local-only traffic: every letter goes to `to` (a local identity of the
/// mailspace), so under the `addressed` send policy nothing leaves the
/// machine. Returns the sent-copy handles.
pub fn write_local_only(ms: &Mailspace, to: &str, sends: usize) -> Traffic {
    write_traffic(ms, to, None, Stop::After(sends))
}

/// Predominantly local traffic with periodic off-host letters: one off the
/// host when the pass starts, then one every `off_host_every` —
/// [`OFF_HOST_EVERY`] for a real run — with the rest local to `to`.
/// Returns the identities needed to verify the off-host letters later.
pub fn write_local_with_periodic_off_host(
    ms: &Mailspace,
    to: &str,
    sends: usize,
    off_host_every: Duration,
) -> Traffic {
    write_traffic(ms, to, Some(off_host_every), Stop::After(sends))
}

/// One measurement window's writes for `mode`, until `until`: nothing for
/// the idle mode, local-only letters for the local mode, and predominantly
/// local letters with periodic off-host letters on the corpus's own
/// schedule for the mixed mode. The traffic classifies as it goes, so the
/// caller can verify the off-host part after the window.
pub fn write_window(ms: &Mailspace, to: &str, mode: WindowMode, until: Instant) -> Traffic {
    match mode {
        WindowMode::Idle => Traffic {
            sends: 0,
            local: Vec::new(),
            off_host: Vec::new(),
            seconds: 0.0,
        },
        WindowMode::Local => write_traffic(ms, to, None, Stop::At(until)),
        WindowMode::Mixed => write_traffic(ms, to, Some(OFF_HOST_EVERY), Stop::At(until)),
    }
}

/// Actual rows in the mailspace's `messages` table: what the storage layer
/// holds, counted separately from the send count (each local delivery is a
/// row per recipient plus the sender's sent copy; an off-host letter is
/// its sent copy only).
pub fn message_rows(root: &Path) -> usize {
    let db = root.join(".vivi").join("mail.sqlite");
    let conn = Connection::open(&db).unwrap();
    let rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
        .unwrap();
    usize::try_from(rows).unwrap_or_default()
}

/// Every record the transport index serves `viewer`, paging through the
/// index's default page size (`ListQuery` has no raised limit). A large
/// corpus — 5,000 sends with a periodic off-host tail — spans pages.
pub fn read_all_records(
    projects: &ProjectStore,
    project_id: &str,
    viewer: &Viewer<'_>,
) -> Vec<ProjectRecord> {
    let mut all = Vec::new();
    let mut since = 0u64;
    loop {
        let page = projects
            .read_page(project_id, &ListQuery::since(since), viewer)
            .unwrap();
        let empty = page.records.is_empty();
        since = page.records.last().map_or(since, |record| record.sequence);
        all.extend(page.records);
        if empty {
            return all;
        }
    }
}

/// Corpus-enumeration work the old selector performs per window: every
/// push pass lists the mailspace's whole `messages` table through the
/// verified full-listing selection (`sync::push` over `sync::select`), so
/// its work scales with actual row counts. That makes the total
/// enumeration **derived evidence** — row counts times passes — never a
/// directly measured counter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CandidateWork {
    /// Push passes that completed inside the window; each is one full
    /// listing.
    pub passes: u64,
    /// Actual message rows the mailspace held when the window started
    /// (measured directly, then used only as this derivation's input).
    pub rows_per_pass: usize,
    /// `passes × rows_per_pass`: the derived row visits a window's passes
    /// enumerated.
    pub enumerated_rows: u128,
}

/// What one measurement window measured. The directly measured counters
/// come first; [`WindowSummary::candidate`] is deliberately separate, so a
/// reader can tell evidence from arithmetic.
#[derive(Clone, Debug)]
pub struct WindowSummary {
    /// The write mode the window carried.
    pub mode: WindowMode,
    /// Wall seconds the window spanned, first to last CPU sample.
    pub seconds: f64,
    /// Server CPU seconds integrated over the window's samples.
    pub cpu_seconds: f64,
    /// Average fraction of one core the server kept busy.
    pub average_cores: f64,
    /// Latencies, in seconds, of the push passes that completed inside the
    /// window: the server's own traced elapsed time per pass.
    pub pass_seconds: Vec<f64>,
    /// Actual message rows across both hosted mailspaces, at window start.
    pub rows_before: usize,
    /// Actual message rows across both hosted mailspaces, at window end.
    pub rows_after: usize,
    /// Letters the window's writers sent.
    pub writer_sends: usize,
    /// Wall seconds the writer threads spent sending.
    pub writer_seconds: f64,
    /// Writer threads the mode ran (zero for the idle mode).
    pub writer_threads: usize,
    /// Candidate selector work, derived — see [`CandidateWork`].
    pub candidate: CandidateWork,
}

impl WindowSummary {
    /// Summarize one window. `wall_seconds` must be positive and finite —
    /// the sampler's advancing window guarantees it — so `average_cores`
    /// and the candidate derivation are always defined.
    pub fn new(
        mode: WindowMode,
        cpu_seconds: f64,
        wall_seconds: f64,
        writer_sends: usize,
        writer_seconds: f64,
        writer_threads: usize,
        pass_seconds: Vec<f64>,
        rows_before: usize,
        rows_after: usize,
    ) -> Self {
        assert!(
            wall_seconds.is_finite() && wall_seconds > 0.0,
            "a window spans positive wall time, got {wall_seconds}"
        );
        let passes = u64::try_from(pass_seconds.len()).unwrap_or(u64::MAX);
        let candidate = CandidateWork {
            passes,
            rows_per_pass: rows_before,
            enumerated_rows: u128::from(passes) * u128::try_from(rows_before).unwrap_or(u128::MAX),
        };
        Self {
            mode,
            seconds: wall_seconds,
            cpu_seconds,
            average_cores: cpu_seconds / wall_seconds,
            pass_seconds,
            rows_before,
            rows_after,
            writer_sends,
            writer_seconds,
            writer_threads,
            candidate,
        }
    }

    /// Report lines: the measured counters first, then the candidate
    /// selector work, labeled as derivation so it is never mistaken for a
    /// measured counter.
    pub fn lines(&self) -> Vec<String> {
        let passes = self.pass_seconds.len();
        let slowest = self.pass_seconds.iter().cloned().fold(0.0, f64::max);
        vec![
            format!(
                "window {}: {:.3} s wall, {:.3} s server cpu, {:.4} core average",
                self.mode.name(),
                self.seconds,
                self.cpu_seconds,
                self.average_cores
            ),
            format!("passes: {passes} completed in-window, slowest {slowest:.3} s"),
            format!(
                "rows: {} before, {} after (both hosted mailspaces)",
                self.rows_before, self.rows_after
            ),
            format!(
                "writer: {} sends in {:.3} s across {} thread(s)",
                self.writer_sends, self.writer_seconds, self.writer_threads
            ),
            format!(
                "candidate (derived, not measured): {} full-listing passes x {} rows = {} row visits",
                self.candidate.passes, self.candidate.rows_per_pass, self.candidate.enumerated_rows
            ),
        ]
    }
}

/// The server's own push-pass latency log, parsed into window-bounded
/// latencies. `timed_push` in `src/server/projects/hosted_index.rs` emits
/// one debug line per pass; keep the marker and the `elapsed_ms` field in
/// step with it. The harness sees these lines only because the child's
/// stderr is drained into a bounded buffer, so a chatty server can never
/// wedge on a full pipe.
pub struct PassLog;

impl PassLog {
    /// The debug line `timed_push` emits per push pass.
    const MARKER: &str = "hosted index pass complete";

    /// The elapsed-time field of that line.
    const ELAPSED: &str = "elapsed_ms=";

    /// Latencies, in seconds, of the passes whose log line carries a
    /// timestamp inside `[start, end]` — the wall-clock window the runner
    /// recorded around its CPU samples, so warm-up and verification passes
    /// never count as measured latency. The server's subscriber styles its
    /// output with ANSI escapes; they are stripped before parsing, so a
    /// line reads the same however it was styled. A marker line that does
    /// not parse is a format drift, and drift fails loudly instead of
    /// silently dropping a pass from the report.
    pub fn in_window(log: &str, start: SystemTime, end: SystemTime) -> Vec<f64> {
        let start = chrono::DateTime::<chrono::Utc>::from(start);
        let end = chrono::DateTime::<chrono::Utc>::from(end);
        log.lines()
            .map(strip_ansi)
            .filter(|line| line.contains(Self::MARKER))
            .filter_map(|line| {
                let at = line.split_whitespace().next()?;
                let at = chrono::DateTime::parse_from_rfc3339(at)
                    .unwrap_or_else(|err| panic!("pass log timestamp {at:?}: {err}"))
                    .with_timezone(&chrono::Utc);
                let elapsed = line
                    .split(Self::ELAPSED)
                    .nth(1)
                    .unwrap_or_else(|| panic!("pass log line without elapsed_ms: {line}"));
                let digits: String = elapsed.chars().take_while(char::is_ascii_digit).collect();
                let ms: u64 = digits
                    .parse()
                    .unwrap_or_else(|err| panic!("pass log elapsed_ms {digits:?}: {err}"));
                (at >= start && at <= end).then(|| ms as f64 / 1_000.0)
            })
            .collect()
    }
}

/// Strip ANSI escape sequences (the CSI `ESC [ ... letter` form the
/// tracing subscriber styles its output with) so log parsing sees plain
/// text.
fn strip_ansi(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for next in chars.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

impl HostedMeasure {
    /// A fresh fixture: temp base, isolated server data directory, and two
    /// hosted mailspaces (`before` and `after`) registered by one
    /// `hosted::reconcile` pass. Each mailspace keeps `mind` and `hand`
    /// identities and the hub's `send = "addressed"` policy, and mints its
    /// own disposable sync key before registration.
    pub fn open(name: &str) -> Self {
        let base = scratch(name);
        let data = base.join("data");
        std::fs::create_dir_all(&data).unwrap();
        let projects = ProjectStore::new(&data);
        let identities = IdentityRegistry::new(&data);
        let mut roots = BTreeSet::new();
        let mut hands = Vec::new();
        for host_name in ["before", "after"] {
            let root = base.join(host_name);
            let mut mailspace = Mailspace::init(Some(&root)).unwrap();
            mailspace.add_identity("mind").unwrap();
            mailspace.add_identity("hand").unwrap();
            let config = mailspace.config_path();
            let mut text = std::fs::read_to_string(&config).unwrap();
            text.push_str("\n[sync]\n");
            text.push_str(&format!("name = \"{HOST_MEMBER}\"\n"));
            text.push_str("send = \"addressed\"\n");
            std::fs::write(&config, text).unwrap();
            // Disposable authentication material: this mailspace mints its
            // own sync key under its root, and reconcile admits it.
            SyncIdentity::ensure(&mailspace.dir).unwrap();
            hands.push((root.clone(), mailspace.address_for("hand")));
            roots.insert(root);
        }
        let report = hosted::reconcile(&projects, &identities, &roots);
        assert!(report.refused.is_empty(), "{report:?}");
        assert_eq!(report.served.len(), 2, "{report:?}");
        // Pair each served entry with its prepared root by path: the
        // served order follows the sorted root set, not the build order.
        let mut hosts = Vec::new();
        for served in &report.served {
            let root = &served.root;
            let hand_address = hands
                .iter()
                .find(|(prepared, _)| prepared == root)
                .map(|(_, hand)| hand.clone())
                .unwrap_or_else(|| panic!("served root {} was not prepared", root.display()));
            let mailspace = Mailspace::open_root(root.clone()).unwrap();
            hosts.push(MeasureHost {
                name: root
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default()
                    .to_owned(),
                root: root.clone(),
                project_id: served.project_id.clone(),
                member: served.member.clone(),
                prefix: mailspace.address_prefix().to_owned(),
                hand_address,
            });
        }
        hosts.sort_by(|a, b| a.name.cmp(&b.name));
        Self {
            base,
            data,
            projects,
            identities,
            hosts,
        }
    }

    /// The host named `name` (`before` or `after`).
    pub fn host(&self, name: &str) -> &MeasureHost {
        self.hosts
            .iter()
            .find(|host| host.name == name)
            .unwrap_or_else(|| panic!("no host named {name}"))
    }

    /// Open the host's mailspace for writing.
    pub fn mailspace(&self, host: &MeasureHost) -> Mailspace {
        Mailspace::open_root(host.root.clone()).unwrap()
    }

    /// Actual message rows in a host's mailspace.
    pub fn message_rows(&self, host: &MeasureHost) -> usize {
        message_rows(&host.root)
    }

    /// The deterministic initial corpus for real measurement runs: each
    /// host receives the same predominantly local traffic with periodic
    /// off-host letters (one at start, then one every [`OFF_HOST_EVERY`]),
    /// built by the same writer in the same order, so a before run and an
    /// after run start from the same shape. Returns each host's traffic
    /// classification with its actual message-row count. `sends` must be
    /// at least [`MIN_MEASURE_SENDS`]; the guard fires before anything is
    /// sent.
    pub fn build_initial_corpus(&self, sends: usize) -> Vec<(Traffic, usize)> {
        assert!(
            sends >= MIN_MEASURE_SENDS,
            "measurement runs enforce the {MIN_MEASURE_SENDS}-send minimum; \
             only a smoke fixture may shrink the corpus"
        );
        self.hosts
            .iter()
            .map(|host| {
                let mailspace = self.mailspace(host);
                let traffic = write_local_with_periodic_off_host(
                    &mailspace,
                    &host.hand_address,
                    sends,
                    OFF_HOST_EVERY,
                );
                traffic.enforce_minimum();
                (traffic, message_rows(&host.root))
            })
            .collect()
    }
}
