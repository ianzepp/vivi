//! Reusable fixture for the hosted-index measurement ([`super`]): the hub
//! mailspace with the default letter mix, the steady-state scenario
//! constructors, and count readers for what the mailspace actually holds.
//!
//! Scenario constructors guarantee what their names say by construction:
//! the graph-only and event-recorded scenarios re-read the message counts
//! after their setup and panic if a letter appeared, so a fixture change can
//! never silently turn a no-send scenario into a send scenario.

use std::path::PathBuf;

use vivi::mailspace::{Mailspace, SendRequest};
use vivi::storage::MailspaceEventInput;

/// The off-host address every traveling letter is addressed to.
pub const OFF_HOST: &str = "mind@0badc0de.local";

/// The default corpus size: a quarter off-host needs, a quarter off-host
/// mail, and half local-only needs.
pub const DEFAULT_LETTERS: usize = 5000;

/// `VIVI_MEASURE_LETTERS` overrides the letter count.
pub fn letters_from_env() -> usize {
    std::env::var("VIVI_MEASURE_LETTERS")
        .ok()
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(DEFAULT_LETTERS)
}

/// How much of the mix is addressed off the host (the share a bootstrap
/// pass indexes): needs at `n % 4 == 0` plus mail at `n % 4 == 1`.
pub fn off_host_share(letters: usize) -> usize {
    (0..letters).filter(|n| n % 4 < 2).count()
}

/// Send one letter as the measurement mix does: needs carry format-1 front
/// matter (`done_when`), plain mail does not. `kind = Some("need")` files
/// the letter under the `needs` role, everything else under `inbox`.
pub fn send(ms: &Mailspace, to: &str, kind: Option<&str>, n: usize) {
    ms.send(SendRequest {
        from: "mind".into(),
        to: vec![to.into()],
        cc: Vec::new(),
        bcc: Vec::new(),
        subject: format!("letter {n}"),
        body: if kind == Some("need") {
            // A format-1 need letter: front matter with done_when, then prose.
            format!("+++\ndone_when = [\"letter {n} is done\"]\n+++\n\nletter {n} body\n")
        } else {
            format!("letter {n} body\n")
        },
        role: if kind == Some("need") {
            "needs"
        } else {
            "inbox"
        }
        .into(),
        kind: kind.map(str::to_owned),
        reply_to: None,
    })
    .unwrap();
}

/// The default mix: one quarter off-host needs, one quarter off-host mail,
/// and half local-only needs to the host's own `hand` (local only under
/// `send = "addressed"`).
pub fn send_mix(hub: &Hub, letters: usize) {
    for n in 0..letters {
        match n % 4 {
            0 => send(&hub.ms, OFF_HOST, Some("need"), n),
            1 => send(&hub.ms, OFF_HOST, None, n),
            _ => send(&hub.ms, &hub.own_hand, Some("need"), n),
        }
    }
}

/// A scratch directory removed again on drop, panics included.
pub struct Scratch {
    pub path: PathBuf,
}

impl Scratch {
    pub fn new(name: &str) -> Scratch {
        let path = std::env::temp_dir().join(format!("vivi-server-measure-{name}"));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Scratch { path }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// The hub mailspace: identities `mind` and `hand`, `send = "addressed"`,
/// so only letters addressed off the host travel and a hosted-index pass
/// never picks the local half up.
pub struct Hub {
    pub scratch: Scratch,
    /// The mailspace root (the hosted project's mailspace root).
    pub root: PathBuf,
    pub ms: Mailspace,
    /// The host's own `hand` address: local-only letters go here.
    pub own_hand: String,
}

impl Hub {
    pub fn init(scratch: &Scratch) -> Hub {
        let root = scratch.path.join("host");
        let mut ms = Mailspace::init(Some(&root)).unwrap();
        ms.add_identity("mind").unwrap();
        ms.add_identity("hand").unwrap();
        let path = ms.config_path();
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("\n[sync]\nname = \"hub\"\nsend = \"addressed\"\n");
        std::fs::write(&path, text).unwrap();
        let ms = Mailspace::open_root(root.clone()).unwrap();
        let own_hand = ms.address_for("hand");
        Hub {
            scratch: Scratch {
                path: scratch.path.clone(),
            },
            root,
            ms,
            own_hand,
        }
    }

    /// The server data dir beside the host mailspace.
    pub fn data_dir(&self) -> PathBuf {
        self.scratch.path.join("data")
    }
}

/// What the mailspace store actually holds. `contents` counts distinct
/// letters (a local send holds a sent copy and a delivered copy of the same
/// content); `events` and `graph_events` count the two event logs the
/// change watermark covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MailCounts {
    pub messages: i64,
    pub contents: i64,
    pub events: i64,
    pub graph_events: i64,
}

impl MailCounts {
    /// Per-field difference `after - before`, for scenario reporting.
    pub fn delta(after: MailCounts, before: MailCounts) -> MailCounts {
        MailCounts {
            messages: after.messages - before.messages,
            contents: after.contents - before.contents,
            events: after.events - before.events,
            graph_events: after.graph_events - before.graph_events,
        }
    }
}

impl std::fmt::Display for MailCounts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "messages={} contents={} events={} graph_events={}",
            self.messages, self.contents, self.events, self.graph_events
        )
    }
}

