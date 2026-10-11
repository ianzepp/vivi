use super::*;

const FULL: &str = r#"
name = "main"
repo = "acme/widgets"
slots = 4
image = "rust:1.97-bookworm"
setup = ["apt-get install -y nodejs"]
seed = ["cargo build"]
build_dir = "target"

[fly]
app = "lane-pool"
region = "iad"
size = "performance-8x"
memory = 32768
tmpfs = "16g"

[github]
app_id = 42
private_key = "~/keys/app.pem"

[harness]
name = "pi"
run = "pi -p --model {model} @{brief}"
models = ["zai/glm-5.3-flash", "zai/glm-5.3"]
"#;

fn full() -> PoolConfig {
    toml::from_str(FULL).unwrap()
}

fn err(config: &PoolConfig) -> String {
    config.validate("space").unwrap_err().to_string()
}

#[test]
fn parses_a_full_table() {
    let config = full();
    config.validate("space").unwrap();
    assert_eq!(config.repo, "acme/widgets");
    assert_eq!(config.slots, 4);
    assert_eq!(config.github.app_id, 42);
    let harness = config.harness.as_ref().unwrap();
    assert_eq!(harness.name, "pi");
    assert_eq!(harness.models.len(), 2);
}

#[test]
fn defaults_fill_optional_fields() {
    let minimal: PoolConfig = toml::from_str(
        r#"
repo = "o/r"
slots = 1
image = "debian:12"
build_dir = "out"
[fly]
app = "a"
[github]
app_id = 1
private_key = "k.pem"
"#,
    )
    .unwrap();
    minimal.validate("space").unwrap();
    assert_eq!(minimal.provider, "fly");
    assert_eq!(minimal.fly.region, "iad");
    assert_eq!(minimal.fly.size, "performance-8x");
    assert_eq!(minimal.fly.memory, 32768);
    assert_eq!(minimal.fly.tmpfs, "16g");
    assert!(minimal.setup.is_empty() && minimal.harness.is_none());
}

#[test]
fn rejects_missing_required_field() {
    let result: Result<PoolConfig, _> = toml::from_str("app = \"a\"\nrepo = \"o/r\"");
    assert!(result.is_err());
}

#[test]
fn rejects_unknown_provider() {
    let mut config = full();
    config.provider = "aws".into();
    assert!(err(&config).contains("not supported"));
}

#[test]
fn rejects_bad_repo_and_zero_slots() {
    let mut config = full();
    config.repo = "widgets".into();
    assert!(err(&config).contains("owner/name"));
    let mut config = full();
    config.slots = 0;
    assert!(err(&config).contains("slots"));
}

#[test]
fn rejects_escaping_build_dir() {
    for bad in ["/target", "../target", "a/../b", "", "a b", "./x", "a//b"] {
        let mut config = full();
        config.build_dir = bad.into();
        assert!(err(&config).contains("build_dir"), "{bad}");
    }
}

#[test]
fn rejects_bad_tmpfs_and_empty_commands() {
    let mut config = full();
    config.fly.tmpfs = "16".into();
    assert!(err(&config).contains("tmpfs"));
    let mut config = full();
    config.seed = vec!["  ".into()];
    assert!(err(&config).contains("seed"));
}

#[test]
fn rejects_unknown_keys_at_every_level() {
    let top = FULL.replace("slots = 4", "slots = 4\nslotz = 5");
    let config: PoolConfig = toml::from_str(&top).unwrap();
    assert!(err(&config).contains("slotz"));
    let nested = FULL.replace("app_id = 42", "app_id = 42\ntoken = \"x\"");
    let config: PoolConfig = toml::from_str(&nested).unwrap();
    assert!(err(&config).contains("[pool.github]"));
}

#[test]
fn validates_harness() {
    let mut config = full();
    config.harness.as_mut().unwrap().run = String::new();
    assert!(err(&config).contains("harness.run"));
    let mut config = full();
    config
        .harness
        .as_mut()
        .unwrap()
        .models
        .push("zai/glm-5.3".into());
    assert!(err(&config).contains("duplicate"));
}

#[test]
fn pool_name_prefers_explicit_then_mailspace() {
    let mut config = full();
    assert_eq!(config.pool_name("ignored"), "main");
    config.name = None;
    assert_eq!(config.pool_name("Faber Lang!"), "faber-lang");
}

#[test]
fn private_key_path_expands_home() {
    let config = full();
    let path = config.private_key_path();
    assert!(path.ends_with("keys/app.pem"));
    assert!(!path.to_string_lossy().starts_with('~'));
}

#[test]
fn mailspace_config_roundtrips_pool() {
    let config = full();
    let text = toml::to_string(&config).unwrap();
    let again: PoolConfig = toml::from_str(&text).unwrap();
    again.validate("space").unwrap();
    assert_eq!(again.build_dir, config.build_dir);
}

