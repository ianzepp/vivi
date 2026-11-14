use super::*;
use crate::test_support::{SeedRun, Seeded, seeded};

const NOW: i64 = 1_000_000;
const HOUR: i64 = 3600;

/// What to plant on a run beyond the shared `SeedRun` columns.
#[derive(Default)]
struct Extra<'a> {
    push: Option<&'a str>,
    read_only: bool,
    reason: Option<&'a str>,
}

/// One collected run, `ago` seconds before `NOW`.
fn plant(store: &Seeded, job: &str, ago: i64, run: SeedRun<'_>, extra: &Extra<'_>) {
    store.insert(&SeedRun {
        job,
        dispatched_at: NOW - ago - 600,
        collected_at: Some(NOW - ago),
        ..run
    });
    store
        .conn
        .execute(
            "UPDATE slot_runs SET push = ?2, read_only = ?3, landing_reason = ?4 WHERE job = ?1",
            rusqlite::params![job, extra.push, extra.read_only, extra.reason],
        )
        .unwrap();
}

fn finished<'a>() -> SeedRun<'a> {
    SeedRun {
        outcome: Some("finished"),
        agent_rc: Some(0),
        check_rc: Some(0),
        ..SeedRun::default()
    }
}

fn pushed() -> Extra<'static> {
    Extra {
        push: Some("pushed"),
        ..Extra::default()
    }
}

fn with_landing<'a>(landing: &'a str) -> SeedRun<'a> {
    SeedRun {
        landing: Some(landing),
        ..finished()
    }
}

/// A day of runs with one of each kind, and a quieter day before.
fn seed(store: &Seeded) {
    let day = 24 * HOUR;
    let timeout = SeedRun {
        outcome: Some("timeout"),
        ..SeedRun::default()
    };
    plant(store, "t1", HOUR, timeout, &Extra::default());
    let lost = SeedRun {
        outcome: Some("lost"),
        ..SeedRun::default()
    };
    plant(store, "l1", HOUR, lost, &Extra::default());
    for job in ["a1", "a2"] {
        let agent = SeedRun {
            agent_rc: Some(1),
            ..finished()
        };
        plant(store, job, 2 * HOUR, agent, &pushed());
    }
    let check = SeedRun {
        check_rc: Some(101),
        ..finished()
    };
    plant(store, "c1", 2 * HOUR, check, &pushed());
    let quiet = Extra {
        push: Some("nothing"),
        ..Extra::default()
    };
    plant(store, "n1", 3 * HOUR, finished(), &quiet);
    plant(store, "ok1", 3 * HOUR, finished(), &pushed());
    let read_only = Extra {
        read_only: true,
        ..Extra::default()
    };
    plant(store, "ro1", 3 * HOUR, finished(), &read_only);
    plant(
        store,
        "landed1",
        3 * HOUR,
        with_landing("landed"),
        &pushed(),
    );
    // Red landings: the failing command is the reason.
    for (job, command) in [
        ("r1", "faber --test integration"),
        ("r2", "faber --test integration"),
        ("r3", "scripta stage 2"),
    ] {
        let red = Extra {
            reason: Some(command),
            ..pushed()
        };
        plant(store, job, 4 * HOUR, with_landing("red"), &red);
    }
    // Conflicts: the conflicting files are the reason, one per line.
    let first = Extra {
        reason: Some(
            "radix-mir-clif/lower/call.rs\nradix-mir-clif/lower/abi.rs\ncorpus/reference/a.rs",
        ),
        ..pushed()
    };
    plant(store, "x1", 5 * HOUR, with_landing("conflict"), &first);
    let second = Extra {
        reason: Some("radix-mir-clif/lower/emit.rs\nCargo.toml"),
        ..pushed()
    };
    plant(store, "x2", 5 * HOUR, with_landing("conflict"), &second);
    plant(store, "ra1", 5 * HOUR, with_landing("raced"), &pushed());
    // Another pool, to prove the filter.
    let elsewhere = SeedRun {
        outcome: Some("timeout"),
        pool: "other",
        ..SeedRun::default()
    };
    plant(store, "o1", HOUR, elsewhere, &Extra::default());
    // The window before: two timeouts and two agent failures.
    for (job, outcome, rc) in [
        ("p1", "timeout", 0),
        ("p2", "timeout", 0),
        ("p3", "finished", 1),
        ("p4", "finished", 1),
    ] {
        let before = SeedRun {
            outcome: Some(outcome),
            agent_rc: Some(rc),
            check_rc: Some(0),
            ..SeedRun::default()
        };
        plant(store, job, day + 2 * HOUR, before, &pushed());
    }
}

