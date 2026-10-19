//! The `local` provider's part of the pool table.

use super::*;

const FETCH: &str = r#"
provider = "local"
slots = 4

[local]
root = "~/lanes"
"#;

const WORKTREE: &str = r#"
provider = "local"
slots = 4
build_dir = "out"

[local]
root = "/srv/lanes"
worktree_of = "~/code/project"

[harness]
name = "h"
run = "h {model}"
models = ["m"]
"#;

fn id() -> PoolId {
    PoolId::new("space", "", "here")
}

fn parse(text: &str) -> PoolConfig {
    toml::from_str(text).unwrap()
}

fn err(config: &PoolConfig) -> String {
    config.validate(&id()).unwrap_err().to_string()
}

#[test]
fn local_is_a_provider_and_needs_neither_repo_nor_image() {
    assert!(PROVIDERS.contains(&"local"));
    let config = parse(FETCH);
    config.validate(&id()).unwrap();
    assert!(config.repo.is_none() && config.github.is_none());
    assert_eq!(config.image, "");
    assert_eq!(config.tmpfs_size(), "", "a directory has no memory disk");
    assert_eq!(config.local.root_or_default(), "~/lanes");
}

#[test]
fn the_root_has_a_default_and_is_a_directory_of_its_own() {
    let mut config = parse(FETCH);
    config.local.root = None;
    config.validate(&id()).unwrap();
    assert_eq!(config.local.root_or_default(), DEFAULT_LOCAL_ROOT);
    for root in ["", "/", "~/", "relative", "~/a/../b", "/a b", "/tmp/$HOME"] {
        config.local.root = Some(root.to_string());
        assert!(err(&config).contains("local.root must be"), "{root:?}");
    }
}

#[test]
fn worktree_mode_takes_its_source_from_a_local_repository() {
    let config = parse(WORKTREE);
    config.validate(&id()).unwrap();
    assert!(config.repo_access().is_none());
    assert_eq!(config.remote_address(), "", "a worktree pool has no remote");
    let path = config.worktree_source().unwrap();
    assert!(path.ends_with("code/project"), "{path:?}");
    assert!(!path.starts_with("~"), "the home directory is expanded");
    assert!(
        parse(FETCH).worktree_source().is_none(),
        "fetch mode has none"
    );
}

#[test]
fn worktree_of_and_repo_together_are_refused() {
    let mut config = parse(WORKTREE);
    config.repo = Some("acme/widgets".into());
    config.github = Some(PoolGithub {
        app_id: 1,
        private_key: "k.pem".into(),
        unknown: BTreeMap::new(),
    });
    let said = err(&config);
    assert!(
        said.contains("worktree_of and repo cannot both be set"),
        "{said}"
    );
}

#[test]
fn worktree_of_must_be_a_plain_absolute_or_home_path() {
    for path in ["", "relative", "~/", "/", "/a/../b", "/a//b", "/a\nb", "~"] {
        let mut config = parse(WORKTREE);
        config.local.worktree_of = Some(path.to_string());
        assert!(
            err(&config).contains("local.worktree_of must be"),
            "{path:?}"
        );
    }
    for path in ["/abs/path", "~/code/my project", "/a.b/c-d"] {
        let mut config = parse(WORKTREE);
        config.local.worktree_of = Some(path.to_string());
        config.validate(&id()).unwrap();
    }
}

#[test]
fn an_image_is_refused_for_local() {
    let mut config = parse(FETCH);
    config.image = "debian:12".into();
    let said = err(&config);
    assert!(
        said.contains("image does not apply to the local provider"),
        "{said}"
    );
}

#[test]
fn setup_commands_are_allowed_and_run_as_the_user() {
    let mut config = parse(FETCH);
    config.setup = vec!["true".into()];
    config.validate(&id()).unwrap();
    config.setup = vec![" ".into()];
    assert!(err(&config).contains("setup holds an empty command"));
}

#[test]
fn the_build_directory_is_optional_for_every_provider() {
    let mut config = parse(FETCH);
    assert_eq!(config.build_dir, None);
    config.validate(&id()).unwrap();
    config.build_dir = Some("../out".into());
    assert!(err(&config).contains("build_dir"));
    let docker: PoolConfig =
        toml::from_str("provider = \"docker\"\nslots = 1\nimage = \"ubuntu:24.04\"\n").unwrap();
    assert_eq!(docker.build_dir, None);
    docker.validate(&id()).unwrap();
    let fly: PoolConfig =
        toml::from_str("slots = 1\nimage = \"rust:1\"\n[fly]\napp = \"some-app\"\n").unwrap();
    assert_eq!(fly.build_dir, None);
    fly.validate(&id()).unwrap();
}

#[test]
fn a_pool_without_a_build_directory_serializes_without_one() {
    let text = toml::to_string(&parse(FETCH)).unwrap();
    assert!(!text.contains("build_dir"), "{text}");
}

#[test]
fn local_tables_are_checked_for_unknown_keys() {
    let config = parse(&FETCH.replace("root = ", "branch = \"x\"\nroot = "));
    assert!(err(&config).contains("unknown field \"branch\" in [pools.here.local]"));
}

#[test]
fn the_provider_error_names_all_four_providers() {
    let mut config = parse(FETCH);
    config.provider = "podman".into();
    let said = err(&config);
    for name in ["fly", "docker", "ssh", "local"] {
        assert!(said.contains(&format!("\"{name}\"")), "{said}");
    }
}
