//! The boot script and the job runner in the unprivileged, persistent-build
//! mode of a directory slot, run for real under bash.
//!
//! The scripts get a PATH of their own that holds only the ordinary tools
//! they may use. `zstd` and `curl` do not exist there, so a script that
//! required them would fail its seed; `useradd`, `runuser`, `apt-get`,
//! `mount`, `chown` and `pkill` exist only to write down that they were
//! called, and every test ends by checking that none was.

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::*;
use crate::pool::config::PoolConfig;
use crate::pool::dispatch::{self, DispatchFlags};
use crate::pool::github::Secret;
use crate::pool::selftest_source as source;
use crate::pool::{job, ssh_script};

/// Tools the scripts may use, linked into the private PATH.
const ALLOWED: [&str; 32] = [
    "bash", "sh", "git", "date", "awk", "cat", "tar", "rm", "mkdir", "sed", "head", "tr", "cut",
    "du", "mv", "touch", "sleep", "printf", "env", "sync", "chmod", "cp", "dirname", "basename",
    "ls", "grep", "uname", "id", "pwd", "wc", "sort", "gzip",
];

fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

fn write_tool(bin: &Path, name: &str, body: &str) {
    let path = bin.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A directory holding the allowed tools and three stand-ins for the GNU
/// ones a Mac lacks: `timeout` (runs the command, no limit), `sha256sum`,
/// `nproc`.
fn private_bin(root: &Path) -> PathBuf {
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    for name in ALLOWED {
        let found = find_on_path(name).unwrap_or_else(|| panic!("no `{name}` on PATH"));
        std::os::unix::fs::symlink(found, bin.join(name)).unwrap();
    }
    write_tool(
        &bin,
        "timeout",
        "while [ \"$1\" = -k ]; do shift 2; done; shift; exec \"$@\"",
    );
    write_tool(
        &bin,
        "sha256sum",
        "shasum -a 256 \"$@\" 2>/dev/null || /sbin/sha256sum \"$@\"",
    );
    write_tool(&bin, "nproc", "echo 1");
    // Privileged tools exist only to write down that they were called.
    for name in PRIVILEGED {
        let log = root.join("privileged.log");
        write_tool(
            &bin,
            name,
            &format!("echo \"{name} $*\" >> {}; exit 99", log.display()),
        );
    }
    bin
}

/// What must never be called: it needs privileges the login user lacks.
const PRIVILEGED: [&str; 6] = ["useradd", "runuser", "apt-get", "mount", "chown", "pkill"];

fn privileged_calls(root: &Path) -> String {
    std::fs::read_to_string(root.join("privileged.log")).unwrap_or_default()
}

fn embedded_config(seed: &str) -> PoolConfig {
    let real: PoolConfig = toml::from_str(
        "repo = \"o/r\"\nslots = 1\nbuild_dir = \"target\"\nprovider = \"ssh\"\n[ssh]\nhost = \"h\"\n[github]\napp_id = 1\nprivate_key = \"k.pem\"\n",
    )
    .unwrap();
    let mut config = source::test_config(&real);
    config.seed = vec![seed.to_string()];
    config
}

const MODE: SlotMode = SlotMode {
    mounted: false,
    persists: true,
    unprivileged: true,
};

struct Slot {
    work: PathBuf,
    home: PathBuf,
    bin: PathBuf,
    env: BTreeMap<String, String>,
}

/// Lay a slot out the way the `ssh` provider does: the files of the config
/// below the slot directory, the environment as the boot script is given it.
fn lay_out(root: &Path, config: &PoolConfig) -> Slot {
    let work = root.join("slot");
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    for (path, text) in slot_files(config) {
        let target = work.join(path.strip_prefix("/work/").unwrap());
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(target, text).unwrap();
    }
    let mut env = slot_env(config, &Secret::new(String::new()), "main", MODE);
    env.insert("LINGER".into(), "0".into());
    env.insert("IDLE_LIMIT".into(), "0".into());
    Slot {
        work,
        home,
        bin: private_bin(root),
        env,
    }
}

fn boot(slot: &Slot) -> std::process::Output {
    Command::new(slot.bin.join("bash"))
        .arg(slot.work.join("boot.sh"))
        .current_dir(&slot.work)
        .env_clear()
        .env("PATH", &slot.bin)
        .env("HOME", &slot.home)
        .env("WORK", &slot.work)
        .env("VIVI_SLOT_ROOT", &slot.work)
        .envs(&slot.env)
        .output()
        .unwrap()
}

fn said(out: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn the_seed_runs_as_the_login_user_installs_nothing_and_keeps_the_build_directory() {
    let root = tempfile::tempdir().unwrap();
    let config = embedded_config("mkdir -p build && echo warm > build/warm.txt");
    let slot = lay_out(root.path(), &config);
    let out = boot(&slot);
    let log = std::fs::read_to_string(slot.work.join("boot.log")).unwrap_or_default();
    assert!(out.status.success(), "{} {log}", said(&out));
    assert!(slot.work.join("seeded").is_file(), "{log}");
    assert!(!log.contains("seed_failed"), "{log}");
    let warm = slot.work.join("repo/build/warm.txt");
    assert_eq!(std::fs::read_to_string(warm).unwrap(), "warm\n");
    assert!(
        !slot.work.join("build.tar.zst").exists(),
        "nothing is packed"
    );
    assert!(!log.contains("pack_s"), "{log}");
    assert_eq!(
        privileged_calls(root.path()),
        "",
        "no privileged tool was called"
    );
    let remote = slot.work.join("remote.git");
    assert_eq!(git(&remote, &["rev-parse", "--abbrev-ref", "HEAD"]), "main");
    assert_eq!(
        git(&slot.work.join("repo"), &["rev-parse", "HEAD"]),
        std::fs::read_to_string(slot.work.join("seed-base.txt"))
            .unwrap()
            .trim()
    );
}

#[test]
fn a_job_runs_unprivileged_pushes_and_leaves_the_warm_build_in_place() {
    let root = tempfile::tempdir().unwrap();
    let config = embedded_config("mkdir -p build && echo warm > build/warm.txt");
    let slot = lay_out(root.path(), &config);
    assert!(boot(&slot).status.success());
    let plan = dispatch::plan_job(
        &config,
        "p",
        ("task-1", "subject", ""),
        &DispatchFlags {
            model: source::MODEL.to_string(),
            from: Some("main".into()),
            branch: Some("work/one".into()),
            budget: Some(60),
            allow_default_branch: true,
        },
        job::DEFAULT_RULES.to_string(),
        "job000000001".to_string(),
    )
    .unwrap();
    let archive = job::pack(&job::build_bundle(
        &plan,
        "p-slot-1",
        &Secret::new("tok".into()),
    ));
    std::fs::write(slot.work.join("job.tgz"), archive.unwrap()).unwrap();
    let out = boot(&slot);
    let job_dir = slot.work.join("job");
    let result = std::fs::read_to_string(job_dir.join("result")).unwrap_or_default();
    let run_log = std::fs::read_to_string(job_dir.join("run.log")).unwrap_or_default();
    assert!(out.status.success(), "{} {run_log}", said(&out));
    assert!(result.contains("push=pushed"), "{result}\n{run_log}");
    assert!(result.contains("branch=work/one"), "{result}");
    let remote = slot.work.join("remote.git");
    assert!(!git(&remote, &["rev-parse", "refs/heads/work/one"]).is_empty());
    let pushed = git(&remote, &["show", "refs/heads/work/one:standin.txt"]);
    assert!(pushed.starts_with("stand-in standin"), "{pushed}");
    // The agent's inputs sit in the slot, not in a home that does not exist.
    assert!(slot.work.join("home/brief.md").is_file());
    assert!(slot.work.join("home/rules.md").is_file());
    // The Git token is not left in the job directory the agent can read.
    assert!(!job_dir.join("token").exists());
    // The warm build is still there, and nothing was restored or packed.
    assert!(slot.work.join("repo/build/warm.txt").is_file());
    assert!(!slot.work.join("build.tar.zst").exists());
    assert_eq!(
        privileged_calls(root.path()),
        "",
        "no privileged tool was called"
    );
}

#[test]
fn a_second_job_starts_from_a_clean_tree_but_keeps_an_unignored_build_directory() {
    let root = tempfile::tempdir().unwrap();
    let mut config = embedded_config("mkdir -p build && echo warm > build/warm.txt");
    // A repository that does not ignore its build output: the clean must
    // spare the directory the slot keeps warm.
    config
        .embedded
        .as_mut()
        .unwrap()
        .files
        .retain(|(p, _)| !p.ends_with(".gitignore"));
    let slot = lay_out(root.path(), &config);
    assert!(boot(&slot).status.success());
    std::fs::write(slot.work.join("repo/stray.txt"), "left by the last task").unwrap();
    let archive = one_job_archive(&config, "job000000002");
    std::fs::write(slot.work.join("job.tgz"), archive).unwrap();
    boot(&slot);
    let result = std::fs::read_to_string(slot.work.join("job/result")).unwrap_or_default();
    assert!(result.contains("state=done"), "{result}");
    assert!(
        !slot.work.join("repo/stray.txt").exists(),
        "a stray file is cleaned"
    );
    assert!(
        slot.work.join("repo/build/warm.txt").is_file(),
        "the warm build stays"
    );
    assert_eq!(
        privileged_calls(root.path()),
        "",
        "no privileged tool was called"
    );
}

fn one_job_archive(config: &PoolConfig, id: &str) -> Vec<u8> {
    let plan = dispatch::plan_job(
        config,
        "p",
        ("task-2", "subject", ""),
        &DispatchFlags {
            model: source::MODEL.to_string(),
            from: Some("main".into()),
            branch: Some("work/two".into()),
            budget: Some(60),
            allow_default_branch: true,
        },
        job::DEFAULT_RULES.to_string(),
        id.to_string(),
    )
    .unwrap();
    job::pack(&job::build_bundle(
        &plan,
        "p-slot-1",
        &Secret::new(String::new()),
    ))
    .unwrap()
}

/// The text of the shell function `name` in the boot script.
fn function_text(name: &str) -> String {
    let start = BOOT_SCRIPT.find(&format!("{name}() {{")).unwrap();
    let end = BOOT_SCRIPT[start..].find("\n}\n").unwrap() + 3;
    BOOT_SCRIPT[start..start + end].to_string()
}

#[test]
fn a_missing_tool_is_a_failure_when_unprivileged_and_an_install_otherwise() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let log = dir.path().join("apt.log");
    write_tool(
        &bin,
        "apt-get",
        &format!("echo \"$@\" >> {}", log.display()),
    );
    for (flag, expect_install) in [(Some("UNPRIVILEGED"), false), (None, true)] {
        let script = format!(
            "{}\nneed vivi-no-such-tool vivi-pkg\n",
            function_text("need")
        );
        let mut command = Command::new("bash");
        command
            .env_clear()
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .arg("-c")
            .arg(script);
        if let Some(flag) = flag {
            command.env(flag, "1");
        }
        let status = command.status().unwrap();
        assert!(!status.success(), "the tool never appears");
        let installed = std::fs::read_to_string(&log).unwrap_or_default();
        assert_eq!(
            installed.contains("vivi-pkg"),
            expect_install,
            "{flag:?}: {installed}"
        );
        std::fs::remove_file(&log).ok();
    }
}

#[test]
fn the_seed_checks_every_tool_that_unpacks_a_job_archive() {
    assert!(
        BOOT_SCRIPT.contains("TOOLS=\"git tar gzip\""),
        "tar and gzip must be in the required-tools list"
    );
    assert!(BOOT_SCRIPT.contains("seed_failed missing_tool $t"));
    assert!(
        BOOT_SCRIPT.contains("tar -xzf \"$JT\""),
        "the archive is gzip-compressed"
    );
}

#[test]
fn the_remote_command_line_is_one_quoted_word_for_the_login_shell() {
    let line = ssh_script::command_line("~/x", "exec", &["a/slot-1", "echo 'hi' \"$X\""], false);
    assert!(line.starts_with("bash -c '"));
    assert!(line.ends_with('\''));
    let login = ssh_script::command_line("~/x", "probe", &[], true);
    assert!(login.starts_with("bash -l -c '"));
}
