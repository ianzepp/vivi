//! Fixture smoke test for the hosted-process measurement support
//! (`support/hosted_measure.rs`), kept small so it runs in seconds.
//!
//! It proves what the real measurement runs rely on: the two hosted
//! mailspaces are isolated (fresh temp base, one isolated server data
//! directory, distinct project ids, each mailspace's own disposable sync
//! key), the corpus counts are what the writer sent, the traffic
//! classification is consistent and deterministic in shape for both the
//! local-only and the predominantly-local writer, the actual message-row
//! count is recorded separately from the send count, the off-host letters
//! are verifiable in the hosted transport index by the identities the
//! writer returned, and the minimum-corpus guard holds a real measurement
//! run to its floor.
//!
//! Smoke fixtures may shrink the corpus; a measurement run may not (it
//! goes through `build_initial_corpus`, which enforces the 5,000-send
//! minimum). Ignored by default; run with:
//!
//! ```text
//! cargo test --test server_measure_process -- --ignored --nocapture
//! ```
//!
//! `VIVI_MEASURE_SENDS` overrides the smoke corpus size.
//!
//! The process fixture (`support/hosted_process.rs`) is checked here too:
//! the CPU-delta/wall-delta arithmetic stands alone (no process needed),
//! the sampler integrates a live PID honestly, and — ignored, like the
//! fixture smoke — a real `vivi server run` process boots from this
//! fixture's isolated data directory, becomes ready, is accounted while
//! alive, survives short idle and mixed measurement windows end to end
//! (the mixed window's off-host letters are verified in the transport
//! index after the window, and its push passes carry the server's own
//! traced latencies), and is killed and reaped on drop.
//!
//! The real baseline lives in `process_benchmark_windows_*` (ignored): it
//! bootstraps the corpus strictly before any server or window exists,
//! warms the transport indexes, then collects a 120-second warmed window
//! (`support/hosted_measure.rs::MEASURE_WINDOW`) per selected mode —
//! `VIVI_MEASURE_MODE=idle,local,mixed`, all three by default — at the
//! server's unchanged five-second indexing cadence. Each window reports a
//! summary (`WindowSummary`): server CPU delta, wall duration, average
//! fraction of one core, the server's own traced push-pass latencies,
//! actual row counts, writer counts and latency, and — labeled as
//! derivation, never measurement — the old selector's corpus-enumeration
//! work, actual rows times the verified full-listing passes. Off-host
//! visibility is verified only after a window, so verification can never
//! drive the indexing cadence inside one.

use std::panic::AssertUnwindSafe;
use std::time::{Duration, Instant, SystemTime};

use vivi::registry::{Flags, Registry, save_at};
use vivi::server::projects::{IndexOutcome, Viewer};

#[path = "support/hosted_measure.rs"]
mod hosted_measure;
#[path = "support/hosted_process.rs"]
mod hosted_process;

use hosted_measure::{
    CandidateWork, HostedMeasure, MEASURE_WINDOW, MIN_MEASURE_SENDS, OFF_HOST, OffHostLetter,
    PassLog, Traffic, WindowMode, WindowSummary,
};
use hosted_process::{CpuDelta, CpuSampler, HostedServer, Sample, ServerSpec};

/// The smoke corpus: far below the measurement minimum, on purpose.
const SMOKE_SENDS: usize = 24;
/// Local-only letters written into each host before the mixed pass.
const SMOKE_LOCAL_ONLY: usize = 6;
/// A tiny cadence so the periodic schedule fires a few off-host letters
/// inside a seconds-long pass; a real run uses the two-second default.
const SMOKE_EVERY: Duration = Duration::from_millis(20);

/// How much of the drained server log one window's pass-latency parse
/// scans: the harness keeps the recent tail (64 KiB), so this covers all
/// of it without the parse ever blocking the child's stderr pipe.
const LOG_SCAN_BYTES: usize = 256 * 1_024;