fn ready(store: &Seeded, window: Window, pool: &str) -> FailuresData {
    match load(&store.root, NOW, window, pool).unwrap() {
        RunsState::Ready(data) => data,
        other => panic!("expected data, got {other:?}"),
    }
}

fn kind<'a>(data: &'a FailuresData, code: &str) -> &'a KindCount {
    data.kinds
        .iter()
        .find(|kind| kind.code == code)
        .unwrap_or_else(|| panic!("no {code} kind in {:?}", data.kinds))
}

#[test]
fn every_kind_is_counted_from_the_seeded_rows() {
    let store = seeded(true);
    seed(&store);
    let data = ready(&store, Window::Day, "");
    let counts: Vec<(&str, i64)> = data
        .kinds
        .iter()
        .map(|kind| (kind.code, kind.count))
        .collect();
    assert_eq!(
        counts,
        [
            ("timeout", 2),
            ("lost", 1),
            ("agent", 2),
            ("check", 1),
            ("not_pushed", 1),
            ("landing_red", 3),
            ("conflict", 2),
            ("raced", 1),
        ]
    );
    // 12 failures, plus ok1, ro1 and landed1 collected clean.
    assert_eq!(data.failed(), 13);
    assert_eq!(data.collected, 16);
}

#[test]
fn a_clean_unpushed_run_is_not_pushed_unless_it_is_read_only() {
    let store = seeded(true);
    seed(&store);
    let data = ready(&store, Window::Day, "");
    assert_eq!(
        kind(&data, "not_pushed").count,
        1,
        "n1 only, not ro1 or ok1"
    );
}

#[test]
fn trend_compares_with_the_window_before() {
    let store = seeded(true);
    seed(&store);
    let data = ready(&store, Window::Day, "");
    // Two timeouts before; here t1 and o1: flat.
    assert_eq!(kind(&data, "timeout").previous, 2);
    assert_eq!(kind(&data, "timeout").trend(), Trend::Flat);
    // A check failure with none before is new.
    assert_eq!(kind(&data, "check").trend(), Trend::New);
    // Two agent failures before and now: flat.
    assert_eq!(kind(&data, "agent").previous, 2);
    assert_eq!(kind(&data, "agent").trend(), Trend::Flat);
}

#[test]
fn trend_words_follow_the_ten_percent_band() {
    let at = |count, previous| {
        KindCount {
            code: "x",
            label: "x",
            count,
            previous,
        }
        .trend()
    };
    assert_eq!(at(5, 0), Trend::New);
    assert_eq!(at(0, 0), Trend::Flat);
    assert_eq!(at(11, 10), Trend::Flat);
    assert_eq!(at(12, 10), Trend::Up);
    assert_eq!(at(1, 10), Trend::Down);
    assert_eq!(at(0, 3), Trend::Down);
}

#[test]
fn a_window_only_counts_its_own_runs() {
    let store = seeded(true);
    seed(&store);
    // Everything but the 1 h rows is older than an hour... t1, l1 and o1 sit
    // exactly at the edge, so look at a week instead.
    let week = ready(&store, Window::Week, "");
    assert_eq!(kind(&week, "timeout").count, 4, "t1, o1, p1, p2");
    assert_eq!(kind(&week, "timeout").previous, 0);
    let hour = ready(&store, Window::Hour, "");
    assert_eq!(hour.failed(), 3, "t1, l1, o1 at one hour");
}

#[test]
fn the_pool_filter_keeps_one_pool() {
    let store = seeded(true);
    seed(&store);
    let main = ready(&store, Window::Day, "main");
    assert_eq!(kind(&main, "timeout").count, 1, "t1 only");
    let other = ready(&store, Window::Day, "other");
    assert_eq!(other.failed(), 1);
    assert_eq!(other.pools, ["main"]);
}

