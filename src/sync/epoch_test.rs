//! The client half of the server log epoch: a wiped or restored server
//! mints a new epoch, and the next pull that sees it resets its cursor and
//! re-reads from sequence 0 instead of silently reading nothing.

use super::*;

use crate::VivariumError;
use crate::b64;
use crate::storage::sha256_hex;
use crate::sync::fake_server::{Checkout, PROJECT, World, checkout, wire_id};
use crate::sync::pull::{PageProgress, PullPlan};
use crate::sync::state::{Floor, SyncState};
use crate::sync::sync_dir;
use crate::sync::transport::{
    AppendFailure, Appended, Envelope, ListQuery, Page, Record, Transport,
};
use std::cell::Cell;
use std::collections::BTreeSet;
use std::rc::Rc;

use crate::sync::fake_server::{FakeClient, FakeServer};

#[test]
fn an_epoch_change_mid_life_resets_the_cursor_and_refiles_from_zero() {
    let world = World::new();
    world.put(1, "first", "b1");
    world.put(2, "second", "b2");
    world.server.set_epoch(Some(1));

    world.bob.pull(&world.bob_client);
    assert_eq!(world.state().epoch, Some(1));

    // The log is wiped and recreated (a restore): new letters restart the
    // sequences, under a new epoch.
    world.server.wipe();
    world.put(3, "third", "b3");
    world.server.set_epoch(Some(2));

    let report = world.bob.pull(&world.bob_client);

    assert_eq!(report.epoch_reset, Some((1, 2)));
    assert!(
        report.summary().contains(
            "server epoch changed (1 -> 2): cursor and floor reset, re-read from sequence 0"
        ),
        "{}",
        report.summary()
    );
    let sines: Vec<u64> = world.server.lists().iter().map(|l| l.0).collect();
    let stale = sines.iter().position(|s| *s == 2).expect("stale request");
    assert_eq!(sines[stale + 1], 0, "the pull re-read from sequence 0");
    assert_eq!(world.state().epoch, Some(2));
    assert_eq!(world.state().since, 1, "the new log's head");
    assert_eq!(world.state().floor, Some(Floor::full()));
    let ids: Vec<String> = world
        .bob
        .rows()
        .iter()
        .map(|row| row.message_id.clone())
        .collect();
    let unique: BTreeSet<String> = ids.iter().cloned().collect();
    assert_eq!(unique.len(), ids.len(), "nothing filed twice: {ids:?}");
    for expected in [wire_id(1, 0), wire_id(2, 0), wire_id(3, 0)] {
        assert!(
            ids.iter().filter(|id| **id == expected).count() == 1,
            "{expected} held exactly once: {ids:?}"
        );
    }
    // The epoch is the validity: the letter filed after the wipe carries
    // the new epoch, and the old log's letters keep their old bindings.
    let validity_of = |id: &str| {
        world
            .bob
            .row(id)
            .unwrap_or_else(|| panic!("{id} is held"))
            .remote
            .expect("bound to the project")
            .remote_uidvalidity
    };
    assert_eq!(validity_of(&wire_id(3, 0)), 2, "filed under the new epoch");
    assert_eq!(validity_of(&wire_id(1, 0)), 1, "the old log's binding");
    assert_eq!(validity_of(&wire_id(2, 0)), 1, "the old log's binding");
}

#[test]
fn the_first_epoch_seen_is_recorded_with_no_reset() {
    let world = World::new();
    world.put(1, "first", "b1");
    world.server.set_epoch(Some(7));

    let report = world.bob.pull(&world.bob_client);

    assert_eq!(report.epoch_reset, None);
    assert!(
        !report.summary().contains("epoch changed"),
        "{}",
        report.summary()
    );
    assert_eq!(world.state().epoch, Some(7));
    assert_eq!(world.state().since, 1, "the cursor stays");
    assert_eq!(world.bob.rows().len(), 1);
}

#[test]
fn an_absent_epoch_never_resets_even_after_one_was_stored() {
    let world = World::new();
    world.put(1, "first", "b1");
    world.server.set_epoch(Some(5));
    world.bob.pull(&world.bob_client);
    assert_eq!(world.state().epoch, Some(5));

    // The server stops sending an epoch (one that never adopted the field).
    world.server.set_epoch(None);
    world.put(2, "second", "b2");

    let report = world.bob.pull(&world.bob_client);

    assert_eq!(report.epoch_reset, None);
    assert!(!report.summary().contains("epoch changed"));
    assert_eq!(world.state().epoch, Some(5), "absent never resets");
    assert_eq!(world.state().since, 2);
    assert_eq!(world.bob.rows().len(), 2);
}

