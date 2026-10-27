//! Measurement, not assertion: hosted-index passes over a host mailspace of
//! several thousand letters (S4).
//!
//! The mailspace is built with the vivi library's own `send` through the
//! shared fixture ([`fixture`]): a quarter of the letters addressed off the
//! host as needs, a quarter off the host as mail, and half to the host's own
//! `hand` (local only under `send = "addressed"`). Each steady state gets
//! its own timed pass, with the actual message/content/event counts and the
//! pass outcome printed next to every timing:
//!
//! - bootstrap: the first pass, indexing the off-host half (the baseline);
//! - unchanged: a watermark check, no scan;
//! - one off-host addition: the pass scans every message row again to find
//!   one letter (the full-scan cost);
//! - local-only additions: the scan runs, nothing new travels;
//! - graph-only activity: graph events alone move the watermark;
//! - an event-recorded change: the event log alone moves the watermark.
//!
//! Ignored by default; run with:
//!
//! ```text
//! cargo test --release --test server_measure_index -- --ignored --nocapture
//! ```
//!
//! `VIVI_MEASURE_LETTERS` overrides the letter count (default 5000).

#[path = "server_measure_index/fixture.rs"]
mod fixture;

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use vivi::server::hosted;
use vivi::server::identities::IdentityRegistry;
use vivi::server::projects::{IndexOutcome, IndexSkip, ProjectStore};

use fixture::{
    Hub, MailCounts, Scratch, letters_from_env, off_host_share, read_counts,
    scenario_event_recorded, scenario_graph_only, scenario_local_only_additions,
    scenario_one_off_host_addition, send_mix,
};

fn timed<T>(label: &str, work: impl FnOnce() -> T) -> (T, Duration) {
    let start = Instant::now();
    let out = work();
    let took = start.elapsed();
    println!("{label}: {:.1} ms", took.as_secs_f64() * 1000.0);
    (out, took)
}

/// One timed index pass, printing the outcome it produced.
fn timed_pass(label: &str, projects: &ProjectStore, id: &str) -> IndexOutcome {
    let (outcome, _) = timed(label, || projects.index_hosted(id).unwrap());
    println!("  {outcome:?}");
    outcome
}

/// A pass ran and indexed exactly `indexed` letters, refusing none.
fn assert_ran(outcome: &IndexOutcome, indexed: usize) {
    match outcome {
        IndexOutcome::Ran {
            indexed: got,
            duplicates,
            refused,
            stopped,
        } => {
            assert_eq!(*got, indexed, "{outcome:?}");
            assert_eq!(*duplicates, 0, "{outcome:?}");
            assert_eq!(*refused, 0, "{outcome:?}");
            assert_eq!(*stopped, None, "{outcome:?}");
        }
        other => panic!("expected a ran pass, got {other:?}"),
    }
}

