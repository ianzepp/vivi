//! Windowed pull, the floor, resync, paging, and the old state file.

use std::cell::Cell;
use std::time::{Duration, Instant};

use crate::VivariumError;
use crate::storage::{MessageIngestRequest, RemoteBindingInput};
use crate::sync::fake_server::{FakeClient, LETTER_DATE, PROJECT, World, letter, wire_id};
use crate::sync::pull::{PageProgress, PullPlan, PullReport, pull_with};
use crate::sync::state::{Floor, SyncState};
use crate::sync::sync_dir;
use crate::sync::transport::{AppendFailure, Appended, Envelope, ListQuery, Page, Transport};

const T1: &str = "2026-09-01T00:00:00Z";
const T2: &str = "2026-10-01T00:00:00Z";
const T3: &str = "2026-10-10T00:00:00Z";

fn windowed(w: &World, since: &str) -> Result<PullReport, VivariumError> {
    let plan = PullPlan {
        window: Some(since),
        resync: false,
    };
    pull_with(
        &w.bob.mailspace,
        &w.bob.settings,
        &w.bob_client,
        &plan,
        &mut |_| {},
    )
}

fn resynced(w: &World) -> PullReport {
    let plan = PullPlan {
        window: None,
        resync: true,
    };
    pull_with(
        &w.bob.mailspace,
        &w.bob.settings,
        &w.bob_client,
        &plan,
        &mut |_| {},
    )
    .unwrap()
}

fn list(since: u64, after: Option<&str>) -> (u64, u32, Option<String>) {
    (since, 500, after.map(String::from))
}

fn three_letters(w: &World) {
    w.at(T1).put(1, "old", "x");
    w.at(T2).put(2, "mid", "x");
    w.at(T3).put(3, "new", "x");
}

#[test]
fn a_window_files_the_recent_letters_and_records_the_floor() {
    let w = World::new();
    three_letters(&w);

    let report = windowed(&w, "2026-09-20T00:00:00Z").unwrap();

    assert_eq!((report.fetched, report.cursor), (2, 3));
    assert_eq!(
        w.server.lists(),
        vec![list(0, Some("2026-09-20T00:00:00Z"))]
    );
    let state = w.state();
    assert_eq!(
        state.floor,
        Some(Floor {
            sequence: 2,
            time: Some(T2.into())
        }),
        "the floor is the first record received"
    );
    assert_eq!(
        state.history_line(),
        format!("history from: {T2} (sequence 2)")
    );
    assert_eq!(w.live_ids(), vec![wire_id(2, 0), wire_id(3, 0)]);

    let next = w.bob.pull(&w.bob_client);

    assert_eq!(next.fetched, 0);
    assert_eq!(w.server.lists().last(), Some(&list(3, None)));
    assert_eq!(w.state().floor, state.floor, "a plain pull keeps the floor");
}

#[test]
fn an_empty_window_records_the_requested_time_until_a_letter_arrives() {
    let w = World::new();
    three_letters(&w);

    let report = windowed(&w, "2026-10-20T00:00:00Z").unwrap();

    assert_eq!((report.fetched, report.cursor), (0, 0));
    let state = w.state();
    assert_eq!(state.since, 0);
    assert_eq!(
        state.floor,
        Some(Floor {
            sequence: 0,
            time: Some("2026-10-20T00:00:00Z".into())
        })
    );
    assert_eq!(
        state.history_line(),
        "history from: 2026-10-20T00:00:00Z (no letters yet)"
    );

    // A later plain pull must not reach back to the beginning.
    w.at("2026-10-25T00:00:00Z").put(4, "after", "x");
    let next = w.bob.pull(&w.bob_client);

    assert_eq!(next.fetched, 1);
    assert_eq!(
        w.server.lists().last(),
        Some(&list(0, Some("2026-10-20T00:00:00Z")))
    );
    assert_eq!(w.live_ids(), vec![wire_id(4, 0)]);
    let state = w.state();
    assert_eq!(state.since, 4);
    assert_eq!(
        state.floor,
        Some(Floor {
            sequence: 4,
            time: Some("2026-10-25T00:00:00Z".into())
        }),
        "the first letter refines the floor"
    );
}

#[test]
fn letters_without_a_server_time_are_outside_every_window() {
    let w = World::new();
    three_letters(&w);
    for n in [2, 3] {
        w.server.clear_appended_at(&wire_id(n, 0));
    }

    let report = windowed(&w, "2026-09-20T00:00:00Z").unwrap();

    assert_eq!(report.fetched, 0);
    assert_eq!(
        w.state().floor,
        Some(Floor {
            sequence: 0,
            time: Some("2026-09-20T00:00:00Z".into())
        })
    );
}

