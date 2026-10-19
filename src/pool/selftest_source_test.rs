use super::*;

fn real() -> PoolConfig {
    toml::from_str(
        r#"
repo = "acme/widgets"
slots = 8
image = "debian:12"
build_dir = "target"
setup = ["apt-get install -y make"]
seed = ["make warm"]
[fly]
app = "lane"
region = "ord"
size = "performance-8x"
memory = 32768
tmpfs = "16g"
[github]
app_id = 1
private_key = "k.pem"
[harness]
name = "pi"
run = "pi {model}"
models = ["a/b"]
env = ["SECRET_KEY"]
[committer]
name = "Real"
email = "real@example.com"
"#,
    )
    .unwrap()
}

#[test]
fn the_test_pool_is_the_doctor_pool_of_the_same_mailspace() {
    let real = crate::pool::slot::PoolId::new("Space", "", "main");
    let test = real.doctor();
    assert_eq!(
        (test.mailspace.as_str(), test.pool.as_str()),
        ("space", "main-doctor")
    );
}

#[test]
fn the_test_config_replaces_every_project_setting_but_keeps_the_host() {
    let config = test_config(&real());
    assert_eq!(config.slots, 2);
    assert!(config.setup.is_empty());
    assert_eq!(config.seed, ["true"]);
    assert_eq!(config.build_dir.as_deref(), Some("build"));
    assert_eq!(config.fly.app.as_deref(), Some("lane"), "same host app");
    assert_eq!(config.fly.region, "ord");
    assert_eq!(
        (config.fly.size.as_str(), config.fly.memory),
        ("shared-cpu-1x", 256)
    );
    assert_eq!(
        (config.docker.memory.as_str(), config.docker.tmpfs.as_str()),
        ("512m", "64m"),
        "a Docker test pool is small too"
    );
    assert_eq!(config.image, "debian:12");
    let harness = config.harness.as_ref().unwrap();
    assert_eq!(harness.models, [MODEL]);
    assert!(harness.env.is_empty(), "no model key reaches the stand-in");
    assert_eq!(
        config.committer.as_ref().unwrap().email,
        "pool-test@localhost"
    );
    assert_eq!(config.remote_address(), REMOTE);
    assert!(
        config
            .validate(&crate::pool::slot::PoolId::new("ms", "", "main"))
            .is_ok()
    );
}

#[test]
fn the_stand_in_harness_appends_a_line_and_prints_a_receipt() {
    let quick = harness_command(0);
    assert!(quick.starts_with("echo "), "{quick}");
    assert!(quick.contains(">> standin.txt"));
    assert!(quick.contains("{model}"));
    let slow = harness_command(120);
    assert!(slow.starts_with("sleep 120; echo "), "{slow}");
    let rendered = crate::pool::job::render_harness(&slow, MODEL, None);
    assert!(!rendered.contains('{'), "{rendered}");
}

#[test]
fn the_stand_in_harness_runs_under_a_real_shell() {
    let dir = tempfile::tempdir().unwrap();
    let command = crate::pool::job::render_harness(&harness_command(0), MODEL, None);
    let out = std::process::Command::new("bash")
        .arg("-c")
        .arg(&command)
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("stand-in receipt"));
    let written = std::fs::read_to_string(dir.path().join(HARNESS_FILE)).unwrap();
    assert!(written.starts_with("stand-in standin "), "{written}");
}

#[test]
fn the_embedded_source_is_small_and_ignores_the_build_mount() {
    let names: Vec<&str> = SOURCE_FILES.iter().map(|(n, _)| *n).collect();
    assert_eq!(names, ["README.md", "notes.txt", ".gitignore"]);
    let ignore = SOURCE_FILES
        .iter()
        .find(|(n, _)| *n == ".gitignore")
        .unwrap();
    assert!(ignore.1.lines().any(|l| l == "build/"));
}

#[test]
fn the_self_test_runs_no_requirements_and_no_facts_of_the_real_pool() {
    let mut real = real();
    real.requires = vec!["command -v some-tool".into()];
    real.facts = vec!["some-tool --version".into()];
    let test = test_config(&real);
    assert!(test.requires.is_empty() && test.facts.is_empty());
    assert_eq!(real.requires.len(), 1, "the real pool is not touched");
}
