//! Measurement, not assertion: a synthetic 20,000-letter project built at
//! store level, timed for the paged list, an `after` window, a `kind`
//! filter, a `to` filter, and a 100-handle lookup (phase-03 item 13).
//!
//! Ignored by default (building the project takes minutes, because every
//! append rewrites the whole index). Run with:
//!
//! ```text
//! cargo test --release --test measure_windows -- --ignored --nocapture
//! ```
//!
//! `VIVI_MEASURE_LETTERS` overrides the letter count (default 20000).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chrono::{TimeZone, Utc};
use sha2::{Digest, Sha256};

use vivi_server::crypto::StoreCipher;
use vivi_server::projects::{AppendItem, ListQuery, ProjectStore, Viewer};

const PREFIXES: [&str; 10] = [
    "3f9a1c7e", "a1b2c3d4", "0badc0de", "deadbeef", "12345678", "cafef00d", "8badf00d", "feedface",
    "0123abcd", "99887766",
];
const BASE: i64 = 1_760_000_000;
const STEP_SECONDS: i64 = 30;

fn rss_kib() -> String {
    std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_else(|_| "?".to_owned())
}

fn handle_id(n: u64) -> String {
    // Unique 8-hex handle per letter, hashed so handles are not sequential.
    let h = n.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 32;
    format!("msg_{:08x}{:016x}", h as u32, n)
}

/// A letter of about 3 KiB whose bytes are unique per `n`.
fn letter_bytes(id: &str, n: u64) -> Vec<u8> {
    let mut raw = format!(
        "From: hand@3f9a1c7e.local\nTo: mind@{}.local\nMessage-ID: <{id}@p.local>\nSubject: letter {n}\n\n",
        PREFIXES[(n % 10) as usize]
    )
    .into_bytes();
    let mut x = n.wrapping_add(1);
    while raw.len() < 3 * 1024 {
        x = x
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        raw.push(b'a' + ((x >> 33) % 26) as u8);
    }
    raw
}

fn timed<T>(label: &str, work: impl FnOnce() -> T) -> (T, Duration) {
    let start = Instant::now();
    let out = work();
    let took = start.elapsed();
    println!("{label}: {:.1} ms", took.as_secs_f64() * 1000.0);
    (out, took)
}

/// Page `query_for(cursor)` to the end; returns (records, pages).
fn drain(
    store: &ProjectStore,
    cipher: &StoreCipher,
    project: &str,
    viewer: &Viewer<'_>,
    query_for: impl Fn(u64) -> ListQuery,
) -> (usize, usize) {
    let (mut records, mut pages, mut cursor) = (0, 0, 0);
    loop {
        let page = store
            .read_page(cipher, project, &query_for(cursor), viewer)
            .unwrap();
        records += page.records.len();
        pages += 1;
        cursor = page.next_cursor;
        if !page.has_more {
            return (records, pages);
        }
    }
}

