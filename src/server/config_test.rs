use super::*;

fn lookup<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
    move |name| {
        pairs
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| (*value).to_owned())
    }
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vivi-server-config-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn defaults_listen_on_loopback_and_hold_no_secrets() {
    let config = ServerConfig::from_lookup(&lookup(&[("DATA_DIR", "/srv/vivi")]), None).unwrap();
    assert_eq!(config.bind, IpAddr::from([127, 0, 0, 1]));
    assert_eq!(config.port, 8080);
    assert_eq!(config.data_dir, PathBuf::from("/srv/vivi"));
    assert!(config.store_encryption_key.is_none());
    assert!(config.moderation_token.is_none());
    assert!(config.host_operator_token.is_none());
}

#[test]
fn bind_names_the_interface_to_serve_on() {
    let env = [("DATA_DIR", "/srv/vivi"), ("BIND", "100.96.126.80")];
    let config = ServerConfig::from_lookup(&lookup(&env), None).unwrap();
    assert_eq!(config.bind, IpAddr::from([100, 96, 126, 80]));
    let bad = [("DATA_DIR", "/srv/vivi"), ("BIND", "tailnet")];
    let err = ServerConfig::from_lookup(&lookup(&bad), None).unwrap_err();
    assert_eq!(err, "invalid BIND: tailnet");
}

#[test]
fn data_dir_is_required() {
    for env in [&[][..], &[("DATA_DIR", "  ")][..]] {
        let err = ServerConfig::from_lookup(&lookup(env), None).unwrap_err();
        assert!(err.starts_with("DATA_DIR is required"), "{err}");
    }
}

#[test]
fn data_dir_must_be_absolute() {
    let err = ServerConfig::from_lookup(&lookup(&[("DATA_DIR", "./data")]), None).unwrap_err();
    assert_eq!(err, "DATA_DIR must be an absolute path: ./data");
}

#[test]
fn a_credential_file_wins_over_the_environment_and_is_trimmed() {
    let dir = scratch("wins");
    std::fs::write(dir.join("MODERATION_TOKEN"), "from-file\n").unwrap();
    let env = [
        ("DATA_DIR", "/srv/vivi"),
        ("MODERATION_TOKEN", "from-env"),
        ("HOST_OPERATOR_TOKEN", "operator-env"),
    ];
    let config = ServerConfig::from_lookup(&lookup(&env), Some(&dir)).unwrap();
    assert_eq!(config.moderation_token.as_deref(), Some("from-file"));
    assert_eq!(config.host_operator_token.as_deref(), Some("operator-env"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unreadable_credential_is_an_error_not_a_fallback() {
    let dir = scratch("unreadable");
    std::fs::create_dir(dir.join("STORE_ENCRYPTION_KEY")).unwrap();
    let env = [
        ("DATA_DIR", "/srv/vivi"),
        ("STORE_ENCRYPTION_KEY", "from-env"),
    ];
    let err = ServerConfig::from_lookup(&lookup(&env), Some(&dir)).unwrap_err();
    assert!(err.starts_with("cannot read credential"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_blank_operator_token_counts_as_unset() {
    let dir = scratch("blank");
    std::fs::write(dir.join("HOST_OPERATOR_TOKEN"), "\n").unwrap();
    let env = [("DATA_DIR", "/srv/vivi")];
    let config = ServerConfig::from_lookup(&lookup(&env), Some(&dir)).unwrap();
    assert!(config.host_operator_token.is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hosted_identity_keeps_its_old_rules() {
    let env = [
        ("DATA_DIR", "/srv/vivi"),
        ("HOSTED_DEFAULT_IDENTITY", " ops.1 "),
    ];
    let config = ServerConfig::from_lookup(&lookup(&env), None).unwrap();
    assert_eq!(config.hosted_default_identity.as_deref(), Some("ops.1"));
    let bad = [
        ("DATA_DIR", "/srv/vivi"),
        ("HOSTED_DEFAULT_IDENTITY", "a b"),
    ];
    assert!(ServerConfig::from_lookup(&lookup(&bad), None).is_err());
}
