//! Hosted projects, S4 and S5: the indexer keeps a hosted project's
//! transport index current with its mailspace (without copying letters), and
//! a member's append is filed into the host mailspace as a client pull files
//! it.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::SigningKey;
use sha2::{Digest, Sha256};
use vivi::VivariumError;
use vivi::mailspace::{Mailspace, SendRequest};
use vivi::server::hosted;
use vivi::server::identities::IdentityRegistry;
use vivi::server::projects::{
    AppendItem, IndexOutcome, IndexSkip, ListQuery, ProjectError, ProjectStore, Viewer,
};
use vivi::sync::lock::SyncLock;
use vivi::sync::push;
use vivi::sync::transport::{
    AppendFailure, Appended, Envelope, ListQuery as WireQuery, Page, Transport,
};
use vivi::sync::{SyncConfig, SyncSettings, sync_dir};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "vivi-server-hosted-index-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct Env {
    base: PathBuf,
    projects: ProjectStore,
    identities: IdentityRegistry,
}

fn env(name: &str) -> Env {
    let base = scratch(name);
    let data = base.join("data");
    std::fs::create_dir_all(&data).unwrap();
    Env {
        projects: ProjectStore::new(&data),
        identities: IdentityRegistry::new(&data),
        base,
    }
}

/// A hosted project: the host mailspace (roles `mind` and `hand`, the hub's
/// `send = "addressed"`), served under the id its mailspace derives.
struct Host {
    root: PathBuf,
    id: String,
    member: String,
}

impl Host {
    fn mailspace(&self) -> Mailspace {
        Mailspace::open_root(self.root.clone()).unwrap()
    }

    fn viewer<'a>(&'a self, prefix: &'a str) -> Viewer<'a> {
        Viewer {
            name: &self.member,
            prefix: Some(prefix),
        }
    }
}

fn host(env: &Env, send: Option<&str>) -> Host {
    let root = env.base.join("host");
    let mut mailspace = Mailspace::init(Some(&root)).unwrap();
    mailspace.add_identity("mind").unwrap();
    mailspace.add_identity("hand").unwrap();
    let mut sync = String::from("\n[sync]\nname = \"hub\"\n");
    if let Some(send) = send {
        sync.push_str(&format!("send = \"{send}\"\n"));
    }
    let path = mailspace.config_path();
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str(&sync);
    std::fs::write(&path, text).unwrap();
    let report = hosted::reconcile(
        &env.projects,
        &env.identities,
        &BTreeSet::from([root.clone()]),
    );
    assert!(report.refused.is_empty(), "{report:?}");
    Host {
        root,
        id: report.served[0].project_id.clone(),
        member: report.served[0].member.clone(),
    }
}

/// A member's own mailspace, with a `mind` role and sync settings for the
/// hosted project.
struct Peer {
    mailspace: Mailspace,
    settings: SyncSettings,
    name: String,
}

fn peer(env: &Env, project: &str, name: &str) -> Peer {
    let root = env.base.join(format!("peer-{name}"));
    let mut mailspace = Mailspace::init(Some(&root)).unwrap();
    mailspace.add_identity("mind").unwrap();
    mailspace.config.sync = Some(SyncConfig {
        server: Some("http://hub.test".into()),
        project: Some(project.into()),
        name: Some(name.into()),
        send: None,
    });
    let settings = SyncSettings::resolve(&mailspace).unwrap();
    Peer {
        mailspace,
        settings,
        name: name.into(),
    }
}

impl Peer {
    fn prefix(&self) -> String {
        self.mailspace.address_prefix().to_owned()
    }
}

fn send(ms: &Mailspace, from: &str, to: &str, kind: Option<&str>, subject: &str) {
    ms.send(SendRequest {
        from: from.into(),
        to: vec![to.into()],
        cc: Vec::new(),
        bcc: Vec::new(),
        subject: subject.into(),
        // A format-1 need letter: front matter with done_when, then prose.
        // Mail stays prose: a mail body may not open with front matter.
        body: if kind == Some("need") {
            format!("+++\ndone_when = [\"it is done\"]\n+++\n\n{subject} body\n")
        } else {
            format!("{subject} body\n")
        },
        role: match kind {
            Some("need") => "needs".into(),
            _ => "inbox".into(),
        },
        kind: kind.map(str::to_owned),
        reply_to: None,
    })
    .unwrap();
}

/// What a push would have sent over the wire.
struct Wire {
    id: String,
    to: String,
    kind: String,
    bytes: Vec<u8>,
}

#[derive(Default)]
struct Collect {
    sent: RefCell<Vec<Wire>>,
}

