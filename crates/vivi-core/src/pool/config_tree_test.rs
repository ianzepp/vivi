use super::*;
use crate::pool::slot::PoolId;

fn config(setting: &str) -> PoolConfig {
    toml::from_str(&format!("provider = 'local'\nslots = 1\n{setting}\n")).unwrap()
}

pub(super) fn target(url: &str) -> JobTarget {
    JobTarget {
        check: None,
        id: "alias-not-the-repo-name".into(),
        source: url.into(),
        publish_remote: None,
        cache_key: "a".repeat(64),
        local_source: None,
        seed: vec![],
        requires: vec![],
        build_dir: None,
    }
}

#[test]
fn tree_root_is_optional_absolute_and_roundtrips_as_a_pool_setting() {
    assert!(config("").tree_root.is_none());
    let good = config("tree_root = '/Volumes/Build Trees'");
    good.validate(&PoolId::new("space", "", "lanes")).unwrap();
    let roundtrip: PoolConfig = toml::from_str(&toml::to_string(&good).unwrap()).unwrap();
    assert_eq!(roundtrip.tree_root, good.tree_root);
    for root in [
        "", "/", "relative", "~/trees", "/a/../b", "/a/./b", "/a//b", "/a/", "/a\nb", "/a\0b",
    ] {
        let mut cfg = config("");
        cfg.tree_root = Some(root.into());
        let error = cfg
            .validate(&PoolId::new("space", "", "lanes"))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("[pools.lanes] tree_root"),
            "{root:?}: {error}"
        );
    }
    let cfg = config("tree_root = '/trees'\n[workspace]\nenabled = true");
    assert!(
        cfg.validate(&PoolId::new("space", "", "lanes"))
            .unwrap_err()
            .to_string()
            .contains("persistent directories")
    );
}

#[test]
fn tree_path_uses_url_owner_repo_and_the_exact_slot_name() {
    for url in [
        "git@hub.example:faberlang/radix.git",
        "ssh://git@hub.example:2222/faberlang/radix.git",
        "https://hub.example/faberlang/radix.git",
        "https://hub.example/faberlang/radix/",
        "file:///srv/git/faberlang/radix.git",
    ] {
        assert_eq!(
            target_tree(Some("/trees"), &target(url), "space-lanes-slot-2")
                .unwrap()
                .as_deref(),
            Some("/trees/faberlang/radix/space-lanes-slot-2"),
            "{url}"
        );
    }
    assert_eq!(
        target_tree(None, &target("no-url"), "slot-1").unwrap(),
        None
    );
    for url in [
        "no-url",
        "git@host:radix.git",
        "ssh://host/../radix.git",
        "https://host/owner/..",
        "git@host:owner/repo?query",
    ] {
        assert!(
            target_tree(Some("/trees"), &target(url), "slot-1").is_err(),
            "{url}"
        );
    }
    assert!(target_tree(Some("/trees"), &target("git@host:o/r"), "../slot").is_err());
}

#[test]
fn a_local_mirror_uses_its_repository_url_not_its_local_path() {
    let mut target = target("/mirrors/arbitrary-name");
    target.local_source = Some(target.source.clone());
    target.publish_remote = Some("git@hub.example:faberlang/radix.git".into());
    assert_eq!(
        target_tree(Some("/trees"), &target, "pool-slot-1")
            .unwrap()
            .as_deref(),
        Some("/trees/faberlang/radix/pool-slot-1")
    );
    target.publish_remote = None;
    assert!(target_tree(Some("/trees"), &target, "pool-slot-1").is_err());
}
