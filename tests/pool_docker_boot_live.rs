//! Against a real Docker daemon: a slot booted with the real boot script
//! reads its one-time secret from a file and removes it, takes the harness
//! environment from the creating process, and leaves the memory disk to the
//! provider. Ignored by default (needs a daemon and an image); run with
//! `cargo test --test pool_docker_boot_live -- --ignored --nocapture`.

use std::process::Command;
use std::time::{Duration, Instant};

use vivi::pool::PoolConfig;
use vivi::pool::bootscript::{SlotMode, slot_command, slot_env, slot_files};
use vivi::pool::docker::DockerProvider;
use vivi::pool::github::Secret;
use vivi::pool::provider::{CreateSpec, PoolProvider};
use vivi::pool::selftest_source::test_config;
use vivi::pool::slot::{self, PoolId};

const POOL: &str = "vivi-livetest-boot";
const TOKEN: &str = "ghs_live_test_token_value";

fn real() -> PoolConfig {
    toml::from_str(
        "repo = \"o/r\"\nslots = 2\nimage = \"ubuntu:24.04\"\nbuild_dir = \"target\"\nprovider = \"docker\"\n[github]\napp_id = 1\nprivate_key = \"k.pem\"\n",
    )
    .unwrap()
}

fn text(command: &mut Command) -> String {
    let out = command.output().unwrap();
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
#[ignore = "needs a Docker daemon"]
fn a_booted_slot_consumes_its_secret_file_and_leaves_the_mount_to_the_provider() {
    // SAFETY: this test binary holds one test, so nothing reads the
    // environment while it is set.
    unsafe { std::env::set_var("VIVI_LIVE_PROBE", "probe-value") };
    let config = test_config(&real());
    let provider = DockerProvider::new(&config);
    let mut env = slot_env(
        &config,
        &Secret::new(TOKEN.into()),
        "main",
        SlotMode {
            mounted: true,
            ..SlotMode::default()
        },
    );
    let once = env.remove("GH_TOKEN").unwrap();
    let spec = CreateSpec {
        name: PoolId::new("livetest", POOL).slot_name(1),
        image: config.image.clone(),
        metadata: slot::slot_metadata(&PoolId::new("livetest", POOL), 1, "main"),
        env,
        once_env: [("GH_TOKEN".to_string(), once)].into(),
        passthrough_env: vec!["VIVI_LIVE_PROBE".into(), "VIVI_LIVE_UNSET".into()],
        files: slot_files(&config),
        command: slot_command(),
        build_dir: config.build_dir.clone(),
        tmpfs: "64m".into(),
    };
    let id = provider.create(&spec).unwrap();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check(&provider, &id)));
    provider.destroy(&id).unwrap();
    assert!(outcome.is_ok(), "see the assertion above");
}

fn check(provider: &DockerProvider, id: &str) {
    let until = Instant::now() + Duration::from_secs(120);
    let rec = loop {
        let rec = provider.machine(id).unwrap();
        if rec.state == "stopped" {
            break rec;
        }
        assert!(Instant::now() < until, "seed did not finish");
        std::thread::sleep(Duration::from_millis(500));
    };
    assert_eq!(rec.exit_code, Some(0), "the seed ran clean");
    let files = provider
        .read_stopped(id, &["/work/once.env", "/work/seeded", "/work/boot.log"])
        .unwrap()
        .unwrap();
    assert!(files.contains_key("/work/seeded"), "{:?}", files.keys());
    assert!(
        !files.contains_key("/work/once.env"),
        "the secret file was removed"
    );
    let log = String::from_utf8_lossy(&files["/work/boot.log"]).into_owned();
    assert!(log.contains("seed_done"), "{log}");
    let inspected = text(Command::new("docker").args(["inspect", id]));
    assert!(
        !inspected.contains(TOKEN),
        "the token never reached the configuration"
    );
    assert!(
        inspected.contains("VIVI_LIVE_PROBE=probe-value"),
        "a set harness name is passed"
    );
    assert!(
        !inspected.contains("VIVI_LIVE_UNSET"),
        "an unset one is left out"
    );
    assert!(
        inspected.contains("/work/repo/build"),
        "the provider mounted the memory disk"
    );
    let logs = text(Command::new("docker").args(["logs", id]));
    assert!(
        !logs.contains("mount:"),
        "the script did not try to mount: {logs}"
    );
    println!("boot ok; arch {:?}", rec.metadata.get(slot::META_ARCH));
}
