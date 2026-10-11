use std::collections::BTreeMap;

use super::*;
use crate::pool::config::PoolConfig;
use crate::pool::{collect, job};

fn config() -> PoolConfig {
    toml::from_str(
        r#"
repo = "acme/widgets"
slots = 2
image = "debian:12"
build_dir = "out/cache"
setup = ["apt-get install -y make", "echo done"]
seed = ["make warm"]
[fly]
app = "a"
tmpfs = "8g"
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
    let env = slot_env(&config(), &token, &"b".repeat(40), SlotMode::default());
    assert_eq!(env["REMOTE"], "https://github.com/acme/widgets.git");
    assert!(!env.contains_key("REPO"));
    assert!(!env.contains_key("SOURCE_DIR"));
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
        r#""$W/setup.sh""#,
        r#""$W/seed.sh""#,
        "JT=$W/job.tgz",
        r#""$W/job/run.sh""#,
        r#""$W/job/limit""#,
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
    assert!(BOOT_SCRIPT.contains(r#"timeout -k 30 "$LIMIT" bash "$W/job/run.sh""#));
    assert!(BOOT_SCRIPT.contains("LIMIT=3600"));
    // The job directory is only replaced by a new job, never wiped at boot.
    let wipes: Vec<&str> = BOOT_SCRIPT
        .lines()
        .filter(|l| l.contains(r#"rm -rf "$W/job""#))
        .collect();
    assert_eq!(wipes, [r#"rm -rf "$W/job""#], "one wipe, at new-job unpack");
    let unpack = BOOT_SCRIPT.find(r#"rm -rf "$W/job""#).unwrap();
    let new_job = BOOT_SCRIPT.find("# New job.").unwrap();
    assert!(unpack > new_job, "the wipe sits inside the new-job path");
    // A collect boot (same archive) does not run the job again, and a boot
    // with nothing to run stops itself within the idle limit.
    assert!(BOOT_SCRIPT.contains("collect_boot"));
    assert!(BOOT_SCRIPT.contains("IDLE_LIMIT=${IDLE_LIMIT:-60}"));
    assert_eq!(BOOT_SCRIPT.matches(r#"sleep "$IDLE_LIMIT""#).count(), 2);
    // The archive copy is removed before the agent can exist.
    assert!(BOOT_SCRIPT.contains(r#"rm -f "$JT"; echo "$SHA" > "$W/job.sha""#));
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

fn embedded(config: &PoolConfig) -> PoolConfig {
    crate::pool::selftest_source::test_config(config)
}

#[test]
fn a_github_pool_sends_the_token_and_an_https_remote() {
    let token = crate::pool::github::Secret::new("tok".to_string());
    let env = slot_env(&config(), &token, "main", SlotMode::default());
    assert_eq!(env["REMOTE"], "https://github.com/acme/widgets.git");
    assert_eq!(env["GH_TOKEN"], "tok");
    assert!(!env.contains_key("SOURCE_DIR"));
    assert_eq!(slot_files(&config()).len(), 3);
}

#[test]
fn an_embedded_source_is_delivered_with_a_file_remote_and_no_token() {
    let config = embedded(&config());
    let empty = crate::pool::github::Secret::new(String::new());
    let env = slot_env(&config, &empty, "main", SlotMode::default());
    assert_eq!(env["REMOTE"], "file:///work/remote.git");
    assert_eq!(env["SOURCE_DIR"], SOURCE_DIR);
    assert_eq!(env["BASE"], "main");
    assert!(!env.contains_key("GH_TOKEN"), "{env:?}");
    assert!(env.contains_key("LINGER") && env.contains_key("IDLE_LIMIT"));
    let files = slot_files(&config);
    let paths: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
    assert!(paths.contains(&"/work/source/README.md"), "{paths:?}");
    assert!(paths.contains(&"/work/source/notes.txt"));
    assert!(paths.contains(&"/work/source/.gitignore"));
    let ignore = files
        .iter()
        .find(|(p, _)| p.ends_with(".gitignore"))
        .unwrap();
    assert_eq!(ignore.1, "build/\n", "the build mount must not be cleaned");
}

#[test]
fn the_boot_script_turns_the_delivered_source_into_a_bare_remote() {
    for needed in [
        "remote_url()",
        r#"git init -q --bare "$LOCAL_REMOTE""#,
        r#"symbolic-ref HEAD refs/heads/main"#,
        r#"git push -q "$LOCAL_REMOTE" HEAD:refs/heads/main"#,
        r#"git fetch -q --depth 1 "$(remote_url)" "$BASE""#,
        r#"file://*) LOCAL_REMOTE=$(real "${REMOTE#file://}")"#,
        "seed_failed source",
        r#"chown -R hand:hand /home/hand "$RD" ${LOCAL_REMOTE:+"$LOCAL_REMOTE"}"#,
    ] {
        assert!(BOOT_SCRIPT.contains(needed), "script lacks {needed}");
    }
    assert!(!BOOT_SCRIPT.contains("github.com"), "no hard-wired host");
    assert!(!BOOT_SCRIPT.contains("${REPO}"));
}

#[test]
fn the_runner_takes_a_full_remote_and_adds_the_token_only_to_https() {
    for needed in [
        r#"URL=$REMOTE"#,
        r#"https://*) [ -n "$TOKEN" ] && URL="https://x-access-token:${TOKEN}@${REMOTE#https://}""#,
        r#"git fetch -q --depth "${DEPTH:-1}" "$URL" "$FROM""#,
        r#"git push -q "$URL" "HEAD:refs/heads/$BRANCH""#,
    ] {
        assert!(job::RUNNER.contains(needed), "runner lacks {needed}");
    }
    assert!(!job::RUNNER.contains("github.com"));
    assert!(!job::RUNNER.contains("${REPO}"));
}

fn slot_env_for(provider: &str, mounted: bool) -> BTreeMap<String, String> {
    let mut config = config();
    config.provider = provider.to_string();
    config.docker.tmpfs = "2g".into();
    let token = crate::pool::github::Secret::new(String::new());
    let mode = SlotMode {
        mounted,
        ..SlotMode::default()
    };
    slot_env(&config, &token, "main", mode)
}

#[test]
fn a_host_that_mounts_the_memory_disk_says_so_and_sizes_it_from_its_own_table() {
    let docker = slot_env_for("docker", true);
    assert_eq!(docker["BUILD_MOUNTED"], "1");
    assert_eq!(docker["TMPFS_SIZE"], "2g");
    let fly = slot_env_for("fly", false);
    assert!(!fly.contains_key("BUILD_MOUNTED"));
    assert_eq!(fly["TMPFS_SIZE"], "8g");
}

/// The text of the shell function `name` in the boot script.
fn function_text(name: &str) -> String {
    let start = BOOT_SCRIPT.find(&format!("{name}() {{")).unwrap();
    let end = BOOT_SCRIPT[start..].find("\n}\n").unwrap() + 3;
    BOOT_SCRIPT[start..start + end].to_string()
}

/// Run `mount_build` in `dir` with a `mount` that only writes down its call;
/// `host_flag` names the variable the host sets, if any.
fn run_mount_build(dir: &std::path::Path, host_flag: Option<&str>) -> Option<(bool, String)> {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let stub = bin.join("mount");
    std::fs::write(&stub, "#!/bin/sh\necho \"$@\" >> \"$MOUNT_LOG\"\n").unwrap();
    std::fs::set_permissions(&stub, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let log = dir.join("mount.log");
    let script = format!("{}\nmount_build\n", function_text("mount_build"));
    let mut command = std::process::Command::new("bash");
    command
        .current_dir(dir)
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("MOUNT_LOG", &log)
        .env("BUILD_DIR", "out/cache")
        .env("TMPFS_SIZE", "64m")
        .env_remove("BUILD_MOUNTED")
        .env_remove("BUILD_PERSISTS")
        .arg("-c")
        .arg(script);
    if let Some(flag) = host_flag {
        command.env(flag, "1");
    }
    let status = command.status().ok()?;
    Some((
        status.success() && dir.join("out/cache").is_dir(),
        std::fs::read_to_string(log).unwrap_or_default(),
    ))
}

#[test]
fn the_boot_script_mounts_the_memory_disk_unless_the_host_already_did() {
    let dir = tempfile::tempdir().unwrap();
    let Some((made_dir, called)) = run_mount_build(dir.path(), None) else {
        return;
    };
    assert!(made_dir);
    assert_eq!(called.trim(), "-t tmpfs -o size=64m tmpfs out/cache");
    let dir = tempfile::tempdir().unwrap();
    let (made_dir, called) = run_mount_build(dir.path(), Some("BUILD_MOUNTED")).unwrap();
    assert!(made_dir, "the directory still has to exist");
    assert_eq!(called, "", "a host-mounted memory disk is left alone");
}

#[test]
fn a_persistent_build_directory_gets_no_memory_disk() {
    let dir = tempfile::tempdir().unwrap();
    let (made_dir, called) = run_mount_build(dir.path(), Some("BUILD_PERSISTS")).unwrap();
    assert!(made_dir, "the directory still has to exist");
    assert_eq!(called, "", "a plain directory is not mounted over");
}

#[test]
fn both_mount_sites_of_the_boot_script_go_through_the_conditional() {
    assert_eq!(BOOT_SCRIPT.matches("mount -t tmpfs").count(), 1);
    assert_eq!(BOOT_SCRIPT.matches("mount_build\n").count(), 2);
    assert!(BOOT_SCRIPT.contains(r#"[ -n "${BUILD_MOUNTED:-}${BUILD_PERSISTS:-}" ] ||"#));
    assert!(BOOT_SCRIPT.contains("RD=$W/repo"));
    assert!(BOOT_SCRIPT.contains("W=${WORK:-/work}"));
    assert_eq!(CHECKOUT_DIR, "/work/repo");
}

#[test]
fn a_one_time_secret_file_is_read_then_removed_before_anything_uses_it() {
    let line = BOOT_SCRIPT
        .lines()
        .find(|l| l.contains("once.env") && l.starts_with("if "))
        .unwrap();
    assert!(
        line.contains(r#". "$W/once.env"; rm -f "$W/once.env""#),
        "{line}"
    );
    let first_use = BOOT_SCRIPT.find("remote_url()").unwrap();
    assert!(BOOT_SCRIPT.find(line).unwrap() < first_use);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("once.env");
    std::fs::write(&file, "export GH_TOKEN='it'\\''s'\n").unwrap();
    let probe = format!("W={:?}\n{}", dir.path(), line);
    let Ok(out) = std::process::Command::new("bash")
        .arg("-c")
        .arg(format!("{probe}\nprintf '%s' \"$GH_TOKEN\""))
        .output()
    else {
        return;
    };
    assert_eq!(String::from_utf8_lossy(&out.stdout), "it's");
    assert!(!file.exists(), "the secret file is gone once read");
}

#[test]
fn a_pool_without_a_build_dir_has_no_build_handling_at_all() {
    let mut config = config();
    config.build_dir = None;
    let token = crate::pool::github::Secret::new(String::new());
    let env = slot_env(&config, &token, "main", SlotMode::default());
    for name in [
        "BUILD_DIR",
        "TMPFS_SIZE",
        "BUILD_MOUNTED",
        "BUILD_PERSISTS",
        "WORKTREE_OF",
    ] {
        assert!(!env.contains_key(name), "{name} set: {env:?}");
    }
    config.build_dir = Some("out/cache".into());
    let env = slot_env(&config, &token, "main", SlotMode::default());
    assert_eq!(env["BUILD_DIR"], "out/cache");
    assert!(env.contains_key("TMPFS_SIZE"));
}

#[test]
fn only_a_local_worktree_pool_names_its_repository_in_the_slot_environment() {
    let token = crate::pool::github::Secret::new(String::new());
    let mut config: PoolConfig = toml::from_str(
        "provider = \"local\"\nslots = 1\n[local]\nroot = \"/r\"\nworktree_of = \"/r/project\"\n",
    )
    .unwrap();
    let env = slot_env(&config, &token, "abc", SlotMode::default());
    assert_eq!(env["WORKTREE_OF"], "/r/project");
    assert_eq!(env["REMOTE"], "", "no remote: nothing is fetched");
    config.local.worktree_of = None;
    assert!(!slot_env(&config, &token, "abc", SlotMode::default()).contains_key("WORKTREE_OF"));
    let mut elsewhere = self::config();
    elsewhere.provider = "ssh".into();
    elsewhere.local.worktree_of = Some("/r/project".into());
    assert!(!slot_env(&elsewhere, &token, "abc", SlotMode::default()).contains_key("WORKTREE_OF"));
}

#[test]
fn the_boot_script_handles_the_build_directory_only_when_it_is_named() {
    let mount = function_text("mount_build");
    assert!(mount.contains("BUILD_DIR"), "{mount}");
    let dir = tempfile::tempdir().unwrap();
    let ran = run_mount_build_without_build_dir(dir.path());
    assert!(ran, "mount_build must return cleanly with no BUILD_DIR");
    assert!(
        !dir.path().join("mount.log").exists(),
        "no mount without a build dir"
    );
}

fn run_mount_build_without_build_dir(dir: &std::path::Path) -> bool {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let stub = bin.join("mount");
    std::fs::write(&stub, "#!/bin/sh\necho \"$@\" >> \"$MOUNT_LOG\"\n").unwrap();
    std::fs::set_permissions(&stub, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let script = format!("set -e\n{}\nmount_build\n", function_text("mount_build"));
    std::process::Command::new("bash")
        .current_dir(dir)
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("MOUNT_LOG", dir.join("mount.log"))
        .env_remove("BUILD_DIR")
        .env("BUILD_MOUNTED", "1")
        .args(["-c", &script])
        .status()
        .unwrap()
        .success()
}
