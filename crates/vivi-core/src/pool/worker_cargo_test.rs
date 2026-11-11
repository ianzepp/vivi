//! Exercise the real driver lifecycle without mounting macOS disk images.
use super::*;
use std::os::unix::fs::PermissionsExt;

fn tool(home: &Path, name: &str, body: &str) {
    let bin = home.join("tools");
    std::fs::create_dir_all(&bin).unwrap();
    let path = bin.join(name);
    std::fs::write(&path, format!("#!/bin/bash\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

fn cached(home: &Path, sccache: bool) -> WorkerProvider {
    cached_for(home, sccache, "ms")
}

fn cached_for(home: &Path, sccache: bool, mailspace: &str) -> WorkerProvider {
    std::fs::write(home.join("rustc-version"), "compiler-a\n").unwrap();
    tool(
        home,
        "rustc",
        &format!("cat '{}'", home.join("rustc-version").display()),
    );
    let mut cfg = config(12, "dir", 0);
    cfg.worker.cargo_target.enabled = true;
    cfg.worker.cargo_target.root = Some(home.join("build cache ' quoted").display().to_string());
    cfg.worker.cargo_target.cap_gib = 1;
    cfg.worker.cargo_target.sccache = sccache;
    WorkerProvider::with_runner(
        &cfg,
        mailspace,
        "lanes",
        Box::new(Shell {
            home: home.to_path_buf(),
            path_first: Some(home.join("tools")),
        }),
    )
    .with_seed_pacing(Duration::from_secs(20), Duration::from_millis(50))
}

const BOOT: &str = "W=$WORK; . \"$WORK/build-cache.sh\"; prepare_build_output \"$WORK\" || exit 8; env > env.txt\n";

fn target(home: &Path, index: u32) -> PathBuf {
    home.join("build cache ' quoted/slot-targets/lanes")
        .join(format!("slot-{index:02}"))
}

#[test]
fn persistent_cargo_paths_are_per_slot_padded_and_reused_after_boundary_removal() {
    let home = home("");
    let p = cached(home.path(), false);
    let first = open(&p, BOOT).unwrap();
    let second = open(&p, BOOT).unwrap();
    let a = target(home.path(), 1);
    let b = target(home.path(), 2);
    assert!(a.is_dir() && b.is_dir());
    assert_ne!(a, b);
    std::fs::write(a.join("warm"), "object").unwrap();
    let files = p
        .read_stopped(&first.id, &["/work/env.txt"])
        .unwrap()
        .unwrap();
    let env = String::from_utf8_lossy(&files["/work/env.txt"]);
    assert!(
        env.contains(&format!("CARGO_TARGET_DIR={}\n", a.display())),
        "{env}"
    );
    assert!(!env.contains("SCCACHE_SERVER_UDS="));
    p.destroy(&first.id).unwrap();
    assert!(!home.path().join("vivi-worker").join(&first.id).exists());
    assert!(a.join("warm").exists());
    assert!(!a.with_extension("lock").exists());
    let again = open(&p, BOOT).unwrap();
    assert_eq!(again.index, 1);
    assert!(a.join("warm").exists(), "same compiler preserves output");
    p.destroy(&again.id).unwrap();
    p.destroy(&second.id).unwrap();
}

#[test]
fn persistent_cargo_wipes_on_compiler_change_and_uses_checkout_toolchain() {
    let home = home("");
    let p = cached(home.path(), false);
    let slot = open(&p, BOOT).unwrap();
    let a = target(home.path(), 1);
    std::fs::write(a.join("old-object"), "old").unwrap();
    p.destroy(&slot.id).unwrap();
    std::fs::write(home.path().join("rustc-version"), "compiler-b\n").unwrap();
    let next = open(&p, BOOT).unwrap();
    assert!(!a.join("old-object").exists());
    assert_eq!(
        std::fs::read_to_string(a.join(".vivi-rustc")).unwrap(),
        "compiler-b\n"
    );
    p.destroy(&next.id).unwrap();
    tool(home.path(), "rustc", "cat rust-toolchain.toml");
    let boot = "mkdir checkout; echo checkout-compiler > checkout/rust-toolchain.toml; W=$WORK; . \"$WORK/build-cache.sh\"; prepare_build_output \"$WORK/checkout\"\n";
    let next = open(&p, boot).unwrap();
    assert_eq!(
        std::fs::read_to_string(a.join(".vivi-rustc")).unwrap(),
        "checkout-compiler\n"
    );
    p.destroy(&next.id).unwrap();
}

#[test]
fn persistent_cargo_trims_over_cap_only_when_slot_becomes_idle() {
    let home = home("");
    let p = cached(home.path(), false);
    // Report 2 GiB allocated without writing a GiB just to test a size limit.
    tool(home.path(), "du", "printf '2097152\\t%s\\n' \"$2\"");
    let slot = open(&p, BOOT).unwrap();
    let a = target(home.path(), 1);
    std::fs::write(a.join("warm"), "object").unwrap();
    assert!(a.join("warm").exists(), "never trim while the slot is held");
    let ledger = home.path().join("vivi-worker/.ledger/ms-lanes-slot-1");
    assert!(
        std::fs::read_to_string(ledger)
            .unwrap()
            .contains("cargo_cap_kib=1048576\n")
    );
    p.destroy(&slot.id).unwrap();
    assert!(
        !a.exists(),
        "over-cap target is removed, not merely cleaned"
    );
    let next = open(&p, BOOT).unwrap();
    assert!(
        a.is_dir() && !a.join("warm").exists(),
        "next job starts cold"
    );
    p.destroy(&next.id).unwrap();
}

#[test]
fn persistent_cargo_sccache_server_has_stable_tmp_cwd_and_socket() {
    let home = home("");
    let p = cached(home.path(), true);
    tool(
        home.path(),
        "sccache",
        "printf '%s|%s|%s|%s' \"$TMPDIR\" \"$PWD\" \"$SCCACHE_SERVER_UDS\" \"${VIVI_AGENT_JOB:-}\" > \"$SCCACHE_DIR/server-env\"",
    );
    let slot = open(&p, BOOT).unwrap();
    let cache = home.path().join("build cache ' quoted/sccache");
    let expected = format!(
        "{}|{}|{}|",
        cache.join("tmp").display(),
        cache.display(),
        cache.join("server.sock").display()
    );
    assert_eq!(
        std::fs::read_to_string(cache.join("server-env")).unwrap(),
        expected
    );
    let files = p
        .read_stopped(&slot.id, &["/work/env.txt"])
        .unwrap()
        .unwrap();
    let env = String::from_utf8_lossy(&files["/work/env.txt"]);
    for setting in [
        "RUSTC_WRAPPER=sccache\n",
        "CARGO_INCREMENTAL=0\n",
        "SCCACHE_SERVER_UDS=",
    ] {
        assert!(env.contains(setting), "{env}");
    }
    p.destroy(&slot.id).unwrap();
    assert!(
        cache.join("tmp").is_dir(),
        "server tmp survives job teardown"
    );
    let next = open(&p, BOOT).unwrap();
    assert_eq!(
        std::fs::read_to_string(cache.join("server-env")).unwrap(),
        expected
    );
    p.destroy(&next.id).unwrap();
}

#[test]
fn persistent_cargo_off_keeps_the_old_environment_and_creates_no_cache() {
    let home = home("");
    let p = provider(home.path(), 1, "dir", 0);
    let slot = open(&p, "test ! -f build-cache.env; env > env.txt\n").unwrap();
    let files = p
        .read_stopped(&slot.id, &["/work/env.txt"])
        .unwrap()
        .unwrap();
    let env = String::from_utf8_lossy(&files["/work/env.txt"]);
    assert!(!env.contains("CARGO_TARGET_DIR="), "{env}");
    p.destroy(&slot.id).unwrap();
    assert!(!home.path().join("vivi-worker/slot-targets").exists());
}

#[test]
fn persistent_cargo_never_shares_a_target_with_another_mailspace() {
    let home = home("");
    let p = cached(home.path(), false);
    let first = open(&p, BOOT).unwrap();
    let other_id = PoolId {
        mailspace: "other".into(),
        pool: "lanes".into(),
    };
    let mut other = spec(BOOT);
    other.metadata.insert(META_MAILSPACE.into(), "other".into());
    let other_provider = cached_for(home.path(), false, "other");
    let error = other_provider
        .open_for_dispatch(&OpenRequest {
            pool: &other_id,
            task: "t2",
            now: 1,
            budget_secs: 60,
            spec: other,
            progress: &Progress::silent(),
            ready_timeout_secs: None,
        })
        .unwrap_err()
        .to_string();
    assert!(error.contains("target is held"), "{error}");
    assert!(target(home.path(), 1).with_extension("lock").is_dir());
    assert!(
        p.machine(&first.id)
            .unwrap()
            .metadata
            .contains_key(META_POOL)
    );
    p.destroy(&first.id).unwrap();
}

#[test]
fn persistent_cargo_stale_teardown_leaves_a_new_reservation_untouched() {
    let home = home("");
    let p = cached(home.path(), false);
    tool(home.path(), "du", "printf '2097152\\t%s\\n' \"$2\"");
    let slot = open(&p, BOOT).unwrap();
    let a = target(home.path(), 1);
    std::fs::write(a.join("warm"), "new output").unwrap();
    // A teardown interrupted after freeing its reservation can leave an old
    // ledger. A later owner must not be killed, trimmed or unlocked by it.
    let owner = a.with_extension("lock").join("owner");
    std::fs::write(&owner, "new-reservation\n").unwrap();
    p.destroy(&slot.id).unwrap();
    assert!(a.join("warm").exists());
    assert_eq!(std::fs::read_to_string(owner).unwrap(), "new-reservation\n");
}

#[test]
fn persistent_cargo_default_root_is_the_parent_of_the_image_directory() {
    let home = home("");
    let images = home.path().join("build/images");
    std::fs::create_dir_all(&images).unwrap();
    std::fs::write(
        home.path().join("vivi-worker/worker.conf"),
        format!(
            "apfs_dir={}\ndir_root={}\n",
            images.display(),
            home.path().join("dirs").display()
        ),
    )
    .unwrap();
    let mut cfg = config(1, "dir", 0);
    cfg.worker.cargo_target.enabled = true;
    let p = WorkerProvider::with_runner(
        &cfg,
        "ms",
        "lanes",
        Box::new(Shell {
            home: home.path().to_path_buf(),
            path_first: None,
        }),
    );
    let slot = open(&p, "true\n").unwrap();
    let a = home.path().join("build/slot-targets/lanes/slot-01");
    assert!(a.is_dir());
    p.destroy(&slot.id).unwrap();
    assert!(a.is_dir());
}

#[test]
fn persistent_cargo_refuses_a_root_inside_the_disposable_boundary() {
    let home = home("");
    let mut cfg = config(1, "dir", 0);
    cfg.worker.cargo_target.enabled = true;
    cfg.worker.cargo_target.root = Some(
        home.path()
            .join("dirs/ms-lanes-slot-1/escaped")
            .display()
            .to_string(),
    );
    let p = WorkerProvider::with_runner(
        &cfg,
        "ms",
        "lanes",
        Box::new(Shell {
            home: home.path().to_path_buf(),
            path_first: None,
        }),
    );
    let error = open(&p, "true\n").unwrap_err().to_string();
    assert!(error.contains("root is inside a job boundary"), "{error}");
    assert!(p.list().unwrap().is_empty());
}