#[test]
#[ignore = "measurement; builds a mailspace of thousands of letters"]
fn one_index_pass_over_thousands_of_letters() {
    let letters = letters_from_env();
    let scratch = Scratch::new(&format!("index-{}", std::process::id()));
    let hub = Hub::init(&scratch);

    let ((), build) = timed(&format!("build {letters} letters"), || {
        send_mix(&hub, letters)
    });
    println!(
        "  ({:.2} ms per letter)",
        build.as_secs_f64() * 1000.0 / letters as f64
    );
    println!("  corpus: {}", read_counts(&hub.ms));

    let data = hub.data_dir();
    let (projects, identities) = (ProjectStore::new(&data), IdentityRegistry::new(&data));
    let report = hosted::reconcile(&projects, &identities, &BTreeSet::from([hub.root.clone()]));
    let id = report.served[0].project_id.clone();

    // Baseline: the first pass indexes the off-host half, letter by letter.
    let bootstrap = timed("bootstrap pass (baseline: index the off-host half)", || {
        projects.index_hosted(&id).unwrap()
    });
    println!("  {:?}", bootstrap.0);
    assert_ran(&bootstrap.0, off_host_share(letters));

    // Unchanged mailspace: a watermark check, no scan.
    let outcome = timed_pass("pass with nothing changed", &projects, &id);
    assert_eq!(outcome, IndexOutcome::Skipped(IndexSkip::Unchanged));

    // One new off-host letter moves the watermark: the pass scans every
    // message row again to find it.
    let before = read_counts(&hub.ms);
    scenario_one_off_host_addition(&hub, letters);
    println!(
        "  scenario: {}",
        MailCounts::delta(read_counts(&hub.ms), before)
    );
    let outcome = timed_pass("steady pass, one new letter (full scan)", &projects, &id);
    assert_ran(&outcome, 1);

    // More local-only needs: the scan runs, nothing new travels, nothing new
    // is indexed.
    let before = read_counts(&hub.ms);
    scenario_local_only_additions(&hub, letters + 1, 250);
    println!(
        "  scenario: {}",
        MailCounts::delta(read_counts(&hub.ms), before)
    );
    let outcome = timed_pass(
        "steady pass, local-only additions (full scan)",
        &projects,
        &id,
    );
    assert_ran(&outcome, 0);

    // Graph-only activity: graph events alone move the watermark, no letter
    // is sent.
    let before = read_counts(&hub.ms);
    scenario_graph_only(&hub);
    println!(
        "  scenario: {}",
        MailCounts::delta(read_counts(&hub.ms), before)
    );
    let outcome = timed_pass(
        "steady pass, graph-only activity (full scan)",
        &projects,
        &id,
    );
    assert_ran(&outcome, 0);

    // An event-recorded change: the event log alone moves the watermark, no
    // letter is sent.
    let before = read_counts(&hub.ms);
    scenario_event_recorded(&hub);
    println!(
        "  scenario: {}",
        MailCounts::delta(read_counts(&hub.ms), before)
    );
    let outcome = timed_pass(
        "steady pass, event-recorded change (full scan)",
        &projects,
        &id,
    );
    assert_ran(&outcome, 0);

    println!("  final: {}", read_counts(&hub.ms));
}

#[test]
fn fixture_builds_the_default_mix() {
    let scratch = Scratch::new(&format!("mix-{}", std::process::id()));
    let hub = Hub::init(&scratch);
    let letters = 8;
    send_mix(&hub, letters);

    let counts = read_counts(&hub.ms);
    // Eight letters: an off-host send holds one row (the sent copy), a
    // local-only need holds two (sent copy plus the delivered copy for
    // `hand`). 4 off-host letters + 4 local letters = 12 rows, 8 contents.
    assert_eq!(counts.messages, 12, "{counts}");
    assert_eq!(counts.contents, 8, "{counts}");
    // Every send logs its sent copy; the four local sends log deliveries too.
    assert_eq!(counts.events, 4 + 2 * 4, "{counts}");
    // The four needs mint backlog nodes (graph events); plain mail does not.
    assert!(counts.graph_events >= 4, "{counts}");
}

#[test]
fn no_send_scenarios_send_nothing() {
    let scratch = Scratch::new(&format!("scenarios-{}", std::process::id()));
    let hub = Hub::init(&scratch);
    send_mix(&hub, 8);
    let baseline = read_counts(&hub.ms);

    scenario_graph_only(&hub);
    let after = read_counts(&hub.ms);
    assert_eq!(
        after.messages, baseline.messages,
        "graph-only sent a letter"
    );
    assert!(after.graph_events > baseline.graph_events, "{after}");

    scenario_event_recorded(&hub);
    let after = read_counts(&hub.ms);
    assert_eq!(
        after.messages, baseline.messages,
        "event scenario sent a letter"
    );
    assert_eq!(after.events, baseline.events + 1, "{after}");

    scenario_one_off_host_addition(&hub, 8);
    let after = read_counts(&hub.ms);
    assert_eq!(after.contents, baseline.contents + 1, "{after}");

    scenario_local_only_additions(&hub, 9, 2);
    let after = read_counts(&hub.ms);
    // Two local needs: a sent copy plus a delivered copy each, one content.
    assert_eq!(after.messages, baseline.messages + 5, "{after}");
    assert_eq!(after.contents, baseline.contents + 3, "{after}");
}
