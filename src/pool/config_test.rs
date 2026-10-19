use super::*;

const FULL: &str = r#"
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

/// The identity every test pool has: pool `main` of mailspace `space`.
fn id() -> PoolId {
    PoolId::new("space", "main")
}

fn err(config: &PoolConfig) -> String {
    config.validate(&id()).unwrap_err().to_string()
}

#[test]
fn parses_a_full_table() {
    let config = full();
    config.validate(&id()).unwrap();
    assert_eq!(config.repo.as_deref(), Some("acme/widgets"));
    assert_eq!(config.slots, 4);
    assert_eq!(config.github.as_ref().unwrap().app_id, 42);
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
    minimal.validate(&id()).unwrap();
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
    config.repo = Some("widgets".into());
    assert!(err(&config).contains("owner/name"));
    let mut config = full();
    config.slots = 0;
    assert!(err(&config).contains("slots"));
}

#[test]
fn rejects_escaping_build_dir() {
    for bad in ["/target", "../target", "a/../b", "", "a b", "./x", "a//b"] {
        let mut config = full();
        config.build_dir = Some(bad.into());
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
    assert!(err(&config).contains("[pools.main.github]"));
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
fn every_message_names_the_pool() {
    let mut config = full();
    config.provider = "aws".into();
    assert!(
        err(&config).starts_with("configuration error: [pools.main] ")
            || err(&config).contains("[pools.main] ")
    );
}

#[test]
fn pool_names_are_slugs_that_start_and_end_with_a_letter_or_digit() {
    let config = full();
    for good in ["local", "lanes-2", "a", "9"] {
        config.validate(&PoolId::new("space", good)).unwrap();
    }
    for bad in ["", "Local", "has space", "-x", "x-", "a_b", "a.b", "a/b"] {
        let said = config
            .validate(&PoolId::new("space", bad))
            .unwrap_err()
            .to_string();
        assert!(said.contains("pool name"), "{bad:?}: {said}");
    }
    let said = config
        .validate(&PoolId::new("space", "local-doctor"))
        .unwrap_err()
        .to_string();
    assert!(said.contains("-doctor"), "{said}");
}

#[test]
fn a_mailspace_name_with_no_letter_or_digit_is_refused() {
    let said = full()
        .validate(&PoolId::new("!!!", "main"))
        .unwrap_err()
        .to_string();
    assert!(said.contains("mailspace name"), "{said}");
}

#[test]
fn private_key_path_expands_home() {
    let config = full();
    let path = config.github.unwrap().private_key_path();
    assert!(path.ends_with("keys/app.pem"));
    assert!(!path.to_string_lossy().starts_with('~'));
}

#[test]
fn mailspace_config_roundtrips_pool() {
    let config = full();
    let text = toml::to_string(&config).unwrap();
    let again: PoolConfig = toml::from_str(&text).unwrap();
    again.validate(&id()).unwrap();
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
    config.validate(&id()).unwrap();
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
    config.validate(&id()).unwrap();
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
    config.validate(&id()).unwrap();
}

#[test]
fn harness_run_accepts_the_reasoning_placeholder() {
    let mut config = full();
    config.harness.as_mut().unwrap().run =
        "tool --model {model} --thinking {reasoning} {brief} {rules}".into();
    config.validate(&id()).unwrap();
}

#[test]
fn the_mcp_config_placeholder_is_accepted_and_needs_the_bridge() {
    let mut config = full();
    config.harness.as_mut().unwrap().run = "h --mcp-config {mcp_config}".into();
    let message = err(&config);
    assert!(message.contains("{mcp_config}"), "{message}");
    assert!(message.contains("[pools.main.bridge]"), "{message}");
    config.bridge.enabled = true;
    config.validate(&id()).unwrap();
    config.harness.as_mut().unwrap().run = "tool --model {model}".into();
    config.validate(&id()).unwrap();
    config.bridge.enabled = false;
    config.validate(&id()).unwrap();
}

/// The bridge table is one boolean: `true` parses, the absent table is the
/// same as off, and anything else is refused when the mailspace config
/// loads.
#[test]
fn the_bridge_enabled_field_is_a_bool_and_the_absent_table_is_off() {
    use crate::mailspace::MailspaceConfig;

    let refused: Result<MailspaceConfig, _> = toml::from_str(
        r#"
name = "space"

[pools.x]
provider = "local"

[pools.x.bridge]
enabled = "yes"
"#,
    );
    let message = match refused {
        Ok(_) => panic!("the non-bool enabled parsed"),
        Err(e) => e.to_string(),
    };
    assert!(message.contains("enabled"), "{message}");

    let on: MailspaceConfig = toml::from_str(
        r#"
name = "space"

[pools.x]
provider = "local"
slots = 1

[pools.x.bridge]
enabled = true
"#,
    )
    .unwrap();
    assert!(on.pools["x"].bridge.enabled);

    let off: MailspaceConfig = toml::from_str(
        r#"
name = "space"

[pools.x]
provider = "local"
slots = 1
"#,
    )
    .unwrap();
    assert!(!off.pools["x"].bridge.enabled);
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
fn a_fly_pool_requires_an_app_with_no_default() {
    let mut config = full();
    config.fly.app = None;
    let said = err(&config);
    assert!(
        said.contains("fly.app is required") && said.contains("[pools.main]"),
        "{said}"
    );
    let text = FULL.replace("app = \"lane-pool\"\n", "");
    let parsed: PoolConfig = toml::from_str(&text).unwrap();
    assert!(err(&parsed).contains("fly.app is required"));
    // Only a fly pool needs one.
    let mut config = docker();
    config.fly.app = None;
    config.validate(&id()).unwrap();
}

#[test]
fn the_organization_is_the_one_a_new_app_is_made_in() {
    let mut config = full();
    assert_eq!(config.organization_name(), "personal");
    config.fly.organization = Some("acme".into());
    assert_eq!(config.organization_name(), "acme");
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
fn old_style_provider_keys_at_the_pool_level_are_unknown_fields() {
    for key in ["app", "organization", "region", "size", "memory", "tmpfs"] {
        let old = FULL.replace("slots = 4", &format!("slots = 4\n{key} = \"x\""));
        let config: PoolConfig = toml::from_str(&old).unwrap();
        let message = err(&config);
        assert!(message.contains(key), "{key}: {message}");
        assert!(message.contains("unknown field"), "{key}: {message}");
    }
}

#[test]
fn the_name_setting_is_gone() {
    let config: PoolConfig =
        toml::from_str(&FULL.replace("slots = 4", "slots = 4\nname = \"x\"")).unwrap();
    assert!(err(&config).contains("unknown field \"name\""));
}

#[test]
fn unknown_keys_in_the_fly_table_are_rejected() {
    let text = FULL.replace("[fly]\n", "[fly]\nregon = \"iad\"\n");
    let config: PoolConfig = toml::from_str(&text).unwrap();
    let message = err(&config);
    assert!(
        message.contains("regon") && message.contains("[pools.main.fly]"),
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
    assert!(config.validate(&id()).is_ok());
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
    config.validate(&id()).unwrap();
    assert_eq!(config.docker.host.as_deref(), Some("ssh://ci@build-box"));
    assert_eq!(config.docker.memory, "8g");
    assert_eq!(config.docker.cpus, Some(2.5));
    assert_eq!(config.tmpfs_size(), "4g");
}

#[test]
fn docker_settings_have_sensible_defaults_and_the_local_daemon_is_the_default() {
    let mut config = docker();
    config.docker = PoolDocker::default();
    config.validate(&id()).unwrap();
    assert_eq!(config.docker.host, None);
    assert_eq!(config.docker.memory, "4g");
    assert_eq!(config.docker.cpus, None);
    assert_eq!(config.tmpfs_size(), "2g");
    let bare: PoolConfig = toml::from_str(&DOCKER.replace(
        "[docker]\nhost = \"ssh://ci@build-box\"\nmemory = \"8g\"\ncpus = 2.5\ntmpfs = \"4g\"\n",
        "",
    ))
    .unwrap();
    bare.validate(&id()).unwrap();
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
    assert!(err(&config).contains("unknown field \"swap\" in [pools.main.docker]"));
}

#[test]
fn the_fly_settings_are_not_checked_against_a_docker_pool_and_back() {
    let mut config = docker();
    config.fly.region = "Not A Slug".into();
    config.fly.memory = 0;
    config.validate(&id()).unwrap();
    let mut config = full();
    config.docker.memory = "eight".into();
    config.validate(&id()).unwrap();
}

#[test]
fn providers_other_than_the_three_are_refused_and_all_are_named() {
    let mut config = docker();
    config.provider = "podman".into();
    let said = err(&config);
    assert!(
        said.contains("\"fly\"") && said.contains("\"docker\"") && said.contains("\"ssh\""),
        "{said}"
    );
}

const SSH: &str = r#"
repo = "acme/widgets"
slots = 2
build_dir = "target"
provider = "ssh"

[ssh]
host = "ci@build-box"
root = "~/lanes"

[github]
app_id = 1
private_key = "k.pem"
"#;

fn ssh() -> PoolConfig {
    toml::from_str(SSH).unwrap()
}

#[test]
fn an_ssh_table_parses_and_validates_with_no_image() {
    let config = ssh();
    config.validate(&id()).unwrap();
    assert_eq!(config.ssh.host.as_deref(), Some("ci@build-box"));
    assert_eq!(config.ssh.root_or_default(), "~/lanes");
    assert_eq!(config.image, "");
    assert_eq!(config.tmpfs_size(), "", "a directory has no memory disk");
}

#[test]
fn the_ssh_root_defaults_under_the_remote_home() {
    let mut config = ssh();
    config.ssh.root = None;
    config.validate(&id()).unwrap();
    assert_eq!(config.ssh.root_or_default(), "~/vivi-pool");
}

#[test]
fn an_image_is_refused_for_ssh_and_still_required_elsewhere() {
    let mut config = ssh();
    config.image = "debian:12".into();
    let said = err(&config);
    assert!(
        said.contains("image does not apply to the ssh provider"),
        "{said}"
    );
    let mut config = docker();
    config.image = String::new();
    assert!(err(&config).contains("image must be a single word"));
    let mut config = full();
    config.image = String::new();
    assert!(err(&config).contains("image must be a single word"));
}

#[test]
fn the_ssh_host_is_required_and_cannot_be_an_option() {
    let mut config = ssh();
    config.ssh.host = None;
    assert!(err(&config).contains("ssh.host is required"));
    for host in [
        "",
        "-oProxyCommand=x",
        "a b",
        "host;rm",
        "a\nb",
        "$(x)",
        "user@",
    ] {
        let mut config = ssh();
        config.ssh.host = Some(host.to_string());
        if host == "user@" {
            // Syntactically plain; `ssh` itself refuses it at connect time.
            config.validate(&id()).unwrap();
            continue;
        }
        assert!(err(&config).contains("ssh.host must be"), "{host:?}");
    }
    for host in ["pharos", "me@server.example.com", "me@[::1]", "10.0.0.7"] {
        let mut config = ssh();
        config.ssh.host = Some(host.to_string());
        config.validate(&id()).unwrap();
    }
}

#[test]
fn the_ssh_root_must_be_a_directory_of_its_own() {
    for root in [
        "",
        "/",
        "~",
        "~/",
        "//",
        "relative/dir",
        "~/a/../b",
        "~/./a",
        "/a//b",
        "/a/",
        "~/a b",
        "~/a;b",
        "/tmp/$HOME",
    ] {
        let mut config = ssh();
        config.ssh.root = Some(root.to_string());
        assert!(err(&config).contains("ssh.root must be"), "{root:?}");
    }
    for root in ["~/vivi-pool", "/srv/lanes", "/home/me/a.b/c_d-e"] {
        let mut config = ssh();
        config.ssh.root = Some(root.to_string());
        config.validate(&id()).unwrap();
    }
}

#[test]
fn ssh_root_stays_absolute() {
    let mut config = ssh();
    config.ssh.root = Some("rel/x".into());
    assert!(
        err(&config).contains("ssh.root must be"),
        "{}",
        err(&config)
    );
}

#[test]
fn ssh_tables_are_checked_for_unknown_keys_and_only_for_ssh() {
    let config: PoolConfig = toml::from_str(&SSH.replace("root = ", "port = 22\nroot = ")).unwrap();
    assert!(err(&config).contains("unknown field \"port\" in [pools.main.ssh]"));
    let mut config = docker();
    config.ssh.host = Some("-bad".into());
    config.validate(&id()).unwrap();
}

const PROVIDER_ONLY: &str = r#"
provider = "docker"
slots = 2
image = "ubuntu:24.04"

[docker]
memory = "1g"
"#;

#[test]
fn a_pool_with_only_its_provider_settings_validates() {
    let config: PoolConfig = toml::from_str(PROVIDER_ONLY).unwrap();
    config.validate(&id()).unwrap();
    assert!(config.repo.is_none() && config.github.is_none());
    assert!(config.repo_access().is_none());
    assert_eq!(config.build_dir, None, "no default build directory");
    assert_eq!(config.remote_address(), "");
    let ssh: PoolConfig =
        toml::from_str("provider = \"ssh\"\nslots = 1\n[ssh]\nhost = \"box\"\n").unwrap();
    ssh.validate(&id()).unwrap();
}

#[test]
fn the_commands_that_need_a_repository_name_the_missing_setting() {
    let config: PoolConfig = toml::from_str(PROVIDER_ONLY).unwrap();
    for what in ["create slots", "dispatch"] {
        let said = config.require_repo("main", what).unwrap_err().to_string();
        assert!(
            said.contains("[pools.main] repo is required to") && said.contains(what),
            "{said}"
        );
    }
    let repo_only: PoolConfig = toml::from_str(
        &format!("{PROVIDER_ONLY}\n").replace("slots = 2", "slots = 2\nrepo = \"o/r\""),
    )
    .unwrap();
    let said = repo_only
        .require_repo("main", "dispatch")
        .unwrap_err()
        .to_string();
    assert!(said.contains("github is required"), "{said}");
    assert!(full().require_repo("main", "dispatch").is_ok());
}

#[test]
fn a_legacy_repo_requires_github_but_shared_credentials_allow_job_targets() {
    let mut config = full();
    config.github = None;
    assert!(err(&config).contains("github is required when repo is set"));
    let mut config = full();
    config.repo = None;
    config.validate(&id()).unwrap();
}

#[test]
fn requires_and_facts_parse_as_command_lists_and_default_to_empty() {
    let config = full();
    assert!(config.requires.is_empty() && config.facts.is_empty());
    let mut config = full();
    config.requires = vec!["command -v tool".into(), "tool --list".into()];
    config.facts = vec!["tool --query --csv".into(), "uname -srm".into()];
    config.validate(&id()).unwrap();
    let parsed: PoolConfig = toml::from_str(&format!(
        "{FULL}\n"
    ).replacen("seed = [\"cargo build\"]", "seed = [\"cargo build\"]\nrequires = [\"command -v tool\"]\nfacts = [\"uname -srm\", \"tool --version\"]", 1))
    .unwrap();
    assert_eq!(parsed.requires, ["command -v tool"]);
    assert_eq!(parsed.facts, ["uname -srm", "tool --version"]);
    parsed.validate(&id()).unwrap();
}

#[test]
fn requires_and_facts_reject_empty_and_multi_line_entries_naming_the_field() {
    for field in ["requires", "facts"] {
        for bad in ["", "   ", "a\nb", "a\r", "a\u{1b}[0m"] {
            let mut config = full();
            let list = vec![bad.to_string()];
            match field {
                "requires" => config.requires = list,
                _ => config.facts = list,
            }
            let said = err(&config);
            assert!(said.contains(field), "{field} {bad:?}: {said}");
        }
        let mut config = full();
        let list = vec!["printf 'a\tb'".to_string()];
        match field {
            "requires" => config.requires = list,
            _ => config.facts = list,
        }
        config.validate(&id()).unwrap();
    }
}

const BASELINE_POOL: &str = r#"
provider = "local"
slots = 1
build_dir = "target"
build_baseline_gib = 24
"#;

#[test]
fn build_baseline_gib_parses_and_validates() {
    let config: PoolConfig = toml::from_str(BASELINE_POOL).unwrap();
    assert_eq!(config.build_baseline_gib, Some(24));
    assert_eq!(config.build_baseline_bytes(), Some(24 * 1024_u64.pow(3)));
    config.validate(&id()).unwrap();
    for bad in ["0", "2000"] {
        let text = BASELINE_POOL.replace(
            "build_baseline_gib = 24",
            &format!("build_baseline_gib = {bad}"),
        );
        let config: PoolConfig = toml::from_str(&text).unwrap();
        let said = err(&config);
        assert!(said.contains("build_baseline_gib"), "{bad}: {said}");
        assert!(said.contains("1..=1024"), "{bad}: {said}");
    }
}