/// Read [`MailCounts`] straight from the mailspace store.
pub fn read_counts(ms: &Mailspace) -> MailCounts {
    let conn = rusqlite::Connection::open(ms.store_path()).unwrap();
    let scalar = |sql: &str| -> i64 { conn.query_row(sql, [], |row| row.get(0)).unwrap() };
    MailCounts {
        messages: scalar("SELECT COUNT(*) FROM messages"),
        contents: scalar("SELECT COUNT(DISTINCT content_id) FROM messages"),
        events: scalar("SELECT COUNT(*) FROM mailspace_events"),
        graph_events: scalar("SELECT COUNT(*) FROM work_graph_events"),
    }
}

/// Panic unless exactly the given number of message rows and distinct
/// contents were added between the two reads. Used to keep the no-send
/// scenarios honest.
fn assert_message_delta(
    before: MailCounts,
    after: MailCounts,
    messages: i64,
    contents: i64,
    label: &str,
) {
    let delta = MailCounts::delta(after, before);
    assert_eq!(delta.messages, messages, "{label}: unexpected message rows");
    assert_eq!(
        delta.contents, contents,
        "{label}: unexpected distinct contents"
    );
}

/// Scenario: one new off-host letter (a need) moves the watermark and gives
/// the pass exactly one letter to index.
pub fn scenario_one_off_host_addition(hub: &Hub, ordinal: usize) {
    let before = read_counts(&hub.ms);
    send(&hub.ms, OFF_HOST, Some("need"), ordinal);
    assert_message_delta(before, read_counts(&hub.ms), 1, 1, "one off-host addition");
}

/// Scenario: `count` more local-only needs. They move the message watermark
/// (so the pass runs) but never travel, so nothing new is indexed.
pub fn scenario_local_only_additions(hub: &Hub, first_ordinal: usize, count: usize) {
    let before = read_counts(&hub.ms);
    for n in first_ordinal..first_ordinal + count {
        send(&hub.ms, &hub.own_hand, Some("need"), n);
    }
    assert_message_delta(
        before,
        read_counts(&hub.ms),
        2 * count as i64,
        count as i64,
        "local-only additions",
    );
}

/// Scenario: import a small work graph. Graph events move the watermark
/// without any letter being sent, so the pass must run a full scan and find
/// nothing to index. Panics if the import sent a letter.
pub fn scenario_graph_only(hub: &Hub) {
    let before = read_counts(&hub.ms);
    let mermaid = "flowchart TD\na[\"measure a\"]\nb[\"measure b\"]\na --> b\n";
    let report = hub
        .ms
        .graph_import("measure-scenario", mermaid, false)
        .unwrap();
    assert!(report.created, "graph-only scenario: import not created");
    assert_message_delta(before, read_counts(&hub.ms), 0, 0, "graph-only activity");
}

/// Scenario: append one item event (a note on the newest sent copy). The
/// event log moves the watermark without any letter being sent. Panics if
/// the setup sent a letter.
pub fn scenario_event_recorded(hub: &Hub) {
    let before = read_counts(&hub.ms);
    let storage = hub.ms.storage().unwrap();
    let newest = storage
        .list_messages_by_role("sent")
        .unwrap()
        .pop()
        .expect("event-recorded scenario: no sent copy");
    let id = storage
        .append_mailspace_event(&MailspaceEventInput {
            command: "mail note".into(),
            event_type: "note".into(),
            actor_identity: Some("mind".into()),
            account: newest.account.clone(),
            message_id: newest.message_id.clone(),
            content_id: newest.content_id.clone(),
            from_role: None,
            to_role: None,
            from_identity: Some("mind".into()),
            to_identity: Some("mind".into()),
            subject: newest.subject.clone(),
            note: Some("measurement scenario: event-recorded change".into()),
        })
        .unwrap();
    assert!(id > 0, "event-recorded scenario: no event row");
    assert_message_delta(before, read_counts(&hub.ms), 0, 0, "event-recorded change");
}