#[test]
fn since_is_refused_on_a_mailspace_that_already_pulled() {
    let w = World::new();
    three_letters(&w);
    w.bob.pull(&w.bob_client);
    let lists = w.server.lists().len();
    let before = w.state();

    let error = windowed(&w, "7d").unwrap_err().to_string();

    assert!(error.contains("--since"), "{error}");
    assert!(error.contains("vivi sync resync"), "{error}");
    assert_eq!(w.server.lists().len(), lists, "refused before any request");
    assert_eq!(w.state(), before, "the state is untouched");
}

#[test]
fn a_plain_first_pull_records_full_history() {
    let w = World::new();
    assert_eq!(w.state().history_line(), "history from: nothing pulled yet");
    three_letters(&w);

    w.bob.pull(&w.bob_client);

    let state = w.state();
    assert_eq!(state.floor, Some(Floor::full()));
    assert_eq!(state.history_line(), "history from: full history");
    assert_eq!(w.server.lists(), vec![list(0, None)]);
}

#[test]
fn a_state_file_from_before_windows_reads_as_unknown_and_keeps_pulling() {
    let w = World::new();
    three_letters(&w);
    let dir = sync_dir(&w.bob.mailspace.dir);
    std::fs::create_dir_all(&dir).unwrap();
    let raw = format!(
        "{{\n  \"project\": \"{}\",\n  \"since\": 2\n}}",
        w.bob.settings.project
    );
    std::fs::write(dir.join("state.json"), raw).unwrap();

    let state = SyncState::load(&dir, &w.bob.settings.project).unwrap();
    assert_eq!(state.since, 2);
    assert_eq!(state.floor, None);
    assert_eq!(
        state.history_line(),
        "history from: unknown (pulled before windows)"
    );

    let report = w.bob.pull(&w.bob_client);

    assert_eq!((report.fetched, report.cursor), (1, 3));
    assert_eq!(w.server.lists(), vec![list(2, None)]);
    assert_eq!(w.state().floor, None, "an unknown floor is not guessed");
    assert_eq!(w.live_ids(), vec![wire_id(3, 0)]);
}

#[test]
fn resync_replays_everything_and_records_full_history() {
    let w = World::new();
    three_letters(&w);
    windowed(&w, "2026-09-20T00:00:00Z").unwrap();
    assert_eq!(w.live_ids().len(), 2);

    let report = resynced(&w);

    assert_eq!(report.fetched, 3);
    assert_eq!(report.existing, 2, "what the window held is left alone");
    assert_eq!(report.filed.values().sum::<usize>(), 1, "the old letter");
    assert_eq!(report.restored, 0);
    assert_eq!(w.server.lists().last(), Some(&list(0, None)));
    let state = w.state();
    assert_eq!(state.since, 3);
    assert_eq!(state.floor, Some(Floor::full()));
    assert_eq!(w.live_ids().len(), 3);
}

#[test]
fn resync_is_idempotent() {
    let w = World::new();
    three_letters(&w);
    w.bob.pull(&w.bob_client);
    let rows = w.bob.rows();
    let blobs = w.bob.blob_files();

    let report = resynced(&w);

    assert_eq!((report.fetched, report.existing), (3, 3));
    assert!(report.filed.is_empty());
    assert_eq!((w.bob.rows(), w.bob.blob_files()), (rows, blobs));
}

#[test]
fn a_long_history_is_read_in_pages_of_five_hundred() {
    let w = World::new();
    for n in 1..=1_200 {
        w.put(n, &format!("letter {n}"), "x");
    }
    let mut seen: Vec<PageProgress> = Vec::new();

    let report = pull_with(
        &w.bob.mailspace,
        &w.bob.settings,
        &w.bob_client,
        &PullPlan::default(),
        &mut |page| seen.push(page.clone()),
    )
    .unwrap();

    assert_eq!(
        (report.fetched, report.pages, report.cursor),
        (1_200, 3, 1_200)
    );
    assert_eq!(
        w.server.lists(),
        vec![list(0, None), list(500, None), list(1_000, None)]
    );
    let progress = |page, records, cursor, more| PageProgress {
        page,
        records,
        cursor,
        more,
    };
    assert_eq!(
        seen,
        vec![
            progress(1, 500, 500, true),
            progress(2, 500, 1_000, true),
            progress(3, 200, 1_200, false),
        ]
    );
    assert_eq!(w.live_ids().len(), 1_200);
}

/// A server that does not say `has_more`: the read ends at an empty page.
struct NoHasMore<'a>(&'a FakeClient);

impl Transport for NoHasMore<'_> {
    fn append(&self, id: &str, envelope: &Envelope<'_>) -> Result<Appended, AppendFailure> {
        self.0.append(id, envelope)
    }

    fn fetch(&self, query: &ListQuery<'_>) -> Result<Page, VivariumError> {
        let mut page = self.0.fetch(query)?;
        page.has_more = None;
        Ok(page)
    }
}