#[test]
fn hot_files_count_conflicts_by_two_component_prefix() {
    let store = seeded(true);
    seed(&store);
    let data = ready(&store, Window::Day, "");
    let hot: Vec<(&str, i64)> = data
        .hot_paths
        .iter()
        .map(|path| (path.prefix.as_str(), path.count))
        .collect();
    // x1 touches radix-mir-clif/lower/ twice but counts once per run.
    assert_eq!(
        hot,
        [
            ("radix-mir-clif/lower/", 2),
            ("Cargo.toml", 1),
            ("corpus/reference/", 1),
        ]
    );
}

#[test]
fn red_landings_are_counted_by_failing_command() {
    let store = seeded(true);
    seed(&store);
    let data = ready(&store, Window::Day, "");
    let red: Vec<(&str, i64)> = data
        .red_commands
        .iter()
        .map(|red| (red.command.as_str(), red.count))
        .collect();
    assert_eq!(
        red,
        [("faber --test integration", 2), ("scripta stage 2", 1)]
    );
}

#[test]
fn needs_that_keep_coming_back_have_three_runs_and_no_landing() {
    let store = seeded(true);
    let back = |job: &str, run: SeedRun<'static>, extra: &Extra<'_>| {
        let run = SeedRun {
            need: Some("need-back"),
            ..run
        };
        plant(&store, job, HOUR, run, extra);
    };
    let check = SeedRun {
        check_rc: Some(101),
        ..finished()
    };
    back("b1", check, &pushed());
    back("b2", with_landing("conflict"), &pushed());
    back("b3", with_landing("raced"), &pushed());
    back("b4", with_landing("red"), &pushed());
    // A need that landed once does not come back.
    for (job, run) in [
        ("g1", with_landing("conflict")),
        ("g2", with_landing("landed")),
        ("g3", with_landing("raced")),
    ] {
        let run = SeedRun {
            need: Some("need-landed"),
            ..run
        };
        plant(&store, job, HOUR, run, &pushed());
    }
    // Too few runs.
    for job in ["f1", "f2"] {
        let run = SeedRun {
            need: Some("need-few"),
            ..with_landing("raced")
        };
        plant(&store, job, HOUR, run, &pushed());
    }
    let data = ready(&store, Window::Day, "");
    assert_eq!(data.repeat_needs.len(), 1, "{:?}", data.repeat_needs);
    let need = &data.repeat_needs[0];
    assert_eq!((need.need.as_str(), need.runs), ("need-back", 4));
    let mix: std::collections::BTreeMap<&str, i64> = need
        .mix
        .iter()
        .map(|(label, count)| (label.as_str(), *count))
        .collect();
    assert_eq!(
        mix,
        [
            ("check red", 1),
            ("conflict", 1),
            ("landing red", 1),
            ("raced", 1)
        ]
        .into_iter()
        .collect()
    );
}

#[test]
fn path_prefixes_stop_at_two_directories() {
    assert_eq!(path_prefix("a/b/c/d.rs").as_deref(), Some("a/b/"));
    assert_eq!(path_prefix("a/b/c.rs").as_deref(), Some("a/b/"));
    assert_eq!(path_prefix("a/b.rs").as_deref(), Some("a/"));
    assert_eq!(path_prefix("Cargo.toml").as_deref(), Some("Cargo.toml"));
    assert_eq!(path_prefix("./a/b.rs").as_deref(), Some("a/"));
    assert_eq!(path_prefix("  "), None);
}

#[test]
fn windows_parse_and_default_to_a_day() {
    assert_eq!(Window::parse("1h"), Window::Hour);
    assert_eq!(Window::parse("7d"), Window::Week);
    assert_eq!(Window::parse("24h"), Window::Day);
    assert_eq!(Window::parse("junk"), Window::Day);
}

#[test]
fn a_store_without_the_slot_index_reads_as_such() {
    let store = seeded(false);
    assert_eq!(
        load(&store.root, NOW, Window::Day, "").unwrap(),
        RunsState::NoSlotRuns
    );
}
