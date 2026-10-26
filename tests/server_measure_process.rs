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
//! alive, and is killed and reaped on drop.

use std::panic::AssertUnwindSafe;
use std::time::{Duration, Instant};

use vivi::registry::{Flags, Registry, save_at};
use vivi::server::projects::{IndexOutcome, Viewer};

#[path = "support/hosted_measure.rs"]
mod hosted_measure;
#[path = "support/hosted_process.rs"]
mod hosted_process;

use hosted_measure::{HostedMeasure, MIN_MEASURE_SENDS, OFF_HOST};
use hosted_process::{CpuDelta, CpuSampler, HostedServer, Sample, ServerSpec};

/// The smoke corpus: far below the measurement minimum, on purpose.
const SMOKE_SENDS: usize = 24;
/// Local-only letters written into each host before the mixed pass.
const SMOKE_LOCAL_ONLY: usize = 6;
/// A tiny cadence so the periodic schedule fires a few off-host letters
/// inside a seconds-long pass; a real run uses the two-second default.
const SMOKE_EVERY: Duration = Duration::from_millis(20);

fn smoke_sends() -> usize {
    std::env::var("VIVI_MEASURE_SENDS")
        .ok()
        .and_then(|raw| raw.parse().ok())
        .filter(|sends| (1..SMOKE_SENDS).contains(sends))
        .unwrap_or(SMOKE_SENDS)
}

fn viewer_of<'a>(host: &'a hosted_measure::MeasureHost) -> Viewer<'a> {
    Viewer {
        name: &host.member,
        prefix: Some(&host.prefix),
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

    // What the server serves is the fixture's registry decision: register
    // both hosted roots `serve = true` in the fixture's own registry file,
    // which the child reads through its isolated VIVI_CONFIG_DIR.
    let config = env.base.join("config");
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

    let mut server = HostedServer::spawn(ServerSpec::new(env.data.clone(), config));
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

    // Cleanup: dropping the handle kills and reaps on the success path;
    // Drop covers the failure and unwind paths the same way.
    drop(server);
    assert!(
        !hosted_process::process_alive(pid),
        "server pid {pid} was not reaped"
    );
}