#[test]
fn a_server_that_does_not_page_is_read_until_an_empty_page() {
    let w = World::new();
    for n in 1..=5 {
        w.put(n, &format!("letter {n}"), "x");
    }
    w.server.page_limit(2);

    let transport = NoHasMore(&w.bob_client);
    let report = crate::sync::pull::pull(&w.bob.mailspace, &w.bob.settings, &transport).unwrap();

    assert_eq!((report.fetched, report.cursor), (5, 5));
    assert_eq!(report.pages, 4, "three pages of letters, then an empty one");
    assert_eq!(w.live_ids().len(), 5);
}

/// Fails the n-th read; every other call goes through.
struct FailsOn<'a> {
    inner: &'a FakeClient,
    n: usize,
    calls: Cell<usize>,
}

impl Transport for FailsOn<'_> {
    fn append(&self, id: &str, envelope: &Envelope<'_>) -> Result<Appended, AppendFailure> {
        self.inner.append(id, envelope)
    }

    fn fetch(&self, query: &ListQuery<'_>) -> Result<Page, VivariumError> {
        self.calls.set(self.calls.get() + 1);
        if self.calls.get() == self.n {
            return Err(VivariumError::Message("project server unreachable".into()));
        }
        self.inner.fetch(query)
    }
}

#[test]
fn an_interrupted_pull_resumes_at_the_last_whole_page() {
    let w = World::new();
    for n in 1..=5 {
        w.put(n, &format!("letter {n}"), "x");
    }
    w.server.page_limit(2);
    let flaky = FailsOn {
        inner: &w.bob_client,
        n: 2,
        calls: Cell::new(0),
    };

    let error = crate::sync::pull::pull(&w.bob.mailspace, &w.bob.settings, &flaky).unwrap_err();

    assert!(error.to_string().contains("unreachable"));
    let state = w.state();
    assert_eq!(state.since, 2, "the first page was saved");
    assert_eq!(state.floor, Some(Floor::full()));
    assert_eq!(w.live_ids().len(), 2);

    let report = w.bob.pull(&w.bob_client);

    assert_eq!((report.fetched, report.cursor), (3, 5));
    assert_eq!(w.live_ids().len(), 5);
    assert_eq!(w.server.lists().last(), Some(&list(4, None)));
}

#[test]
#[ignore = "perf: 5,000 letters; run with --ignored --nocapture"]
fn five_thousand_letters_in_one_pull() {
    let w = World::new();
    for n in 1..=5_000 {
        w.put(n, &format!("letter {n}"), "a short body\r\n");
    }
    let started = Instant::now();
    let mut page_times: Vec<Duration> = Vec::new();
    let mut last = started;

    let report = pull_with(
        &w.bob.mailspace,
        &w.bob.settings,
        &w.bob_client,
        &PullPlan::default(),
        &mut |_| {
            page_times.push(last.elapsed());
            last = Instant::now();
        },
    )
    .unwrap();

    let elapsed = started.elapsed();
    assert_eq!((report.fetched, report.pages), (5_000, 10));
    assert_eq!(w.live_ids().len(), 5_000);
    #[allow(clippy::cast_precision_loss)]
    let rate = 5_000.0 / elapsed.as_secs_f64();
    println!("pulled 5000 letters in {elapsed:?} ({rate:.0} letters/s, 10 pages)");
    println!("per-page times (500 letters each): {page_times:?}");
}

/// The floor under the pull: the same letters through `ingest_if_absent`
/// alone, one transaction each, with no filing, backlog or handle work.
#[test]
#[ignore = "perf: 1,000 letters ingested one transaction each; run with --ignored --nocapture"]
fn one_thousand_letters_ingested_alone() {
    let fx = crate::sync::fake_server::checkout("bob", &["mind"]);
    let mut storage = fx.mailspace.storage().unwrap();
    let letters: Vec<Vec<u8>> = (1..=1_000u64)
        .map(|n| {
            letter(
                &wire_id(n, 0),
                "mind@a1b2c3d4.local",
                "mind@e5f6a7b8.local",
                &format!("letter {n}"),
                "a short body\r\n",
                LETTER_DATE,
                "",
            )
        })
        .collect();
    let started = Instant::now();

    for (index, bytes) in letters.iter().enumerate() {
        let request = MessageIngestRequest {
            account: "mind".into(),
            local_role: "needs".into(),
            read_state: false,
            starred: false,
            message_id_hint: None,
            presented_id: Some(wire_id(index as u64 + 1, 0)),
            seed_hint: "perf".into(),
            remote: Some(RemoteBindingInput {
                account: "mind".into(),
                provider: "vivi-project".into(),
                remote_mailbox: PROJECT.into(),
                remote_uid: index as u32 + 1,
                remote_uidvalidity: 1,
            }),
        };
        storage.ingest_if_absent(&request, bytes, None).unwrap();
    }

    let elapsed = started.elapsed();
    #[allow(clippy::cast_precision_loss)]
    let rate = 1_000.0 / elapsed.as_secs_f64();
    println!("ingested 1000 letters alone in {elapsed:?} ({rate:.0} letters/s)");
}