#[test]
fn harness_env_rules_and_committer_parse() {
    let mut config = full();
    config.harness.as_mut().unwrap().env = vec!["ZAI_API_KEY".into(), "CARGO_HOME".into()];
    config.harness.as_mut().unwrap().rules = Some("docs/rules.md".into());
    config.committer = Some(PoolCommitter {
        name: "bot".into(),
        email: "bot@example.com".into(),
        unknown: BTreeMap::new(),
    });
    config.validate("space").unwrap();
    let rules = config
        .harness
        .as_ref()
        .unwrap()
        .rules_path(std::path::Path::new("/proj"));
    assert_eq!(rules, Some(PathBuf::from("/proj/docs/rules.md")));
}

#[test]
fn harness_toml_accepts_the_new_keys() {
    let text = format!("{FULL}\n[committer]\nname = \"bot\"\nemail = \"b@x.io\"\n").replace(
        "models = [",
        "env = [\"KEY_ONE\"]\nrules = \"r.md\"\nmodels = [",
    );
    let config: PoolConfig = toml::from_str(&text).unwrap();
    config.validate("space").unwrap();
    assert_eq!(config.harness.unwrap().env, ["KEY_ONE"]);
    assert_eq!(config.committer.unwrap().email, "b@x.io");
}

#[test]
fn harness_env_names_must_be_plain_and_never_the_git_token() {
    for bad in ["GH_TOKEN", "A B", "1X", "A=b", ""] {
        let mut config = full();
        config.harness.as_mut().unwrap().env = vec![bad.into()];
        assert!(err(&config).contains("harness.env"), "{bad}");
    }
}

#[test]
fn harness_run_rejects_unknown_placeholders_but_not_shell_expansion() {
    let mut config = full();
    config.harness.as_mut().unwrap().run = "tool --model {modle} {brief}".into();
    assert!(err(&config).contains("{modle}"));
    config.harness.as_mut().unwrap().run = "tool --model {model} -x ${home} {rules}".into();
    config.validate("space").unwrap();
}

#[test]
fn model_slugs_are_restricted_to_safe_characters() {
    let mut config = full();
    config.harness.as_mut().unwrap().models = vec!["a b".into()];
    assert!(err(&config).contains("harness.models"));
    config.harness.as_mut().unwrap().models = vec!["x;rm".into()];
    assert!(err(&config).contains("harness.models"));
}

#[test]
fn committer_must_be_single_line_text() {
    let mut config = full();
    config.committer = Some(PoolCommitter {
        name: "bot\nx".into(),
        email: "b@x.io".into(),
        unknown: BTreeMap::new(),
    });
    assert!(err(&config).contains("committer.name"));
}

#[test]
fn app_defaults_to_the_pool_name_with_a_suffix_of_the_organization() {
    let mut config = full();
    config.fly.app = None;
    config.name = None;
    config.validate("My Project").unwrap();
    assert_eq!(config.app_name("My Project"), "my-project-4a0a33");
    assert_eq!(config.organization_name(), "personal");
    config.name = Some("lanes".into());
    assert_eq!(config.app_name("My Project"), "lanes-4a0a33");
    config.fly.organization = Some("acme".into());
    assert_eq!(config.app_name("My Project"), "lanes-822b33");
    config.fly.app = Some("shared".into());
    assert_eq!(
        config.app_name("My Project"),
        "shared",
        "an explicit app wins"
    );
    assert_eq!(config.organization_name(), "acme");
}

#[test]
fn the_default_app_name_is_stable_per_organization_and_always_a_valid_name() {
    assert_eq!(
        default_app_name("lanes", "acme"),
        default_app_name("lanes", "acme")
    );
    assert_ne!(
        default_app_name("lanes", "acme"),
        default_app_name("lanes", "other")
    );
    assert_ne!(
        default_app_name("lanes", "acme"),
        default_app_name("tracks", "acme")
    );
    for pool in [
        "a",
        "lanes",
        "a-b-c",
        &"x".repeat(200),
        &format!("{}-tail", "y".repeat(56)),
    ] {
        let name = default_app_name(pool, "Some Org");
        assert!(is_slug(&name), "{name}");
        assert!(name.len() <= 63, "{name}");
        assert!(!name.contains("--"), "{name}");
        let suffix = name.rsplit('-').next().unwrap();
        assert_eq!(suffix.len(), 6);
        assert!(
            suffix
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
        );
    }
    assert!(default_app_name(&"x".repeat(200), "o").starts_with(&"x".repeat(56)));
}

#[test]
fn app_and_organization_must_be_slugs() {
    let mut config = full();
    config.fly.app = Some("Bad App".into());
    assert!(err(&config).contains("app"));
    let mut config = full();
    config.fly.organization = Some("Acme Inc".into());
    assert!(err(&config).contains("organization"));
}

#[test]
fn a_table_without_app_parses() {
    let config: PoolConfig = toml::from_str(&FULL.replace("app = \"lane-pool\"\n", "")).unwrap();
    config.validate("space").unwrap();
    assert!(config.fly.app.is_none());
}