#[test]
fn an_unusable_epoch_value_is_ignored_by_the_page_parser() {
    for raw in ["\"3\"", "\"\"", "3.5", "-1", "5000000000", "true", "[]"] {
        let body = format!(r#"{{"next_cursor":0,"epoch":{raw}}}"#);
        let page: Page = serde_json::from_str(&body).unwrap();
        assert_eq!(page.epoch, None, "{raw}");
        assert!(page.epoch_unusable, "{raw} must be flagged unusable");
    }
}

#[test]
fn absent_null_and_zero_mean_no_epoch_without_a_warning() {
    for body in [
        r#"{"next_cursor":0}"#,
        r#"{"next_cursor":0,"epoch":null}"#,
        r#"{"next_cursor":0,"epoch":0}"#,
    ] {
        let page: Page = serde_json::from_str(body).unwrap();
        assert_eq!(page.epoch, None, "{body}");
        assert!(!page.epoch_unusable, "{body}");
    }
    let page: Page = serde_json::from_str(r#"{"next_cursor":0,"epoch":4294967295}"#).unwrap();
    assert_eq!(page.epoch, Some(4294967295));
    assert!(!page.epoch_unusable);
}

/// A read page with one letter addressed to `fx`'s `mind` role.
fn canned_page(fx: &Checkout, sequence: u64, subject: &str) -> Page {
    let to = fx.address("mind");
    let bytes = format!(
        "From: carol@elsewhere.example\r\nTo: {to}\r\nSubject: {subject}\r\n\
         Message-ID: <{subject}@x.test>\r\nDate: Sun, 11 Oct 2026 12:00:00 +0000\r\n\r\nbody\r\n"
    )
    .into_bytes();
    Page {
        records: vec![Record {
            sequence,
            message_id: wire_id(sequence, sequence),
            from: "carol@elsewhere.example".into(),
            to: Some(to),
            kind: Some("need".into()),
            content_sha256: sha256_hex(&bytes),
            from_prefix: None,
            appended_at: None,
            raw_eml: b64::encode(&bytes),
        }],
        next_cursor: sequence,
        has_more: None,
        epoch: None,
        epoch_unusable: false,
    }
}

/// A transport that serves fixed pages and refuses appends.
struct Canned {
    pages: Vec<Page>,
}

impl Transport for Canned {
    fn append(&self, _: &str, _: &Envelope<'_>) -> Result<Appended, AppendFailure> {
        Err(AppendFailure::Stop("canned".into()))
    }

    fn fetch(&self, query: &ListQuery<'_>) -> Result<Page, VivariumError> {
        Ok(self
            .pages
            .iter()
            .find(|page| {
                page.records
                    .first()
                    .is_some_and(|r| r.sequence > query.since)
            })
            .cloned()
            .unwrap_or(Page {
                records: Vec::new(),
                next_cursor: query.since,
                has_more: None,
                epoch: None,
                epoch_unusable: false,
            }))
    }
}

#[test]
fn an_unusable_epoch_warns_once_and_behaves_as_absent() {
    let fx = checkout("bob", &["mind"]);
    let first = Page {
        epoch_unusable: true,
        ..canned_page(&fx, 1, "one")
    };
    let second = Page {
        epoch_unusable: true,
        ..canned_page(&fx, 2, "two")
    };
    let canned = Canned {
        pages: vec![first, second],
    };

    let report = pull::pull(&fx.mailspace, &fx.settings, &canned).unwrap();

    let warnings = report
        .warnings
        .iter()
        .filter(|w| *w == "server sent an unusable epoch; ignoring it")
        .count();
    assert_eq!(warnings, 1, "one warning per pull: {:?}", report.warnings);
    assert_eq!(report.epoch_reset, None);
    let state = SyncState::load(&sync_dir(&fx.mailspace.dir), PROJECT).unwrap();
    assert_eq!(state.epoch, None);
    assert_eq!(state.since, 2);
    assert_eq!(report.filed.values().sum::<usize>(), 2);
}

#[test]
fn the_epoch_round_trips_through_the_state_file() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("sync");
    let mut state = SyncState::load(&dir, "prj").unwrap();
    state.since = 4;
    state.epoch = Some(9);
    state.save(&dir).unwrap();

    assert_eq!(SyncState::load(&dir, "prj").unwrap(), state);
    let raw = std::fs::read_to_string(dir.join("state.json")).unwrap();
    assert!(raw.contains("\"epoch\": 9"), "{raw}");
}

#[test]
fn an_epoch_of_none_is_not_written_to_the_state_file() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("sync");
    let mut state = SyncState::load(&dir, "prj").unwrap();
    state.since = 4;
    state.save(&dir).unwrap();

    let raw = std::fs::read_to_string(dir.join("state.json")).unwrap();
    assert!(!raw.contains("epoch"), "{raw}");
    assert_eq!(SyncState::load(&dir, "prj").unwrap().epoch, None);
}

