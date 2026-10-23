//! Dependency edges converge across mailspaces: the prerequisites of an
//! item are a function of its front-matter `depends_on` and its
//! `dependency-added` / `dependency-removed` letters alone. Per dependency
//! the newest letter by `(X-Vivi-Event-At, Message-ID)` wins, over the
//! header too, so members that pull the same letters land the same edges
//! whatever order the letters arrive in.

use std::collections::BTreeSet;
use std::rc::Rc;

use super::fake_server::{Checkout, FakeServer, checkout, wire_id};
use crate::letter::fixture::body_with_deps;
use crate::mailspace::{Mailspace, RoleUpdate, SendRequest};
use crate::sync::transport::{Envelope, Transport};

/// A member whose `mind` seat is of kind `mind`, so its dependency letters
/// count for every member.
fn member(name: &str) -> Checkout {
    let mut checkout = checkout(name, &["mind", "hand"]);
    checkout
        .mailspace
        .set_role(
            "mind",
            RoleUpdate {
                kind: Some(Some("mind".into())),
                ..RoleUpdate::default()
            },
        )
        .unwrap();
    checkout
}

/// The author, two holders, and a fresh member who pulls only when asked.
struct World {
    server: Rc<FakeServer>,
    a: Checkout,
    b: Checkout,
    c: Checkout,
    d: Checkout,
}

fn world() -> World {
    World {
        server: FakeServer::new(),
        a: member("alice"),
        b: member("bob"),
        c: member("carol"),
        d: member("dave"),
    }
}

impl World {
    fn to(&self) -> Vec<String> {
        vec![
            self.b.address("mind"),
            self.c.address("mind"),
            self.d.address("mind"),
        ]
    }

    fn push(&self) {
        self.a.push(&self.a.client(&self.server));
    }

    fn pull(&self, who: &Checkout) {
        who.pull(&who.client(&self.server));
    }

    /// Alice sends `subject` as a need to every holder.
    fn send(&self, subject: &str, depends_on: &[&str]) -> String {
        let result = self
            .a
            .mailspace
            .send(SendRequest {
                from: "mind".into(),
                to: self.to(),
                cc: Vec::new(),
                bcc: Vec::new(),
                subject: subject.into(),
                body: body_with_deps(
                    "need",
                    "it works\n",
                    &depends_on
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>(),
                ),
                role: "needs".into(),
                kind: Some("need".into()),
                reply_to: None,
            })
            .unwrap();
        result
            .delivered
            .first()
            .map_or(result.sent, |delivered| delivered.handle.clone())
    }

    /// The three items: a release that was sent depending on `x`, then `x`
    /// and `y`. Alice has pushed; bob and carol have pulled.
    fn release(&self) -> (String, String, String) {
        let x = self.send("work x", &[]);
        let y = self.send("work y", &[]);
        let release = self.send("release 1.0", &[&x]);
        self.push();
        self.pull(&self.b);
        self.pull(&self.c);
        (release, x, y)
    }
}

fn depend(who: &Checkout, release: &str, add: &[&str], remove: &[&str]) {
    who.mailspace
        .set_dependencies(
            "mind",
            release,
            "need",
            &add.iter().map(ToString::to_string).collect::<Vec<_>>(),
            &remove.iter().map(ToString::to_string).collect::<Vec<_>>(),
        )
        .unwrap();
}

/// The content ids of the prerequisites of `release` on `who`'s graph.
fn deps_of(who: &Checkout, release: &str) -> BTreeSet<String> {
    who.mailspace
        .backlog_dependencies(release)
        .unwrap()
        .into_iter()
        .map(|dep| content_of(who, &dep.handle))
        .collect()
}

fn content_of(who: &Checkout, handle: &str) -> String {
    who.row(&who.id_of(handle)).unwrap().content_id
}

fn contents(who: &Checkout, handles: &[&String]) -> BTreeSet<String> {
    handles.iter().map(|h| content_of(who, h)).collect()
}

fn readiness(mailspace: &Mailspace, handle: &str) -> String {
    let storage = mailspace.storage().unwrap();
    let source = storage.resolve_message_token(handle).unwrap();
    let handle = storage.display_handle(&source).unwrap();
    drop(storage);
    mailspace
        .graph_show("backlog")
        .unwrap()
        .nodes
        .into_iter()
        .find(|node| handle.starts_with(&node.source_id) || node.source_id.starts_with(&handle))
        .unwrap_or_else(|| panic!("no node for {handle}"))
        .readiness
}

fn header_values(bytes: &[u8], name: &str) -> Vec<String> {
    let Some(parsed) = mail_parser::MessageParser::default().parse(bytes) else {
        return Vec::new();
    };
    parsed
        .headers()
        .iter()
        .filter(|header| header.name().eq_ignore_ascii_case(name))
        .filter_map(|header| match header.value() {
            mail_parser::HeaderValue::Text(text) => Some(text.trim().to_string()),
            _ => None,
        })
        .collect()
}

/// The dependency letters `who` holds, oldest first by `X-Vivi-Event-At`.
fn dependency_letters(who: &Checkout) -> Vec<Vec<u8>> {
    let mut seen = BTreeSet::new();
    let mut letters: Vec<(String, Vec<u8>)> = who
        .rows()
        .into_iter()
        .map(|row| who.bytes_of(&row.message_id))
        .filter(|bytes| {
            header_values(bytes, "X-Vivi-Event")
                .iter()
                .any(|kind| kind.starts_with("dependency-"))
        })
        .filter(|bytes| seen.insert(header_values(bytes, "Message-ID")))
        .map(|bytes| {
            let at = header_values(&bytes, "X-Vivi-Event-At")
                .into_iter()
                .next()
                .unwrap_or_default();
            (at, bytes)
        })
        .collect();
    letters.sort_by(|a, b| a.0.cmp(&b.0));
    letters.into_iter().map(|(_, bytes)| bytes).collect()
}