#[test]
fn old_style_provider_keys_in_pool_name_the_new_location() {
    for (key, line) in [
        ("app", "app = \"x\""),
        ("organization", "organization = \"x\""),
        ("region", "region = \"iad\""),
        ("size", "size = \"shared-cpu-1x\""),
        ("memory", "memory = 1024"),
        ("tmpfs", "tmpfs = \"1g\""),
    ] {
        let old = FULL.replace("slots = 4", &format!("slots = 4\n{line}"));
        let config: PoolConfig = toml::from_str(&old).unwrap();
        let message = err(&config);
        assert!(message.contains(key), "{key}: {message}");
        assert!(message.contains("[pool.fly]"), "{key}: {message}");
    }
}

#[test]
fn unknown_keys_in_the_fly_table_are_rejected() {
    let text = FULL.replace("[fly]\n", "[fly]\nregon = \"iad\"\n");
    let config: PoolConfig = toml::from_str(&text).unwrap();
    let message = err(&config);
    assert!(
        message.contains("regon") && message.contains("[pool.fly]"),
        "{message}"
    );
}

#[test]
fn a_repo_name_means_github_and_an_embedded_source_replaces_it() {
    let mut config = full();
    assert_eq!(
        config.remote_address(),
        "https://github.com/acme/widgets.git"
    );
    config.embedded = Some(EmbeddedSource {
        remote: "file:///work/remote.git".into(),
        ..EmbeddedSource::default()
    });
    assert_eq!(config.remote_address(), "file:///work/remote.git");
    assert!(config.validate("space").is_ok());
}

#[test]
fn the_embedded_source_cannot_come_from_toml() {
    let toml = format!("{FULL}\n[embedded]\nremote = \"file:///x\"\n");
    let parsed: PoolConfig = toml::from_str(&toml).unwrap();
    assert!(parsed.embedded.is_none());
    assert!(err(&parsed).contains("unknown field \"embedded\""));
}

const DOCKER: &str = r#"
repo = "acme/widgets"
slots = 2
image = "ubuntu:24.04"
build_dir = "target"
provider = "docker"

[docker]
host = "ssh://ci@build-box"
memory = "8g"
cpus = 2.5
tmpfs = "4g"

[github]
app_id = 1
private_key = "k.pem"
"#;

fn docker() -> PoolConfig {
    toml::from_str(DOCKER).unwrap()
}

#[test]
fn a_docker_table_parses_and_validates() {
    let config = docker();
    config.validate("space").unwrap();
    assert_eq!(config.docker.host.as_deref(), Some("ssh://ci@build-box"));
    assert_eq!(config.docker.memory, "8g");
    assert_eq!(config.docker.cpus, Some(2.5));
    assert_eq!(config.tmpfs_size(), "4g");
}

#[test]
fn docker_settings_have_sensible_defaults_and_the_local_daemon_is_the_default() {
    let mut config = docker();
    config.docker = PoolDocker::default();
    config.validate("space").unwrap();
    assert_eq!(config.docker.host, None);
    assert_eq!(config.docker.memory, "4g");
    assert_eq!(config.docker.cpus, None);
    assert_eq!(config.tmpfs_size(), "2g");
    let bare: PoolConfig = toml::from_str(&DOCKER.replace(
        "[docker]\nhost = \"ssh://ci@build-box\"\nmemory = \"8g\"\ncpus = 2.5\ntmpfs = \"4g\"\n",
        "",
    ))
    .unwrap();
    bare.validate("space").unwrap();
    assert_eq!(bare.tmpfs_size(), "2g");
}

#[test]
fn an_integer_cpu_count_parses() {
    let config: PoolConfig = toml::from_str(&DOCKER.replace("cpus = 2.5", "cpus = 4")).unwrap();
    assert_eq!(config.docker.cpus, Some(4.0));
}

#[test]
fn bad_docker_settings_are_refused_by_name() {
    for (change, field) in [
        (("ssh://ci@build-box", "build-box"), "docker.host"),
        (("ssh://ci@build-box", "ssh://"), "docker.host"),
        (("memory = \"8g\"", "memory = \"eight\""), "docker.memory"),
        (("tmpfs = \"4g\"", "tmpfs = \"0g\""), "docker.tmpfs"),
        (("cpus = 2.5", "cpus = 0"), "docker.cpus"),
        (("cpus = 2.5", "cpus = -1.0"), "docker.cpus"),
    ] {
        let config: PoolConfig = toml::from_str(&DOCKER.replace(change.0, change.1)).unwrap();
        assert!(err(&config).contains(field), "{field}: {}", err(&config));
    }
    let config: PoolConfig =
        toml::from_str(&DOCKER.replace("cpus = 2.5", "cpus = 2.5\nswap = 1")).unwrap();
    assert!(err(&config).contains("unknown field \"swap\" in [pool.docker]"));
}

#[test]
fn the_fly_settings_are_not_checked_against_a_docker_pool_and_back() {
    let mut config = docker();
    config.fly.region = "Not A Slug".into();
    config.fly.memory = 0;
    config.validate("space").unwrap();
    let mut config = full();
    config.docker.memory = "eight".into();
    config.validate("space").unwrap();
}

#[test]
fn providers_other_than_fly_and_docker_are_refused_and_both_are_named() {
    let mut config = docker();
    config.provider = "podman".into();
    let said = err(&config);
    assert!(
        said.contains("\"fly\"") && said.contains("\"docker\""),
        "{said}"
    );
}