#[test]
fn an_old_state_file_without_the_field_loads_as_no_epoch() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("sync");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("state.json"), r#"{"project":"prj","since":3}"#).unwrap();

    let state = SyncState::load(&dir, "prj").unwrap();

    assert_eq!(state.since, 3);
    assert_eq!(state.epoch, None);
    assert_eq!(state.epoch_line(), "epoch: not sent by the server");
}

#[test]
fn the_epoch_line_names_a_stored_epoch() {
    let mut state = SyncState::default();
    assert_eq!(state.epoch_line(), "epoch: not sent by the server");
    state.epoch = Some(3);
    assert_eq!(state.epoch_line(), "epoch: 3");
}

#[test]
fn an_epoch_change_between_two_pages_of_one_pull_resets() {
    let world = World::new();
    world.server.page_limit(1);
    world.put(1, "first", "b1");
    world.put(2, "second", "b2");
    world.server.set_epoch(Some(1));
    let server = world.server.clone();

    let report = pull::pull_with(
        &world.bob.mailspace,
        &world.bob.settings,
        &world.bob_client,
        &PullPlan::default(),
        &mut |progress: &PageProgress| {
            if progress.page == 1 {
                server.set_epoch(Some(2));
                world.put(3, "third", "b3");
            }
        },
    )
    .unwrap();

    assert_eq!(report.epoch_reset, Some((1, 2)));
    assert!(report.summary().contains("server epoch changed (1 -> 2)"));
    let sines: Vec<u64> = world.server.lists().iter().map(|l| l.0).collect();
    assert_eq!(sines, [0, 1, 0, 1, 2], "page two's epoch re-read from 0");
    let ids: Vec<String> = world
        .bob
        .rows()
        .iter()
        .map(|row| row.message_id.clone())
        .collect();
    let unique: BTreeSet<String> = ids.iter().cloned().collect();
    assert_eq!(unique.len(), ids.len(), "nothing filed twice: {ids:?}");
    for expected in [wire_id(1, 0), wire_id(2, 0), wire_id(3, 0)] {
        assert!(
            ids.iter().filter(|id| **id == expected).count() == 1,
            "{expected} held exactly once: {ids:?}"
        );
    }
    assert_eq!(world.state().epoch, Some(2));
}

/// A transport that mints a fresh epoch for every list response, as a
/// server whose log never stops changing.
struct Bumping {
    server: Rc<FakeServer>,
    client: FakeClient,
    epoch: Cell<u32>,
    fetches: Cell<u32>,
}

impl Transport for Bumping {
    fn append(&self, _: &str, _: &Envelope<'_>) -> Result<Appended, AppendFailure> {
        Err(AppendFailure::Stop("canned".into()))
    }

    fn fetch(&self, query: &ListQuery<'_>) -> Result<Page, VivariumError> {
        self.fetches.set(self.fetches.get() + 1);
        self.epoch.set(self.epoch.get() + 1);
        self.server.set_epoch(Some(self.epoch.get()));
        self.client.fetch(query)
    }
}

#[test]
fn an_epoch_that_changes_on_every_response_ends_the_pull_with_an_error() {
    let world = World::new();
    world.put(1, "first", "b1");
    world.server.set_epoch(Some(1));
    world.bob.pull(&world.bob_client);
    // Every response from now on carries an epoch the client has never
    // seen, so every page of the pull reads a log that no longer exists.
    let bumping = Bumping {
        server: Rc::clone(&world.server),
        client: world.bob.client(&world.server),
        epoch: Cell::new(1),
        fetches: Cell::new(0),
    };

    let error = pull::pull(&world.bob.mailspace, &world.bob.settings, &bumping)
        .expect_err("an unstable log ends the pull");

    assert!(
        error.to_string().contains(
            "server epoch changed 3 times during one pull; the server log is unstable; try again \
             later"
        ),
        "{error}"
    );
    assert!(
        bumping.fetches.get() <= 3,
        "at most three fetch rounds: {}",
        bumping.fetches.get()
    );
    let state = world.state();
    assert_eq!(state.since, 0, "the reset cursor stays saved");
    assert_eq!(state.epoch, Some(4), "the latest epoch stays saved");
}
