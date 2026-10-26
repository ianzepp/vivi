#![allow(dead_code)]
//! Disposable hosted-process measurement fixtures: two isolated hosted
//! mailspaces registered with an isolated server data directory, plus
//! normal-API writer helpers whose traffic is classifiable (local-only,
//! and predominantly local with periodic off-host letters).
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
use std::time::{Duration, Instant};

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

/// The shared writer loop: `sends` letters through the normal API, local
/// traffic to `to`, and — when a schedule is given — an off-host letter
/// when the pass starts and again each time the schedule comes due. The
/// off-host kinds alternate need, mail, need, ... by count, so the mix is
/// deterministic given the fire times.
fn write_traffic(
    ms: &Mailspace,
    to: &str,
    sends: usize,
    off_host_every: Option<Duration>,
) -> Traffic {
    let start = Instant::now();
    let mut local = Vec::new();
    let mut off_host = Vec::new();
    let mut due = off_host_every.map(|_| Instant::now());
    while local.len() + off_host.len() < sends {
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
    write_traffic(ms, to, sends, None)
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
    write_traffic(ms, to, sends, Some(off_host_every))
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