#[test]
#[ignore = "measurement; builds a large project (minutes)"]
fn measure_windows_at_scale() {
    let total: u64 = std::env::var("VIVI_MEASURE_LETTERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20_000);
    let dir: PathBuf = std::env::temp_dir().join(format!("vivi-measure-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let clock = Arc::new(AtomicI64::new(BASE));
    let reading = clock.clone();
    let store = ProjectStore::with_clock(&dir, move || {
        Utc.timestamp_opt(reading.load(Ordering::SeqCst), 0)
            .unwrap()
    });
    let cipher = StoreCipher::from_base64(&STANDARD.encode([0x42u8; 32])).unwrap();
    let project = store.create_project().unwrap().project_id;

    println!("letters: {total}, rss before build: {} KiB", rss_kib());
    // Every append rewrites the whole index (read, stamp, write, fsync), so
    // a 20,000-letter build through `ProjectStore::append` takes several
    // minutes (2,000 appends take about 20 s). The default build therefore
    // writes the project directly in the store's own layout (sealed blobs
    // named by content hash plus an index of envelope rows) and times one
    // real append at full size; `VIVI_MEASURE_APPEND=1` builds through
    // `append` instead, for the same reads.
    let kinds = [Some("need"), Some("task"), Some("want"), None];
    let project_dir = dir.join("projects").join(&project);
    let build_start = Instant::now();
    let mut ids = Vec::with_capacity(total as usize + 1);
    if std::env::var("VIVI_MEASURE_APPEND").is_ok() {
        for n in 0..total {
            clock.store(BASE + n as i64 * STEP_SECONDS, Ordering::SeqCst);
            let id = handle_id(n);
            let to = format!("mind@{}.local", PREFIXES[(n % 10) as usize]);
            let raw = letter_bytes(&id, n);
            let item = AppendItem::new(&id, "hand", Some(&to), kinds[(n % 4) as usize], raw)
                .unwrap()
                .with_from_prefix(Some(PREFIXES[0]));
            store.append(&cipher, &project, &item).unwrap();
            ids.push(id);
            if n > 0 && n % 2_000 == 0 {
                println!(
                    "  appended {n}, {:.0} s",
                    build_start.elapsed().as_secs_f64()
                );
            }
        }
        println!(
            "built through append in {:.0} s",
            build_start.elapsed().as_secs_f64()
        );
    } else {
        let mut rows = Vec::with_capacity(total as usize);
        for n in 0..total {
            let id = handle_id(n);
            let raw = letter_bytes(&id, n);
            let sha: String = Sha256::digest(&raw)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            let sealed = cipher.encrypt(&raw).unwrap();
            let blob = project_dir.join("letters").join(format!("{sha}.sealed"));
            std::fs::write(blob, sealed).unwrap();
            let at = Utc
                .timestamp_opt(BASE + n as i64 * STEP_SECONDS, 0)
                .unwrap();
            rows.push(serde_json::json!({
                "message_id": id,
                "sequence": n + 1,
                "content_sha256": sha,
                "from": "hand",
                "from_prefix": PREFIXES[0],
                "to": format!("mind@{}.local", PREFIXES[(n % 10) as usize]),
                "kind": kinds[(n % 4) as usize],
                "appended_at": at.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            }));
            ids.push(id);
        }
        let index = serde_json::json!({"records": rows, "next_sequence": total});
        let bytes = serde_json::to_vec(&index).unwrap();
        std::fs::write(project_dir.join("items.json"), bytes).unwrap();
        println!(
            "wrote {total} letters in {:.1} s",
            build_start.elapsed().as_secs_f64()
        );
    }
    println!("rss after build: {} KiB", rss_kib());
    let index_bytes = std::fs::metadata(project_dir.join("items.json"))
        .map(|m| m.len())
        .unwrap_or(0);
    println!("items.json: {index_bytes} bytes");

    // One real append against the full-size index (the clock reads past
    // every stamped row).
    clock.store(BASE + total as i64 * STEP_SECONDS, Ordering::SeqCst);
    let id = handle_id(total);
    let raw = letter_bytes(&id, total);
    let item =
        AppendItem::new(&id, "hand", Some("mind@3f9a1c7e.local"), Some("need"), raw).unwrap();
    let (outcome, _) = timed(
        "one append at full size (index read, stamp, rewrite)",
        || store.append(&cipher, &project, &item).unwrap(),
    );
    assert_eq!(outcome.sequence, total + 1);
    ids.push(id);

    // A member of the first mailspace: sees every work item, and the mail
    // addressed to its own prefix.
    let viewer = Viewer {
        name: "mind",
        prefix: Some(PREFIXES[0]),
    };
    let rss_start = rss_kib();

    let (first, _) = timed(
        "first page, default limit (what an unparameterized pull sees)",
        || {
            store
                .read_page(&cipher, &project, &ListQuery::since(0), &viewer)
                .unwrap()
        },
    );
    println!(
        "  -> {} records, has_more={}, next_cursor={}",
        first.records.len(),
        first.has_more,
        first.next_cursor
    );

    let limit500 = |cursor: u64| ListQuery::parse(cursor, Some("500"), None, None, None).unwrap();
    let ((records, pages), _) = timed("full list, limit=500, paged to the end", || {
        drain(&store, &cipher, &project, &viewer, limit500)
    });
    println!("  -> {records} records in {pages} pages");

    let window_start = BASE + (total as i64 * 98 / 100) * STEP_SECONDS;
    let after = Utc
        .timestamp_opt(window_start, 0)
        .unwrap()
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    let ((records, pages), _) = timed("after = last ~2% of letters, limit=500, paged", || {
        drain(&store, &cipher, &project, &viewer, |cursor| {
            ListQuery::parse(cursor, Some("500"), Some(&after), None, None).unwrap()
        })
    });
    println!("  -> {records} records in {pages} pages");

    let ((records, pages), _) = timed("kind=need, limit=500, paged", || {
        drain(&store, &cipher, &project, &viewer, |cursor| {
            ListQuery::parse(cursor, Some("500"), None, Some("need"), None).unwrap()
        })
    });
    println!("  -> {records} records in {pages} pages");

    let ((records, pages), _) = timed(&format!("to={}, limit=500, paged", PREFIXES[0]), || {
        drain(&store, &cipher, &project, &viewer, |cursor| {
            ListQuery::parse(cursor, Some("500"), None, None, Some(PREFIXES[0])).unwrap()
        })
    });
    println!("  -> {records} records in {pages} pages");

    // 100 handles spread across the whole log (visible and not).
    let step = (ids.len() / 100).max(1) | 1;
    let handles: Vec<String> = ids
        .iter()
        .step_by(step)
        .take(100)
        .map(|id| id[4..12].to_owned())
        .collect();
    let (found, _) = timed("lookup of 100 handles", || {
        store
            .lookup_handles(&cipher, &project, &handles, &viewer)
            .unwrap()
    });
    println!(
        "  -> {} found, {} missing (hidden mail to other prefixes)",
        found.records.len(),
        found.missing.len()
    );

    println!(
        "rss before reads: {rss_start} KiB, after reads: {} KiB",
        rss_kib()
    );
    let _ = std::fs::remove_dir_all(&dir);
}