/// Put one letter on the wire by hand, ahead of the push that would carry
/// the rest.
fn append(w: &World, n: u64, bytes: &[u8]) {
    let to = w.to().join(", ");
    w.a.client(&w.server)
        .append(
            &wire_id(n, 1),
            &Envelope {
                raw_eml: bytes,
                to: &to,
                kind: "need",
            },
        )
        .unwrap();
}

#[test]
fn a_header_edge_removed_by_letter_is_gone_on_every_member() {
    let w = world();
    let (release, x, _) = w.release();
    for who in [&w.b, &w.c] {
        assert_eq!(deps_of(who, &release), contents(who, &[&x]));
        assert_eq!(readiness(&who.mailspace, &release), "blocked");
    }

    depend(&w.a, &release, &[], &[&x]);
    assert!(deps_of(&w.a, &release).is_empty(), "the writer's own graph");
    // Alice holds only the sender's copy: waiting on remote work, not blocked.
    assert_ne!(readiness(&w.a.mailspace, &release), "blocked");
    w.push();
    w.pull(&w.b);
    w.pull(&w.d);
    for who in [&w.b, &w.d] {
        assert!(deps_of(who, &release).is_empty(), "{}", who.address("mind"));
        assert_eq!(readiness(&who.mailspace, &release), "ready");
    }
    // c pulls the letter in a later pull than the item.
    w.pull(&w.c);
    assert!(deps_of(&w.c, &release).is_empty());
}

#[test]
fn an_added_dependency_blocks_the_release_on_every_member() {
    let w = world();
    let (release, x, y) = w.release();
    depend(&w.a, &release, &[&y], &[]);
    assert_eq!(deps_of(&w.a, &release), contents(&w.a, &[&x, &y]));
    w.push();
    w.pull(&w.b);
    w.pull(&w.d);
    for who in [&w.b, &w.d] {
        assert_eq!(deps_of(who, &release), contents(who, &[&x, &y]));
        assert_eq!(readiness(&who.mailspace, &release), "blocked");
    }
}

#[test]
fn add_then_remove_converges_in_either_arrival_order() {
    let w = world();
    let (release, x, y) = w.release();
    depend(&w.a, &release, &[&y], &[]);
    depend(&w.a, &release, &[], &[&y]);
    let letters = dependency_letters(&w.a);
    assert_eq!(letters.len(), 2, "one added letter, one removed letter");
    assert_eq!(
        header_values(&letters[0], "X-Vivi-Event"),
        ["dependency-added"]
    );
    assert_eq!(
        header_values(&letters[1], "X-Vivi-Event"),
        ["dependency-removed"]
    );

    // Carol: the newer (remove) letter reaches her first, the older one
    // after it.
    append(&w, 90, &letters[1]);
    w.pull(&w.c);
    assert_eq!(deps_of(&w.c, &release), contents(&w.c, &[&x]));
    w.push();
    w.pull(&w.c);
    assert_eq!(
        deps_of(&w.c, &release),
        contents(&w.c, &[&x]),
        "the older add arriving later changes nothing"
    );

    // Bob: older then newer, across two pulls.
    append(&w, 91, &letters[0]);
    w.pull(&w.b);
    w.push();
    w.pull(&w.b);
    assert_eq!(deps_of(&w.b, &release), contents(&w.b, &[&x]));

    // Dave pulls everything at once; alice's own graph agrees.
    w.pull(&w.d);
    assert_eq!(deps_of(&w.d, &release), contents(&w.d, &[&x]));
    assert_eq!(deps_of(&w.a, &release), contents(&w.a, &[&x]));
}

#[test]
fn remove_then_add_leaves_the_edge_on_every_member() {
    let w = world();
    let (release, x, _) = w.release();
    depend(&w.a, &release, &[], &[&x]);
    depend(&w.a, &release, &[&x], &[]);
    let letters = dependency_letters(&w.a);
    assert_eq!(letters.len(), 2);

    // The newer add letter first: it wins over the header and the older
    // remove that arrives after it.
    append(&w, 92, &letters[1]);
    w.pull(&w.c);
    assert_eq!(deps_of(&w.c, &release), contents(&w.c, &[&x]));
    w.push();
    w.pull(&w.c);
    assert_eq!(deps_of(&w.c, &release), contents(&w.c, &[&x]));
    assert_eq!(readiness(&w.c.mailspace, &release), "blocked");

    w.pull(&w.b);
    w.pull(&w.d);
    for who in [&w.a, &w.b, &w.d] {
        assert_eq!(deps_of(who, &release), contents(who, &[&x]));
    }
}

#[test]
fn audit_repair_never_puts_back_a_removed_header_edge() {
    let w = world();
    let (release, x, _) = w.release();
    depend(&w.a, &release, &[], &[&x]);
    w.push();
    w.pull(&w.b);
    let report = w.b.mailspace.backlog_audit(true).unwrap();
    assert!(
        report
            .findings
            .iter()
            .all(|finding| finding.class != "missing_edge"),
        "{:?}",
        report.findings
    );
    assert!(deps_of(&w.b, &release).is_empty());
    // A second pull of the same letters re-attaches the item's header and
    // still leaves the edge off.
    w.pull(&w.b);
    assert!(deps_of(&w.b, &release).is_empty());
}
