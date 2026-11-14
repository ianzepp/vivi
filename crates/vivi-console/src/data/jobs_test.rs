use super::*;
use crate::test_support::{SeedRun, Seeded, seeded};

const NOW: i64 = 1_000_000;

fn jobs(store: &Seeded, filter: &JobFilter, before: Option<Cursor>) -> JobsPage {
    match load(&store.root, NOW, filter, before).unwrap() {
        RunsState::Ready(page) => page,
        other => panic!("expected a ready page, got {other:?}"),
    }
}

fn names(page: &JobsPage) -> Vec<&str> {
    page.rows.iter().map(|row| row.run.job.as_str()).collect()
}

fn finished(job: &str, age: i64) -> SeedRun<'_> {
    SeedRun {
        job,
        dispatched_at: NOW - age,
        collected_at: Some(NOW - age + 60),
        outcome: Some("finished"),
        agent_rc: Some(0),
        check_rc: Some(0),
        ..SeedRun::default()
    }
}

/// An open run, a good run, a red check, a timeout, a red landing and a run
/// two days old, on two pools.
fn seed(store: &Seeded) {
    store.insert(&SeedRun {
        job: "open",
        dispatched_at: NOW - 100,
        ..SeedRun::default()
    });
    store.insert(&finished("good", 200));
    store.insert(&SeedRun {
        check_rc: Some(101),
        pool: "other",
        slot: "s2",
        model: "model-b",
        need: Some("bbbb1111ff"),
        ..finished("red-check", 300)
    });
    store.insert(&SeedRun {
        outcome: Some("timeout"),
        agent_rc: None,
        check_rc: None,
        ..finished("timeout", 400)
    });
    store.insert(&SeedRun {
        landing: Some("red"),
        need: Some("aaaa0000ff"),
        ..finished("red-landing", 500)
    });
    store.insert(&SeedRun {
        landing: Some("landed"),
        ..finished("landed", 600)
    });
    store.insert(&finished("old", 2 * 24 * 3600));
}

#[test]
fn the_list_is_newest_first_and_bounded_by_the_window() {
    let store = seeded(true);
    seed(&store);
    let page = jobs(&store, &JobFilter::default(), None);
    assert_eq!(
        names(&page),
        [
            "open",
            "good",
            "red-check",
            "timeout",
            "red-landing",
            "landed"
        ]
    );
    assert_eq!(page.next, None);

    let hour = JobFilter {
        window: Window::Hour,
        ..JobFilter::default()
    };
    assert_eq!(jobs(&store, &hour, None).rows.len(), 6);
    let week = JobFilter {
        window: Window::Week,
        ..JobFilter::default()
    };
    assert_eq!(
        jobs(&store, &week, None).rows.last().unwrap().run.job,
        "old"
    );
}

#[test]
fn outcome_filters_pick_one_outcome_or_the_open_runs() {
    let store = seeded(true);
    seed(&store);
    let by = |word: &str| {
        let filter = JobFilter {
            outcome: Some(word.into()),
            ..JobFilter::default()
        };
        names(&jobs(&store, &filter, None))
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    assert_eq!(by("open"), ["open"]);
    assert_eq!(by("timeout"), ["timeout"]);
    assert_eq!(by("finished").len(), 4);
    assert!(by("lost").is_empty());
}

#[test]
fn failed_keeps_bad_outcomes_codes_and_red_landings_but_not_open_runs() {
    let store = seeded(true);
    seed(&store);
    let filter = JobFilter {
        failed: true,
        ..JobFilter::default()
    };
    assert_eq!(
        names(&jobs(&store, &filter, None)),
        ["red-check", "timeout", "red-landing"]
    );
}

#[test]
fn pool_slot_model_and_need_prefix_filters() {
    let store = seeded(true);
    seed(&store);
    let one = |filter: JobFilter| {
        names(&jobs(&store, &filter, None))
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    let base = JobFilter::default;
    assert_eq!(
        one(JobFilter {
            pool: Some("other".into()),
            ..base()
        }),
        ["red-check"]
    );
    assert_eq!(
        one(JobFilter {
            slot: Some("s2".into()),
            ..base()
        }),
        ["red-check"]
    );
    assert_eq!(
        one(JobFilter {
            model: Some("model-b".into()),
            ..base()
        }),
        ["red-check"]
    );
    assert_eq!(
        one(JobFilter {
            need: Some("aaaa".into()),
            ..base()
        }),
        ["red-landing"]
    );
    assert!(
        one(JobFilter {
            need: Some("aaaa0000ffX".into()),
            ..base()
        })
        .is_empty(),
        "a prefix longer than the need matches nothing"
    );
    assert_eq!(
        one(JobFilter {
            pool: Some("other".into()),
            model: Some("model-a".into()),
            ..base()
        })
        .len(),
        0,
        "filters combine"
    );
}

/// 205 runs, three to a second, so a page boundary splits a second.
#[test]
fn paging_visits_every_run_once_even_inside_one_second() {
    let store = seeded(true);
    for n in 0..205 {
        let job = format!("job-{n:03}");
        store.insert(&SeedRun {
            job: &job,
            dispatched_at: NOW - 10 - n / 3,
            ..SeedRun::default()
        });
    }
    let filter = JobFilter::default();
    let mut seen: Vec<String> = Vec::new();
    let mut cursor = None;
    let mut pages = 0;
    loop {
        let page = jobs(&store, &filter, cursor);
        assert!(page.rows.len() as i64 <= PAGE_ROWS);
        seen.extend(page.rows.iter().map(|row| row.run.job.clone()));
        pages += 1;
        match page.next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(pages, 3);
    assert_eq!(seen.len(), 205);
    let mut unique = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), 205, "no run twice, none lost");
    // Newest first; within one second the later insert comes first.
    assert_eq!(seen[0], "job-002");
    assert_eq!(seen[1], "job-001");
    assert_eq!(seen[204], "job-204");
}

#[test]
fn a_store_without_slot_runs_reads_as_such() {
    let store = seeded(false);
    assert_eq!(
        load(&store.root, NOW, &JobFilter::default(), None).unwrap(),
        RunsState::NoSlotRuns
    );
}