fn smoke_sends() -> usize {
    std::env::var("VIVI_MEASURE_SENDS")
        .ok()
        .and_then(|raw| raw.parse().ok())
        .filter(|sends| (1..SMOKE_SENDS).contains(sends))
        .unwrap_or(SMOKE_SENDS)
}

/// The corpus a real measurement run bootstraps: `VIVI_MEASURE_CORPUS`, or
/// the 5,000-send minimum. A smaller run is a fixture mistake; the
/// `build_initial_corpus` guard would refuse it anyway.
fn measure_sends() -> usize {
    let sends = std::env::var("VIVI_MEASURE_CORPUS")
        .ok()
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(MIN_MEASURE_SENDS);
    assert!(
        sends >= MIN_MEASURE_SENDS,
        "a real run bootstraps at least {MIN_MEASURE_SENDS} sends, got {sends}"
    );
    sends
}

/// The modes a real run measures, in order: `VIVI_MEASURE_MODE` names a
/// comma-separated subset of `idle`, `local`, `mixed`; the default is all
/// three. An unknown name refuses rather than measuring the wrong thing.
fn measure_modes() -> Vec<WindowMode> {
    let raw = std::env::var("VIVI_MEASURE_MODE").unwrap_or_else(|_| "idle,local,mixed".into());
    raw.split(',')
        .map(|name| {
            WindowMode::parse(name)
                .unwrap_or_else(|| panic!("unknown VIVI_MEASURE_MODE mode {name:?}"))
        })
        .collect()
}

fn viewer_of<'a>(host: &'a hosted_measure::MeasureHost) -> Viewer<'a> {
    Viewer {
        name: &host.member,
        prefix: Some(&host.prefix),
    }
}

/// The off-host identities one traffic classification wrote, as served
/// records name them.
fn off_host_identities(traffic: &Traffic) -> Vec<(String, Option<String>)> {
    traffic
        .off_host
        .iter()
        .map(|letter: &OffHostLetter| (letter.to.clone(), letter.kind.clone()))
        .collect()
}

/// Register both hosted roots `serve = true` in an isolated registry under
/// the fixture's own base and boot the real binary's server on the
/// fixture's isolated data directory. `rust_log` requests a filter — the
/// measurement needs the server's debug-level pass tracing — and is never
/// inherited from the host either way.
fn boot_serving(env: &HostedMeasure, label: &str, rust_log: Option<&str>) -> HostedServer {
    let config = env.base.join(format!("config-{label}"));
    let mut registry = Registry::default();
    for host in &env.hosts {
        let _ = registry.add(
            host.root.clone(),
            Flags {
                pump: false,
                serve: true,
            },
        );
    }
    save_at(&config.join("vivi").join("pump-service.toml"), &registry)
        .expect("write the isolated registry");
    let spec = match rust_log {
        Some(filter) => ServerSpec::new(env.data.clone(), config).with_rust_log(filter),
        None => ServerSpec::new(env.data.clone(), config),
    };
    HostedServer::spawn(spec)
}

/// One window's outcome: the summary plus what its writers wrote off-host,
/// kept for verification that happens after the window, never during it.
struct WindowRun {
    summary: WindowSummary,
    off_host: Vec<Vec<OffHostLetter>>,
}

