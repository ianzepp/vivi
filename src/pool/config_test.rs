use super::*;

const FULL: &str = r#"
name = "main"
app = "lane-pool"
region = "iad"
repo = "acme/widgets"
slots = 4
size = "performance-8x"
memory = 32768
tmpfs = "16g"
image = "rust:1.97-bookworm"
setup = ["apt-get install -y nodejs"]
seed = ["cargo build"]
build_dir = "target"

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
app = "a"
repo = "o/r"
slots = 1
image = "debian:12"
build_dir = "out"
[github]
app_id = 1
private_key = "k.pem"
"#,
    )
    .unwrap();
    minimal.validate("space").unwrap();
    assert_eq!(minimal.provider, "fly");
    assert_eq!(minimal.region, "iad");
    assert_eq!(minimal.size, "performance-8x");
    assert_eq!(minimal.memory, 32768);
    assert_eq!(minimal.tmpfs, "16g");
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
    config.tmpfs = "16".into();
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