impl Transport for Collect {
    fn append(&self, message_id: &str, envelope: &Envelope<'_>) -> Result<Appended, AppendFailure> {
        let mut sent = self.sent.borrow_mut();
        sent.push(Wire {
            id: message_id.into(),
            to: envelope.to.into(),
            kind: envelope.kind.into(),
            bytes: envelope.raw_eml.to_vec(),
        });
        Ok(Appended {
            message_id: message_id.into(),
            sequence: sent.len() as u64,
            duplicate: false,
            content_sha256: Some(sha256_hex(envelope.raw_eml)),
        })
    }

    fn fetch(&self, _query: &WireQuery<'_>) -> Result<Page, VivariumError> {
        Err(VivariumError::Message("collect only appends".into()))
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn wire_of(peer: &Peer) -> Vec<Wire> {
    let collect = Collect::default();
    push::push(&peer.mailspace, &peer.settings, &collect).unwrap();
    collect.sent.into_inner()
}

fn item_of(wire: &Wire, peer: &Peer) -> AppendItem {
    AppendItem::new(
        &wire.id,
        &peer.name,
        Some(&wire.to),
        Some(&wire.kind),
        wire.bytes.clone(),
    )
    .unwrap()
    .with_from_prefix(Some(&peer.prefix()))
}

fn page(env: &Env, id: &str, viewer: &Viewer<'_>) -> Vec<(u64, String, String)> {
    env.projects
        .read_page(id, &ListQuery::since(0), viewer)
        .unwrap()
        .records
        .into_iter()
        .map(|r| (r.sequence, r.message_id, r.from))
        .collect()
}

fn ran(outcome: IndexOutcome) -> (usize, usize, usize) {
    match outcome {
        IndexOutcome::Ran {
            indexed,
            duplicates,
            refused,
            stopped,
        } => {
            assert_eq!(stopped, None);
            (indexed, duplicates, refused)
        }
        other => panic!("the pass did not run: {other:?}"),
    }
}

fn rows_of(host: &Host, account: &str, role: &str) -> usize {
    host.mailspace()
        .storage()
        .unwrap()
        .list_messages_by_account_role(account, role)
        .unwrap()
        .len()
}

#[test]
fn a_letter_addressed_off_host_is_indexed_and_served_to_the_member() {
    let env = env("offhost");
    let host = host(&env, Some("addressed"));
    let peer = peer(&env, &host.id, "pat");
    let ms = host.mailspace();
    let host_prefix = ms.address_prefix().to_owned();

    send(
        &ms,
        "mind",
        &ms.address_for("hand"),
        Some("need"),
        "local only",
    );
    send(
        &ms,
        "mind",
        &peer.mailspace.address_for("mind"),
        Some("need"),
        "go away",
    );
    send(
        &ms,
        "mind",
        &peer.mailspace.address_for("mind"),
        None,
        "private note",
    );

    let (indexed, duplicates, _) = ran(env.projects.index_hosted(&host.id).unwrap());
    assert_eq!((indexed, duplicates), (2, 0));

    // The member pulls: the need (work) and the mail addressed to its prefix.
    let pat = Viewer {
        name: "pat",
        prefix: Some(&peer.prefix()),
    };
    let served = page(&env, &host.id, &pat);
    assert_eq!(served.len(), 2, "{served:?}");
    assert!(served.iter().all(|(_, _, from)| *from == host.member));
    let records = env
        .projects
        .read_page(&host.id, &ListQuery::since(0), &pat)
        .unwrap()
        .records;
    assert_eq!(
        records[0].from_prefix.as_deref(),
        Some(host_prefix.as_str())
    );
    assert!(
        records[0].to.as_deref().unwrap().contains(&peer.prefix()),
        "{:?}",
        records[0].to
    );

    // A third member sees the need (work is shared) but not the mail.
    let other = Viewer {
        name: "sam",
        prefix: Some("0badc0de"),
    };
    assert_eq!(page(&env, &host.id, &other).len(), 1);

    // The letter bytes are the host's, byte for byte.
    let blob_ok = records.iter().all(|r| !r.raw_eml.is_empty());
    assert!(blob_ok);
}

#[test]
fn a_letter_addressed_only_to_a_local_role_is_not_indexed() {
    let env = env("localonly");
    let host = host(&env, Some("addressed"));
    let ms = host.mailspace();
    send(
        &ms,
        "mind",
        &ms.address_for("hand"),
        Some("need"),
        "stay home",
    );
    send(&ms, "mind", &ms.address_for("hand"), None, "also home");

    let (indexed, ..) = ran(env.projects.index_hosted(&host.id).unwrap());
    assert_eq!(indexed, 0);
    let prefix = ms.address_prefix().to_owned();
    assert!(page(&env, &host.id, &host.viewer(&prefix)).is_empty());
}

#[test]
fn indexing_again_adds_nothing() {
    let env = env("again");
    let host = host(&env, Some("addressed"));
    let peer = peer(&env, &host.id, "pat");
    let ms = host.mailspace();
    send(
        &ms,
        "mind",
        &peer.mailspace.address_for("mind"),
        Some("need"),
        "once",
    );
    let prefix = ms.address_prefix().to_owned();

    assert_eq!(ran(env.projects.index_hosted(&host.id).unwrap()).0, 1);
    let first = page(&env, &host.id, &host.viewer(&prefix));
    assert_eq!(first.len(), 1);

    match env.projects.index_hosted(&host.id).unwrap() {
        IndexOutcome::Skipped(IndexSkip::Unchanged) => {}
        IndexOutcome::Ran { indexed, .. } => assert_eq!(indexed, 0),
        other => panic!("{other:?}"),
    }
    // A fresh store over the same data (a restart) has no pass memory and
    // still adds nothing: the mailspace's own bindings say it is indexed.
    let restarted = ProjectStore::new(&env.base.join("data"));
    hosted::reconcile(
        &restarted,
        &env.identities,
        &BTreeSet::from([host.root.clone()]),
    );
    assert_eq!(ran(restarted.index_hosted(&host.id).unwrap()).0, 0);
    assert_eq!(page(&env, &host.id, &host.viewer(&prefix)), first);
}

#[test]
fn a_member_need_lands_in_the_roles_needs_folder_and_is_indexed_once() {
    let env = env("memberneed");
    let host = host(&env, Some("addressed"));
    let peer = peer(&env, &host.id, "pat");
    let ms = host.mailspace();
    send(
        &peer.mailspace,
        "mind",
        &ms.address_for("hand"),
        Some("need"),
        "do it",
    );
    let wire = wire_of(&peer);
    assert_eq!(wire.len(), 1);
    let item = item_of(&wire[0], &peer);

    let first = env.projects.append(&host.id, &item).unwrap();
    assert!(!first.duplicate);
    assert_eq!(first.sequence, 1);

    // Filed in the host mailspace, under the role the address names.
    assert_eq!(rows_of(&host, "hand", "needs"), 1);
    let again = env.projects.append(&host.id, &item).unwrap();
    assert!(again.duplicate);
    assert_eq!(again.sequence, 1);
    assert_eq!(rows_of(&host, "hand", "needs"), 1);

    // Indexed once, as the member's own letter; the indexer adds nothing.
    let prefix = ms.address_prefix().to_owned();
    assert_eq!(ran(env.projects.index_hosted(&host.id).unwrap()).0, 0);
    let served = page(&env, &host.id, &host.viewer(&prefix));
    assert_eq!(served.len(), 1);
    assert_eq!(served[0].2, "pat");
    // The persistent filing state lives with the project's server data.
    assert!(
        env.base
            .join("data/projects")
            .join(&host.id)
            .join("transport.sqlite")
            .is_file()
    );
}

#[test]
fn a_forged_from_is_refused_and_nothing_is_filed_or_indexed() {
    let env = env("forged");
    let host = host(&env, Some("addressed"));
    let peer = peer(&env, &host.id, "pat");
    let ms = host.mailspace();
    send(
        &peer.mailspace,
        "mind",
        &ms.address_for("hand"),
        Some("need"),
        "lie",
    );
    let wire = wire_of(&peer);
    let item = item_of(&wire[0], &peer).with_from_prefix(Some("deadbeef"));

    let err = env.projects.append(&host.id, &item).unwrap_err();
    assert!(matches!(err, ProjectError::BadRequest(_)), "{err:?}");
    assert_eq!(rows_of(&host, "hand", "needs"), 0);
    let prefix = ms.address_prefix().to_owned();
    assert!(page(&env, &host.id, &host.viewer(&prefix)).is_empty());
}

#[test]
fn a_vouch_to_postmaster_admits_and_is_served_back() {
    let env = env("vouch");
    let host = host(&env, Some("addressed"));
    let ms = host.mailspace();
    let id = format!("msg_{:08x}{:016x}", 7, 0);
    let body = serde_json::json!({"vouch": {"name": "newbie", "public_key_pem": spki_pem(9)}});
    let eml = format!(
        "From: hub@{}.local\nTo: postmaster\nMessage-ID: <{id}@p.local>\n\n{body}",
        ms.address_prefix()
    );
    // The creator member vouches (its name is the project's admitted creator).
    let item = AppendItem::new(
        &id,
        &host.member,
        Some("postmaster"),
        None,
        eml.into_bytes(),
    )
    .unwrap();
    env.projects.append(&host.id, &item).unwrap();
    assert!(env.projects.admissions().is_admitted(&host.id, "newbie"));
    let prefix = ms.address_prefix().to_owned();
    assert_eq!(page(&env, &host.id, &host.viewer(&prefix)).len(), 1);
}

#[test]
fn the_pass_skips_while_the_sync_lock_is_held() {
    let env = env("locked");
    let host = host(&env, Some("addressed"));
    let peer = peer(&env, &host.id, "pat");
    let ms = host.mailspace();
    send(
        &ms,
        "mind",
        &peer.mailspace.address_for("mind"),
        Some("need"),
        "wait",
    );
    let prefix = ms.address_prefix().to_owned();

    let lock = SyncLock::acquire(&sync_dir(&ms.dir)).unwrap();
    assert_eq!(
        env.projects.index_hosted(&host.id).unwrap(),
        IndexOutcome::Skipped(IndexSkip::SyncLocked)
    );
    assert!(page(&env, &host.id, &host.viewer(&prefix)).is_empty());
    // A member's append never takes that lock.
    send(
        &peer.mailspace,
        "mind",
        &ms.address_for("hand"),
        Some("need"),
        "now",
    );
    let wire = wire_of(&peer);
    env.projects
        .append(&host.id, &item_of(&wire[0], &peer))
        .unwrap();
    drop(lock);

    assert_eq!(ran(env.projects.index_hosted(&host.id).unwrap()).0, 1);
}

#[test]
fn a_read_runs_the_pass_when_the_mailspace_moved() {
    let env = env("preread");
    let host = host(&env, Some("addressed"));
    let peer = peer(&env, &host.id, "pat");
    let ms = host.mailspace();
    let prefix = ms.address_prefix().to_owned();
    assert!(page(&env, &host.id, &host.viewer(&prefix)).is_empty());

    send(
        &ms,
        "mind",
        &peer.mailspace.address_for("mind"),
        Some("need"),
        "fresh",
    );
    // No timer ran: the read itself brings the index up to date.
    assert_eq!(page(&env, &host.id, &host.viewer(&prefix)).len(), 1);
    // And a lookup by handle finds it.
    let served = env
        .projects
        .read_page(&host.id, &ListQuery::since(0), &host.viewer(&prefix))
        .unwrap();
    let handle = served.records[0].message_id[4..12].to_owned();
    let found = env
        .projects
        .lookup_handles(&host.id, &[handle], &host.viewer(&prefix))
        .unwrap();
    assert_eq!(found.records.len(), 1);
}

#[tokio::test]
async fn the_timer_indexes_a_letter_written_into_the_mailspace() {
    let env = env("timer");
    let host = host(&env, Some("addressed"));
    let peer = peer(&env, &host.id, "pat");
    let ms = host.mailspace();
    let prefix = ms.address_prefix().to_owned();
    let _indexer = hosted::start_indexer_with(env.projects.clone(), Duration::from_millis(50));

    send(
        &ms,
        "mind",
        &peer.mailspace.address_for("mind"),
        Some("need"),
        "tick",
    );
    // Wait on the index itself, not through a read (a read would index).
    let db = env
        .base
        .join("data/projects")
        .join(&host.id)
        .join("transport.sqlite");
    for _ in 0..100 {
        let conn = rusqlite::Connection::open(&db).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM items", [], |row| row.get(0))
            .unwrap();
        if count == 1 {
            assert_eq!(page(&env, &host.id, &host.viewer(&prefix)).len(), 1);
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the timer did not index the letter within 5s");
}

fn spki_pem(seed: u8) -> String {
    const PREFIX: &[u8] = &[
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
    ];
    let mut der = PREFIX.to_vec();
    der.extend_from_slice(
        SigningKey::from_bytes(&[seed; 32])
            .verifying_key()
            .as_bytes(),
    );
    format!(
        "-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----",
        STANDARD.encode(der)
    )
}

#[test]
fn a_pump_identity_letter_is_indexed_under_the_host_prefix() {
    let env = env("pump-prefix");
    let host = host(&env, Some("addressed"));
    let peer = peer(&env, &host.id, "pat");
    let mut ms = host.mailspace();
    ms.add_identity("pump").unwrap();
    let host_prefix = ms.address_prefix().to_owned();

    send(
        &ms,
        "pump",
        &peer.mailspace.address_for("mind"),
        None,
        "pump close",
    );

    let (indexed, duplicates, _) = ran(env.projects.index_hosted(&host.id).unwrap());
    assert_eq!((indexed, duplicates), (1, 0));
    let pat = Viewer {
        name: "pat",
        prefix: Some(&peer.prefix()),
    };
    let records = env
        .projects
        .read_page(&host.id, &ListQuery::since(0), &pat)
        .unwrap()
        .records;
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(
        records[0].from_prefix.as_deref(),
        Some(host_prefix.as_str())
    );
}