/// One benchmark window against a live server: the mode's writes run on a
/// thread per hosted mailspace (the idle mode runs none) until the
/// deadline, the server's CPU integrates across the window's samples, and
/// the server's own traced pass latencies are bounded to the window's wall
/// clock, so warm-up and post-window work never count as measured
/// latency. Nothing here verifies: reads — and the pre-read index passes
/// they drive — stay outside the timed window by construction.
fn run_window(
    server: &mut HostedServer,
    env: &HostedMeasure,
    mode: WindowMode,
    window: Duration,
) -> WindowRun {
    let rows_before: usize = env.hosts.iter().map(|host| env.message_rows(host)).sum();
    let mut sampler = CpuSampler::attach(server.pid).expect("the server pid is accountable");
    let started = SystemTime::now();
    let start = sampler.sample().expect("the server is live");
    let until = Instant::now() + window;
    let handles: Vec<_> = match mode {
        WindowMode::Idle => Vec::new(),
        WindowMode::Local | WindowMode::Mixed => env
            .hosts
            .iter()
            .map(|host| {
                let mailspace = env.mailspace(host);
                let to = host.hand_address.clone();
                std::thread::spawn(move || {
                    hosted_measure::write_window(&mailspace, &to, mode, until)
                })
            })
            .collect(),
    };
    let writer_threads = handles.len();
    while Instant::now() < until {
        // Each sample holds the sampler's refresh cadence, so the window
        // is a chain of fresh, real accounting windows.
        sampler
            .sample()
            .expect("the server stays live through the window");
    }
    let end = sampler.sample().expect("the server is still live");
    let ended = SystemTime::now();
    let delta = CpuDelta::between(start, end).expect("an advancing window");
    let mut off_host = Vec::new();
    let mut writer_sends = 0;
    let mut writer_seconds = 0.0;
    for handle in handles {
        let traffic = handle.join().expect("the window's writer did not panic");
        writer_sends += traffic.sends;
        writer_seconds += traffic.seconds;
        off_host.push(traffic.off_host);
    }
    let rows_after: usize = env.hosts.iter().map(|host| env.message_rows(host)).sum();
    let log = format!(
        "{}\n{}",
        server.stdout_tail(LOG_SCAN_BYTES),
        server.stderr_tail(LOG_SCAN_BYTES)
    );
    let pass_seconds = PassLog::in_window(&log, started, ended);
    WindowRun {
        summary: WindowSummary::new(
            mode,
            delta.cpu_seconds,
            delta.wall_seconds,
            writer_sends,
            writer_seconds,
            writer_threads,
            pass_seconds,
            rows_before,
            rows_after,
        ),
        off_host,
    }
}

