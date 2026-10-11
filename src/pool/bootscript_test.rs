use super::*;
use crate::pool::config::PoolConfig;
use crate::pool::{collect, job};

fn config() -> PoolConfig {
    toml::from_str(
        r#"
app = "a"
repo = "acme/widgets"
slots = 2
image = "debian:12"
build_dir = "out/cache"
tmpfs = "8g"
setup = ["apt-get install -y make", "echo done"]
seed = ["make warm"]
[github]
app_id = 1
private_key = "k.pem"
"#,
    )
    .unwrap()
}

#[test]
fn commands_render_as_a_failing_fast_script() {
    let script = render_commands(&["a".to_string(), "b \\\n  c".to_string()]);
    assert_eq!(script, "#!/bin/bash\nset -e\na\nb \\\n  c\n");
    assert_eq!(render_commands(&[]), "#!/bin/bash\nset -e\n");
}

#[test]
fn slot_files_hold_boot_setup_and_seed() {
    let files = slot_files(&config());
    let paths: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(paths, ["/work/boot.sh", "/work/setup.sh", "/work/seed.sh"]);
    assert!(files[1].1.contains("apt-get install -y make\necho done\n"));
    assert!(files[2].1.contains("make warm"));
}

#[test]
fn env_is_parameterised_not_baked_into_the_script() {
    let token = crate::pool::github::Secret::new("tok".to_string());
    let env = slot_env(&config(), &token, &"b".repeat(40));
    assert_eq!(env["REPO"], "acme/widgets");
    assert_eq!(env["BUILD_DIR"], "out/cache");
    assert_eq!(env["TMPFS_SIZE"], "8g");
    assert_eq!(env["GH_TOKEN"], "tok");
    assert_eq!(env["BASE"], "b".repeat(40));
}

#[test]
fn boot_script_is_generic_and_keeps_the_job_path() {
    for project_word in ["faber", "radix", "cargo", "node", "pi-coding", "ZAI", "npm"] {
        assert!(
            !BOOT_SCRIPT.contains(project_word),
            "script mentions {project_word}"
        );
    }
    for needed in [
        "/work/setup.sh",
        "/work/seed.sh",
        "/work/job.tgz",
        "/work/job/run.sh",
        "/work/job/limit",
        "timeout -k 30",
        "useradd -m hand",
        "mount -t tmpfs",
        "$BUILD_DIR",
        "$TMPFS_SIZE",
        "no_job_idle_stop",
    ] {
        assert!(BOOT_SCRIPT.contains(needed), "script lacks {needed}");
    }
    assert!(BOOT_SCRIPT.starts_with("#!/bin/bash"));
}

#[test]
fn boot_script_has_valid_bash_syntax() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("boot.sh");
    std::fs::write(&path, BOOT_SCRIPT).unwrap();
    let Ok(status) = std::process::Command::new("bash")
        .arg("-n")
        .arg(&path)
        .status()
    else {
        return;
    };
    assert!(status.success());
}

#[test]
fn boot_script_bounds_every_job_and_keeps_results_until_replaced() {
    // Every job runs under the hard limit, with a default when none is given.
    assert!(BOOT_SCRIPT.contains(r#"timeout -k 30 "$LIMIT" bash /work/job/run.sh"#));
    assert!(BOOT_SCRIPT.contains("LIMIT=3600"));
    // The job directory is only replaced by a new job, never wiped at boot.
    let wipes: Vec<&str> = BOOT_SCRIPT
        .lines()
        .filter(|l| l.contains("rm -rf /work/job"))
        .collect();
    assert_eq!(wipes, ["rm -rf /work/job"], "one wipe, at new-job unpack");
    let unpack = BOOT_SCRIPT.find("rm -rf /work/job").unwrap();
    let new_job = BOOT_SCRIPT.find("# New job.").unwrap();
    assert!(unpack > new_job, "the wipe sits inside the new-job path");
    // A collect boot (same archive) does not run the job again.
    assert!(BOOT_SCRIPT.contains("collect_boot"));
    // The archive copy is removed before the agent can exist.
    assert!(BOOT_SCRIPT.contains(r#"rm -f "$JT"; echo "$SHA" > /work/job.sha"#));
    // A killed job still leaves a result with its job id.
    assert!(BOOT_SCRIPT.contains("state=killed"));
}

#[test]
fn job_runner_is_generic_runs_as_hand_and_never_forces_a_push() {
    for project_word in ["faber", "radix", "cargo", "rustup", "pi ", "ZAI", "npm"] {
        assert!(
            !job::RUNNER.contains(project_word),
            "runner mentions {project_word}"
        );
    }
    assert!(job::RUNNER.contains("runuser -u hand"));
    assert!(job::RUNNER.contains("env -i"));
    assert!(job::RUNNER.contains(r#"timeout -k 15 "$BUDGET""#));
    assert!(job::RUNNER.contains("HEAD:refs/heads/$BRANCH"));
    assert!(!job::RUNNER.contains("--force"));
    assert!(!job::RUNNER.contains("push -f"));
    // The Git token is read into the runner only, not into the agent's env.
    assert!(!job::RUNNER.contains("GH_TOKEN"));
    assert!(job::RUNNER.contains("finish no_changes"));
    assert!(job::RUNNER.contains("finish push_refused"));
    assert!(job::RUNNER.contains("Pool-Run: slot=$SLOT model=$MODEL"));
}

#[test]
fn job_runner_has_valid_bash_syntax() {
    for (name, script) in [
        ("run.sh", job::RUNNER),
        ("collect.sh", collect::COLLECT_SCRIPT),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, script).unwrap();
        let Ok(status) = std::process::Command::new("bash")
            .arg("-n")
            .arg(&path)
            .status()
        else {
            return;
        };
        assert!(status.success(), "{name}");
    }
}
