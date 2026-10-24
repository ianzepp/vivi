//! Measurement, not assertion: one hosted-index pass over a host mailspace
//! of several thousand letters (S4).
//!
//! The mailspace is built with the vivi library's own `send`, a quarter of
//! the letters addressed off the host as needs, a quarter off the host as
//! mail, and half to the host's own `hand` (local only under
//! `send = "addressed"`). Ignored by default; run with:
//!
//! ```text
//! cargo test --release --test measure_index -- --ignored --nocapture
//! ```
//!
//! `VIVI_MEASURE_LETTERS` overrides the letter count (default 5000).

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use vivi::mailspace::{Mailspace, SendRequest};
use vivi_server::hosted;
use vivi_server::identities::IdentityRegistry;
use vivi_server::projects::{IndexOutcome, ProjectStore};

const OFF_HOST: &str = "mind@0badc0de.local";

fn send(ms: &Mailspace, to: &str, kind: Option<&str>, n: usize) {
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

fn timed<T>(label: &str, work: impl FnOnce() -> T) -> (T, Duration) {
    let start = Instant::now();
    let out = work();
    let took = start.elapsed();
    println!("{label}: {:.1} ms", took.as_secs_f64() * 1000.0);
    (out, took)
}

#[test]
#[ignore = "measurement; builds a mailspace of thousands of letters"]
fn one_index_pass_over_thousands_of_letters() {
    let letters: usize = std::env::var("VIVI_MEASURE_LETTERS")
        .ok()
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(5000);
    let base =
        std::env::temp_dir().join(format!("vivi-server-measure-index-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("host");
    let mut ms = Mailspace::init(Some(&root)).unwrap();
    ms.add_identity("mind").unwrap();
    ms.add_identity("hand").unwrap();
    let path = ms.config_path();
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str("\n[sync]\nname = \"hub\"\nsend = \"addressed\"\n");
    std::fs::write(&path, text).unwrap();
    let ms = Mailspace::open_root(root.clone()).unwrap();

    let own_hand = ms.address_for("hand");
    let ((), build) = timed(&format!("build {letters} letters"), || {
        for n in 0..letters {
            match n % 4 {
                0 => send(&ms, OFF_HOST, Some("need"), n),
                1 => send(&ms, OFF_HOST, None, n),
                _ => send(&ms, &own_hand, Some("need"), n),
            }
        }
    });
    println!(
        "  ({:.2} ms per letter)",
        build.as_secs_f64() * 1000.0 / letters as f64
    );

    let data = base.join("data");
    std::fs::create_dir_all(&data).unwrap();
    let (projects, identities) = (ProjectStore::new(&data), IdentityRegistry::new(&data));
    let report = hosted::reconcile(&projects, &identities, &BTreeSet::from([root.clone()]));
    let id = report.served[0].project_id.clone();

    let (outcome, _) = timed("first pass (index the off-host half)", || {
        projects.index_hosted(&id).unwrap()
    });
    println!("  {outcome:?}");

    // Unchanged mailspace: a watermark check, no scan.
    let (outcome, _) = timed("pass with nothing changed", || {
        projects.index_hosted(&id).unwrap()
    });
    println!("  {outcome:?}");

    // One new off-host letter moves the watermark: the pass scans every
    // message row again to find it.
    send(&ms, OFF_HOST, Some("need"), letters);
    let (outcome, _) = timed("steady pass, one new letter", || {
        projects.index_hosted(&id).unwrap()
    });
    println!("  {outcome:?}");
    assert!(
        matches!(outcome, IndexOutcome::Ran { indexed: 1, .. }),
        "{outcome:?}"
    );
    let _ = std::fs::remove_dir_all(&base);
}