/// Wait, outside any window, until both hosted transport indexes hold the
/// expected off-host letters: the indexer's own passes settle the indexes,
/// and the pre-read passes this polling drives are warm-up. A window then
/// starts from a settled, warmed index.
fn wait_index_settled(
    env: &HostedMeasure,
    expected: &[Vec<(String, Option<String>)>],
    deadline: Duration,
) {
    let until = Instant::now() + deadline;
    loop {
        let served: Vec<usize> = env
            .hosts
            .iter()
            .map(|host| {
                hosted_measure::read_all_records(&env.projects, &host.project_id, &viewer_of(host))
                    .len()
            })
            .collect();
        let want: Vec<usize> = expected.iter().map(Vec::len).collect();
        if served == want {
            return;
        }
        assert!(
            Instant::now() < until,
            "the transport indexes never settled: {served:?} served vs {want:?} written"
        );
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Verification, always outside a timed window: every off-host letter the
/// bootstrap and the windows wrote is exactly what the hosted transport
/// index serves, by its to/kind identity, for both hosts.
fn assert_off_host_served(env: &HostedMeasure, expected: &[Vec<(String, Option<String>)>]) {
    for (host, want) in env.hosts.iter().zip(expected) {
        let mut served: Vec<(Option<String>, Option<String>)> =
            hosted_measure::read_all_records(&env.projects, &host.project_id, &viewer_of(host))
                .into_iter()
                .map(|record| (record.to.clone(), record.kind.clone()))
                .collect();
        served.sort_unstable();
        let mut want: Vec<(Option<String>, Option<String>)> = want
            .iter()
            .map(|(to, kind)| (Some(to.clone()), kind.clone()))
            .collect();
        want.sort_unstable();
        assert_eq!(
            served, want,
            "host {}: served off-host letters drifted",
            host.name
        );
    }
}

/// The fixture's shape, checked while the traffic is written: the writer
/// sent exactly what was asked, the classification is consistent, and the
/// actual message rows match the send mix (a local send is one row for the
/// recipient plus the sender's sent copy; an off-host send is its sent
/// copy only). Returns the mixed pass for later verification.
fn check_traffic(
    env: &HostedMeasure,
    host: &hosted_measure::MeasureHost,
    sends: usize,
    local_only: usize,
) -> hosted_measure::Traffic {
    // Local-only: every letter to the host's own hand, none off-host.
    let mailspace = env.mailspace(host);
    let plain = hosted_measure::write_local_only(&mailspace, &host.hand_address, local_only);
    assert_eq!(plain.sends, local_only);
    assert_eq!(plain.local.len(), local_only);
    assert!(plain.off_host.is_empty(), "local-only means no off-host");
    let rows = env.message_rows(host);
    assert_eq!(rows, 2 * local_only, "local send = recipient + sent copy");

    // Mixed: predominantly local, with periodic off-host letters.
    let traffic = hosted_measure::write_local_with_periodic_off_host(
        &mailspace,
        &host.hand_address,
        sends,
        SMOKE_EVERY,
    );
    drop(mailspace);

    assert_eq!(traffic.sends, sends);
    assert_eq!(traffic.local.len() + traffic.off_host.len(), sends);
    assert!(!traffic.local.is_empty(), "predominantly local by shape");
    assert!(!traffic.off_host.is_empty(), "the schedule fired off-host");

    // Classification: local letters stay on the host's own hand (their
    // sent copies ride the local role, never the transport); every
    // off-host letter goes to OFF_HOST, alternates need/mail by fire
    // order, and is a corpus letter.
    for (i, letter) in traffic.off_host.iter().enumerate() {
        assert_eq!(letter.to, OFF_HOST);
        assert_eq!(
            letter.kind.as_deref(),
            if i % 2 == 0 { Some("need") } else { None },
            "off-host letter {i} broke the need/mail alternation"
        );
        assert!(
            letter.subject.starts_with("letter "),
            "off-host letters are corpus letters: {}",
            letter.subject
        );
        assert!(
            !letter.handle.is_empty(),
            "the sent-copy handle is recorded"
        );
    }

    // Actual message rows, recorded separately from the send count.
    let rows = env.message_rows(host);
    assert_eq!(
        rows,
        2 * local_only + 2 * traffic.local.len() + traffic.off_host.len(),
        "host {}: {rows} message rows for {} sends",
        host.name,
        plain.sends + traffic.sends
    );
    traffic
}

#[test]
#[ignore = "fixture smoke; builds two hosted mailspaces with a small corpus"]
fn hosted_measure_fixture_is_isolated_classified_and_guarded() {
    let sends = smoke_sends();
    let env = HostedMeasure::open("smoke");

    // Isolation: everything under a fresh temp base, one server data
    // directory holding the registry and both hosted projects, distinct
    // project ids, and each mailspace's own sync key under its root.
    assert!(env.base.starts_with(std::env::temp_dir()), "{:?}", env.base);
    assert_eq!(env.hosts.len(), 2);
    assert_ne!(env.hosts[0].project_id, env.hosts[1].project_id);
    for host in &env.hosts {
        assert_eq!(host.member, hosted_measure::HOST_MEMBER);
        assert!(host.root.join(".vivi").join("mail.sqlite").is_file());
        assert!(
            env.data.join("projects").join(&host.project_id).is_dir(),
            "host {} is not under the isolated data dir",
            host.name
        );
        assert!(
            host.root
                .join(".vivi")
                .join("chat")
                .join("identity.ed25519")
                .is_file(),
            "host {} has no disposable sync key under its own root",
            host.name
        );
        assert!(
            env.identities
                .by_name(&host.project_id, &host.member)
                .is_some(),
            "{} is not registered with the isolated registry",
            host.member
        );
    }

    // The minimum-corpus guard fires before a measurement run sends
    // anything: both mailspaces are still empty here.
    let refused_small = std::panic::catch_unwind(AssertUnwindSafe(|| env.build_initial_corpus(10)));
    assert!(
        refused_small.is_err(),
        "the {MIN_MEASURE_SENDS}-send minimum must refuse a small corpus"
    );
    for host in &env.hosts {
        assert_eq!(
            env.message_rows(host),
            0,
            "the guard fired before {} sent anything",
            host.name
        );
    }

    // Traffic, per host: local-only first, then the deterministic mixed
    // corpus. Same writer, same order, same shape for the before host and
    // the after host.
    let mut mixed = Vec::new();
    for host in &env.hosts {
        let traffic = check_traffic(&env, host, sends, SMOKE_LOCAL_ONLY);
        mixed.push((host.name.clone(), traffic));
    }
    assert_eq!(mixed[0].1.sends, mixed[1].1.sends);
    assert!(mixed[0].1.off_host.len() >= 1 && mixed[1].1.off_host.len() >= 1);

    // The writer's identities verify the letters where they landed: one
    // index pass files exactly the off-host letters, page-able by their
    // to/kind.
    let host = env.host("before");
    let traffic = &mixed
        .iter()
        .find(|(name, _)| name == &host.name)
        .map(|(_, traffic)| traffic)
        .unwrap();
    let outcome = env.projects.index_hosted(&host.project_id).unwrap();
    let IndexOutcome::Ran {
        indexed, refused, ..
    } = outcome
    else {
        panic!("the first pass must run: {outcome:?}");
    };
    assert_eq!(indexed, traffic.off_host.len(), "{outcome:?}");
    assert_eq!(refused, 0, "{outcome:?}");
    let records =
        hosted_measure::read_all_records(&env.projects, &host.project_id, &viewer_of(host));
    assert_eq!(records.len(), traffic.off_host.len());
    // The transport orders by sequence, not by fire order, so compare the
    // identities as a set: every off-host letter the writer named is
    // served exactly once, with its to and kind.
    let mut served: Vec<(Option<&str>, Option<&str>)> = records
        .iter()
        .map(|record| (record.to.as_deref(), record.kind.as_deref()))
        .collect();
    served.sort_unstable();
    let mut expected: Vec<(Option<&str>, Option<&str>)> = traffic
        .off_host
        .iter()
        .map(|letter| (Some(letter.to.as_str()), letter.kind.as_deref()))
        .collect();
    expected.sort_unstable();
    assert_eq!(served, expected);
}

#[test]
fn cpu_delta_arithmetic_stands_alone() {
    let t0 = Instant::now();
    let start = Sample {
        cpu_seconds: 1.5,
        at: t0,
    };
    let end = Sample {
        cpu_seconds: 2.75,
        at: t0 + Duration::from_millis(2_400),
    };
    let delta = CpuDelta::between(start, end).expect("an advancing window");
    assert_eq!(delta.cpu_seconds, 1.25);
    assert_eq!(
        delta.wall_seconds,
        Duration::from_millis(2_400).as_secs_f64()
    );
    assert!((delta.average_cores() - 1.25 / 2.4).abs() < 1e-12);
    assert!((delta.percent_of_one_core() - delta.average_cores() * 100.0).abs() < 1e-9);

    // An idle window is legitimate: zero CPU over real wall time.
    let idle = CpuDelta::between(
        start,
        Sample {
            cpu_seconds: 1.5,
            at: end.at,
        },
    )
    .unwrap();
    assert_eq!(idle.cpu_seconds, 0.0);
    assert_eq!(idle.average_cores(), 0.0);

    // A window that does not advance is an error, never a division by zero.
    assert_eq!(
        CpuDelta::between(start, start),
        Err(hosted_process::DeltaError(
            "the second sample must come after the first"
        ))
    );
    assert!(CpuDelta::between(end, start).is_err());
}

#[test]
fn cpu_sampler_integrates_a_live_pid() {
    // Mechanics check on a real, live PID: this test process. The
    // measurement harness itself only ever points the sampler at the
    // server's PID, never this one.
    let mut sampler = CpuSampler::attach(std::process::id()).expect("this platform accounts CPU");
    assert_eq!(sampler.pid(), std::process::id());
    assert_eq!(
        sampler.cumulative(),
        0.0,
        "the priming refresh has no window"
    );

    let start = sampler.sample().expect("the pid is live");
    // Burn real CPU inside the window so the integration must see it: a
    // sampler that silently produced zeros would fail the bound below.
    let spin_until = Instant::now() + Duration::from_millis(200);
    let mut spun = 0u64;
    while Instant::now() < spin_until {
        spun = spun.wrapping_add(1);
    }
    let end = sampler.sample().expect("the pid is still live");
    let _ = spun;

    let delta = CpuDelta::between(start, end).expect("an advancing window");
    assert!(
        delta.wall_seconds >= hosted_process::MIN_SAMPLE_INTERVAL.as_secs_f64(),
        "the sampler enforces its own refresh cadence: {delta:?}"
    );
    assert!(
        delta.cpu_seconds >= 0.05,
        "200ms of spin must surface as CPU seconds, not vanish: {delta:?}"
    );
    let cores = usize::from(std::thread::available_parallelism().unwrap()) as f64;
    assert!(
        delta.cpu_seconds <= delta.wall_seconds * cores,
        "one process cannot burn more CPU than the machine has cores: {delta:?}"
    );
}

#[test]
#[ignore = "boots the real vivi server binary as a hosted process"]
fn hosted_server_boots_ready_accounted_and_reaped() {
    let env = HostedMeasure::open("process");

    // What the server serves is the fixture's registry decision, applied by
    // the shared boot helper through the child's isolated VIVI_CONFIG_DIR.
    // The debug filter is asked for explicitly, so the pass-latency log the
    // window parse reads is really on.
    let mut server = boot_serving(&env, "process", Some("vivi=debug"));
    let pid = server.pid;
    assert!(
        hosted_process::process_alive(pid),
        "the server is live after spawn"
    );
    server
        .wait_ready(Duration::from_secs(30))
        .expect("the server becomes ready and answers /healthz");
    assert_ne!(server.spec().port, 0);

    // A live server PID pays real cumulative accounting, outside this
    // process: two samples a window apart.
    let mut sampler = CpuSampler::attach(pid).expect("the server pid is accountable");
    let start = sampler.sample().expect("the server is live");
    let end = sampler.sample().expect("the server is still live");
    let delta = CpuDelta::between(start, end).expect("an advancing window");
    assert!(
        delta.wall_seconds >= hosted_process::MIN_SAMPLE_INTERVAL.as_secs_f64(),
        "accounting windows respect the refresh cadence: {delta:?}"
    );
    assert!(
        delta.cpu_seconds >= 0.0,
        "accounting is never negative: {delta:?}"
    );

    // Two short windows, end to end. The idle window writes nothing: its
    // writer counts are zero and its pass list is whatever the parse bound
    // to the window's wall clock found (the boot pass logged before it must
    // not leak in). The mixed window writes on both hosted mailspaces, the
    // server's unchanged five-second cadence must complete a push pass
    // inside it, and its off-host letters are verified in the transport
    // index — after the window, never during it.
    let idle = run_window(&mut server, &env, WindowMode::Idle, Duration::from_secs(2));
    assert!(
        idle.summary.seconds >= 2.0,
        "the window spans the requested wall time: {:?}",
        idle.summary
    );
    assert_eq!(idle.summary.writer_sends, 0, "idle writes nothing");
    assert_eq!(idle.summary.writer_threads, 0);
    assert!(idle.summary.cpu_seconds >= 0.0);
    assert!(idle.summary.pass_seconds.iter().all(|pass| *pass >= 0.0));
    for line in idle.summary.lines() {
        println!("{line}");
    }

    let mixed = run_window(&mut server, &env, WindowMode::Mixed, Duration::from_secs(6));
    assert!(
        mixed.summary.writer_sends > 0,
        "the mixed window writes: {:?}",
        mixed.summary
    );
    assert!(
        mixed.summary.rows_after > mixed.summary.rows_before,
        "the writers moved the mailspaces: {:?}",
        mixed.summary
    );
    assert!(
        !mixed.summary.pass_seconds.is_empty(),
        "the five-second cadence completes a traced pass inside the window: {:?}",
        mixed.summary
    );
    assert!(
        mixed.off_host.iter().all(|letters| !letters.is_empty()),
        "both hosts sent off-host"
    );
    for line in mixed.summary.lines() {
        println!("{line}");
    }
    let expected: Vec<Vec<(String, Option<String>)>> = mixed
        .off_host
        .iter()
        .map(|letters| {
            letters
                .iter()
                .map(|letter| (letter.to.clone(), letter.kind.clone()))
                .collect()
        })
        .collect();
    wait_index_settled(&env, &expected, Duration::from_secs(30));
    assert_off_host_served(&env, &expected);

    // Cleanup: dropping the handle kills and reaps on the success path;
    // Drop covers the failure and unwind paths the same way.
    drop(server);
    assert!(
        !hosted_process::process_alive(pid),
        "server pid {pid} was not reaped"
    );
}

#[test]
fn window_summary_reports_measured_counters_and_derived_candidate() {
    let summary = WindowSummary::new(
        WindowMode::Mixed,
        4.0,                  // server cpu seconds
        120.0,                // wall seconds
        1_200,                // writer sends
        119.5,                // writer seconds
        2,                    // writer threads
        vec![0.5, 0.25, 1.0], // pass latencies
        10_000,               // rows before
        10_240,               // rows after
    );
    assert_eq!(summary.mode, WindowMode::Mixed);
    assert_eq!(summary.seconds, 120.0);
    assert_eq!(summary.cpu_seconds, 4.0);
    assert!((summary.average_cores - 4.0 / 120.0).abs() < 1e-12);
    assert_eq!(summary.pass_seconds, vec![0.5, 0.25, 1.0]);
    assert_eq!(summary.rows_before, 10_000);
    assert_eq!(summary.rows_after, 10_240);
    assert_eq!(summary.writer_sends, 1_200);
    assert!((summary.writer_seconds - 119.5).abs() < 1e-12);
    assert_eq!(summary.writer_threads, 2);
    // The candidate term is derived: actual rows times the passes, not a
    // measured counter.
    assert_eq!(
        summary.candidate,
        CandidateWork {
            passes: 3,
            rows_per_pass: 10_000,
            enumerated_rows: 30_000,
        }
    );
    let lines = summary.lines().join("\n");
    assert!(lines.contains("derived"), "labeled as derivation: {lines}");
    assert!(
        lines.contains("30000"),
        "the derived total is reported: {lines}"
    );
}

#[test]
fn window_summary_of_an_idle_window_has_no_writer_and_no_candidate() {
    let summary = WindowSummary::new(
        WindowMode::Idle,
        0.25,
        120.0,
        0,
        0.0,
        0,
        Vec::new(),
        9_000,
        9_000,
    );
    assert_eq!(summary.writer_sends, 0);
    assert_eq!(summary.writer_threads, 0);
    assert_eq!(summary.candidate.passes, 0);
    assert_eq!(summary.candidate.rows_per_pass, 9_000);
    assert_eq!(
        summary.candidate.enumerated_rows, 0,
        "no passes, no derived enumeration"
    );
    let lines = summary.lines().join("\n");
    assert!(lines.contains("0 full-listing passes"), "{lines}");
}

#[test]
#[should_panic = "positive wall time"]
fn window_summary_refuses_a_window_that_spans_no_wall_time() {
    let _ = WindowSummary::new(WindowMode::Local, 1.0, 0.0, 1, 1.0, 1, Vec::new(), 1, 1);
}

#[test]
fn pass_log_bounds_latencies_to_the_window() {
    let start = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
    let end = start + Duration::from_secs(120);
    let line = |at: SystemTime, ms: u64| {
        format!(
            "{} DEBUG vivi::server::projects::hosted_index: hosted index pass complete \
             id=\"before\" sent=0 elapsed_ms={ms}",
            chrono::DateTime::<chrono::Utc>::from(at).to_rfc3339()
        )
    };
    // The same shape as the server really prints it: styled with ANSI
    // escapes around the timestamp, level, and target.
    let styled = format!(
        "\u{1b}[2m{}\u{1b}[0m \u{1b}[34mDEBUG\u{1b}[0m \
         \u{1b}[2mvivi::server::projects::hosted_index\u{1b}[0m: \
         hosted index pass complete id=\"after\" sent=0 elapsed_ms=500",
        chrono::DateTime::<chrono::Utc>::from(start + Duration::from_secs(30)).to_rfc3339()
    );
    let log = [
        line(start - Duration::from_secs(5), 111), // a warm-up pass, before
        line(start + Duration::from_secs(5), 250),
        styled,
        line(end - Duration::from_secs(1), 1_250),
        line(end + Duration::from_secs(5), 999), // after the window
        "2026-01-01T00:00:00Z  INFO vivi::server: unrelated line".to_owned(),
    ]
    .join("\n");
    assert_eq!(PassLog::in_window(&log, start, end), vec![0.25, 0.5, 1.25]);
    // An empty log, or one with no pass lines at all, measures nothing.
    assert!(PassLog::in_window("", start, end).is_empty());
    assert!(PassLog::in_window("2026-01-01T00:00:00Z  INFO boot", start, end).is_empty());
}

#[test]
#[ignore = "the real baseline: 120 s warmed windows on a live vivi server process"]
fn process_benchmark_windows_report_cpu_pass_latency_and_candidate_work() {
    let env = HostedMeasure::open("process-benchmark");

    // Bootstrap, strictly before any server or window exists: building the
    // corpus is never inside a measured window. Both hosted mailspaces are
    // filled, and their off-host letters are what the transport index must
    // serve once the server's indexer settles.
    let bootstrap = env.build_initial_corpus(measure_sends());
    let mut expected: Vec<Vec<(String, Option<String>)>> = env
        .hosts
        .iter()
        .zip(&bootstrap)
        .map(|(_, (traffic, _))| off_host_identities(traffic))
        .collect();

    // The child boots on its own defaults, so the indexing cadence is the
    // server's unchanged five-second interval; the debug filter only makes
    // its own pass tracing visible. Warm-up runs outside any window.
    let mut server = boot_serving(&env, "benchmark", Some("vivi=debug"));
    server
        .wait_ready(Duration::from_secs(30))
        .expect("the benchmark server becomes ready");
    wait_index_settled(&env, &expected, Duration::from_secs(180));

    for mode in measure_modes() {
        let run = run_window(&mut server, &env, mode, MEASURE_WINDOW);
        // The window's own off-host letters join the expected set, and the
        // verification runs after the window: reading the index can drive
        // a pre-read pass, and that must never happen inside the window.
        for (slot, letters) in expected.iter_mut().zip(&run.off_host) {
            slot.extend(
                letters
                    .iter()
                    .map(|letter: &OffHostLetter| (letter.to.clone(), letter.kind.clone())),
            );
        }
        assert_off_host_served(&env, &expected);
        for line in run.summary.lines() {
            println!("{line}");
        }
    }

    // Cleanup is Drop's business, on every path.
    drop(server);
}
