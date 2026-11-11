//! The clean agent environment must explicitly carry the worker cache settings.
use super::*;
use crate::pool::job::sh_quote;
use crate::pool::ssh_script::WORKER_CARGO_SCRIPT;

fn install(rig: &Rig, target: &Path, sccache: bool) {
    std::fs::write(rig.work.path().join("build-cache.sh"), WORKER_CARGO_SCRIPT).unwrap();
    let mut env = format!(
        "CARGO_TARGET_DIR={}\nVIVI_CARGO_SCCACHE={}\n",
        sh_quote(&target.display().to_string()),
        u8::from(sccache)
    );
    if sccache {
        let cache = target.parent().unwrap().join("sccache");
        std::fs::create_dir_all(cache.join("tmp")).unwrap();
        env.push_str(&format!(
            "RUSTC_WRAPPER=sccache\nSCCACHE_DIR={}\nSCCACHE_SERVER_UDS={}\nCARGO_INCREMENTAL=0\n",
            sh_quote(&cache.display().to_string()),
            sh_quote(&cache.join("server.sock").display().to_string())
        ));
        // A shell function stands in for the daemon, inherited only by the
        // runner. The harness below is env -i and cannot inherit this function.
        env.push_str("sccache() { test \"$TMPDIR\" = \"$SCCACHE_DIR/tmp\"; }; export -f sccache\n");
    }
    std::fs::write(rig.work.path().join("build-cache.env"), env).unwrap();
}

#[test]
fn persistent_cargo_harness_environment_on_off_and_sccache() {
    for enabled in [false, true] {
        for sccache in [false, true] {
            let rig = Rig::new();
            let cache = tempfile::tempdir().unwrap();
            let target = cache.path().join("slot-targets/lanes/slot-01");
            if enabled {
                install(&rig, &target, sccache);
            }
            let output = rig.work.path().join("cache-env");
            let harness = format!(
                "printf '%s|%s|%s|%s|%s' \"${{CARGO_TARGET_DIR:-}}\" \"${{RUSTC_WRAPPER:-}}\" \"${{SCCACHE_DIR:-}}\" \"${{CARGO_INCREMENTAL:-}}\" \"$TMPDIR\" > {}",
                sh_quote(&output.display().to_string())
            );
            let result = rig.run(&harness, "build");
            assert_eq!(result.get("agent_rc"), "0", "{:?}", result.fields);
            let settings = std::fs::read_to_string(output).unwrap();
            let fields: Vec<_> = settings.split('|').collect();
            assert_eq!(
                fields[0],
                if enabled {
                    target.to_str().unwrap()
                } else {
                    ""
                }
            );
            assert_eq!(fields[1], if enabled && sccache { "sccache" } else { "" });
            assert_eq!(fields[3], if enabled && sccache { "0" } else { "" });
            assert_eq!(fields[2].is_empty(), !enabled || !sccache);
            assert_eq!(
                fields[4],
                rig.work.path().join("tmp").to_str().unwrap(),
                "the job still uses its own temporary directory"
            );
        }
    }
}

#[test]
fn persistent_cargo_overrides_generic_workspace_environment() {
    let rig = Rig::new();
    let cache = tempfile::tempdir().unwrap();
    let target = cache.path().join("slot-01");
    install(&rig, &target, false);
    let output = rig.work.path().join("target-seed-env");
    // Prove generic workspace overrides
    // cannot redirect the persistent target back into the disposable image.
    let settings = "RAMDISK_ENV_COUNT=1\nRAMDISK_ENV_0_NAME=CARGO_TARGET_DIR\nRAMDISK_ENV_0_VALUE=/wrong-target\n";
    let harness = format!(
        "printf '%s' \"$CARGO_TARGET_DIR\" > {}",
        sh_quote(&output.display().to_string())
    );
    let result = rig.run_with_ramdisk_settings(&harness, &[], settings);
    assert_eq!(result.get("agent_rc"), "0", "{:?}", result.fields);
    assert_eq!(
        std::fs::read_to_string(output).unwrap(),
        target.display().to_string()
    );
    assert!(target.join(".vivi-rustc").is_file());
}

#[test]
fn persistent_cargo_is_ready_before_repository_seed_runs() {
    let rig = Rig::new();
    let cache = tempfile::tempdir().unwrap();
    let target = cache.path().join("slot-01");
    install(&rig, &target, false);
    let output = rig.work.path().join("seed-target");
    let seed = format!(
        "test -f \"$CARGO_TARGET_DIR/.vivi-rustc\" && printf '%s' \"$CARGO_TARGET_DIR\" > {}",
        sh_quote(&output.display().to_string())
    );
    std::fs::write(rig.work.path().join("fixture-target-seed.sh"), seed).unwrap();
    let result = rig.run_in_context(
        "true",
        "build",
        &[],
        &rig.f.repo,
        Some(("a", &"a".repeat(64))),
    );
    assert_eq!(result.get("agent_rc"), "0", "{:?}", result.fields);
    assert_eq!(
        std::fs::read_to_string(output).unwrap(),
        target.display().to_string()
    );
}

#[test]
fn persistent_cargo_compiler_probe_failure_is_safe_to_free_before_agent_runs() {
    let rig = Rig::new();
    let cache = tempfile::tempdir().unwrap();
    let target = cache.path().join("slot-01");
    install(&rig, &target, false);
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("warm"), "old output").unwrap();
    let env_path = rig.work.path().join("build-cache.env");
    let mut env = std::fs::read_to_string(&env_path).unwrap();
    env.push_str("rustc() { return 99; }; export -f rustc\n");
    std::fs::write(env_path, env).unwrap();
    let result = rig.run("touch should-not-run", "build");
    assert_eq!(
        result.push(),
        "target_prepare_failed",
        "{:?}",
        result.fields
    );
    assert!(
        target.join("warm").is_file(),
        "never wipe without identifying rustc"
    );
    assert!(!rig.repo().join("should-not-run").exists());
    assert!(crate::pump::envfail::HOST_PUSH_OUTCOMES.contains(&result.push()));
}
