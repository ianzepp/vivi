//! Against a real Docker daemon (the local one, or whatever `DOCKER_HOST`
//! names): the claim is atomic. Ignored by default because it needs a daemon
//! and an image; run it with
//! `cargo test --test pool_docker_live -- --ignored --nocapture`.
//!
//! It creates two containers (and claim containers) under the pool name
//! `vivi-livetest` and removes them again, also after a failure.

use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use vivi::pool::PoolConfig;
use vivi::pool::docker::DockerProvider;
use vivi::pool::provider::{CreateSpec, PoolProvider};
use vivi::pool::slot::{self, META_STATE, PoolId, STATE_READY};

const POOL: &str = "vivi-livetest";
const IMAGE: &str = "ubuntu:24.04";
const RACERS: usize = 12;

fn config() -> PoolConfig {
    toml::from_str(&format!(
        "repo = \"o/r\"\nslots = 2\nimage = \"{IMAGE}\"\nbuild_dir = \"target\"\nprovider = \"docker\"\n[github]\napp_id = 1\nprivate_key = \"k.pem\"\n"
    ))
    .unwrap()
}

fn spec(index: u32) -> CreateSpec {
    CreateSpec {
        name: PoolId::new("livetest", "", POOL).slot_name(index),
        image: IMAGE.into(),
        metadata: slot::slot_metadata(&PoolId::new("livetest", "", POOL), index, "main"),
        env: Default::default(),
        once_env: Default::default(),
        passthrough_env: Vec::new(),
        files: vec![("/work/hello.txt".into(), "hi\n".into())],
        command: vec!["true".into()],
        build_dir: Some("target".into()),
        tmpfs: "16m".into(),
    }
}

/// Removes what the test made, whatever happens.
struct Cleanup<'a>(&'a DockerProvider, Vec<String>);

impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        for id in &self.1 {
            if let Err(e) = self.0.destroy(id) {
                eprintln!("cleanup: could not remove {id}: {e}");
            }
        }
    }
}

fn wait_stopped(provider: &DockerProvider, id: &str) {
    let until = Instant::now() + Duration::from_secs(30);
    while provider.machine(id).unwrap().state != "stopped" {
        assert!(Instant::now() < until, "slot did not stop");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn free_slot(provider: &DockerProvider, index: u32) -> String {
    let id = provider.create(&spec(index)).unwrap();
    wait_stopped(provider, &id);
    provider.seal(&id, &[], META_STATE, STATE_READY).unwrap();
    id
}

#[test]
#[ignore = "needs a Docker daemon"]
fn the_claim_is_atomic_against_a_real_daemon() {
    let provider = Arc::new(DockerProvider::new(&config()));
    let mut made = Cleanup(&provider, Vec::new());
    let id = free_slot(&provider, 1);
    made.1.push(id.clone());

    let listed = provider.list().unwrap();
    let rec = listed.iter().find(|m| m.id == id).unwrap();
    assert_eq!(slot::slot_state(rec), "free", "{rec:?}");
    let arch = rec
        .metadata
        .get(slot::META_ARCH)
        .cloned()
        .unwrap_or_default();
    assert!(!arch.is_empty(), "the slot reports its architecture");
    println!("slot {id} is free, arch {arch}");

    // Many claimants, each its own `docker` process against the one daemon.
    let wins: Vec<bool> = std::thread::scope(|scope| {
        let tasks: Vec<_> = (0..RACERS)
            .map(|n| {
                let (p, id) = (Arc::clone(&provider), id.clone());
                scope.spawn(move || p.claim(&id, &format!("racer{n:03}"), 1).unwrap().is_some())
            })
            .collect();
        tasks.into_iter().map(|t| t.join().unwrap()).collect()
    });
    println!("claim race wins: {wins:?}");
    assert_eq!(wins.iter().filter(|w| **w).count(), 1, "{wins:?}");
    let held = provider.machine(&id).unwrap();
    assert!(
        held.metadata
            .get("vivi_pool_task")
            .is_some_and(|t| t.starts_with("racer"))
    );
    assert_eq!(slot::slot_state(&held), "finished-uncollected");

    // Freed, the slot can be claimed again by exactly one.
    provider.unclaim(&id).unwrap();
    assert_eq!(slot::slot_state(&provider.machine(&id).unwrap()), "free");
    assert!(provider.claim(&id, "again001", 2).unwrap().is_some());
    provider.unclaim(&id).unwrap();
}

#[test]
#[ignore = "needs a Docker daemon"]
fn creating_a_container_by_a_name_in_use_fails_for_all_but_one_separate_process() {
    let provider = DockerProvider::new(&config());
    let mut made = Cleanup(&provider, Vec::new());
    let id = free_slot(&provider, 2);
    made.1.push(id.clone());
    let name = format!("vivi-claim-{id}");
    let children: Vec<_> = (0..RACERS)
        .map(|n| {
            Command::new("docker")
                .args([
                    "create",
                    "--name",
                    &name,
                    "--label",
                    &format!("racer={n}"),
                    IMAGE,
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect();
    let codes: Vec<bool> = children
        .into_iter()
        .map(|mut c| c.wait().unwrap().success())
        .collect();
    println!("separate-process create results: {codes:?}");
    assert_eq!(codes.iter().filter(|ok| **ok).count(), 1, "{codes:?}");
}
