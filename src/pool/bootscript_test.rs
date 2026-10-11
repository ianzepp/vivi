use super::*;
use crate::pool::config::PoolConfig;

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
        "/work/job/go",
        "/work/job/job.sh",
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
