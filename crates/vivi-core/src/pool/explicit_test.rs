use super::*;
use crate::mailspace::MailspaceConfig;

fn config(extra: &str) -> MailspaceConfig {
    toml::from_str(&format!(
        "name = 'faberlang'\n[pools.studio]\nprovider = 'ssh'\n{extra}"
    ))
    .unwrap()
}
const SLOT: &str = "\n[[slot]]\nname = 'radix-1'\nrepository = 'faberlang/radix'\nhost = 'ianzepp@studio-01'\ndir = '/vivi/trees/faberlang/radix/slot-1'\npools = ['studio']\n";

#[test]
fn parses_validates_and_roundtrips_declared_slots() {
    let cfg = config(SLOT);
    validate(&cfg.slots, &cfg.pools).unwrap();
    let roundtrip: MailspaceConfig = toml::from_str(&toml::to_string(&cfg).unwrap()).unwrap();
    assert_eq!(roundtrip.slots[0].name, "radix-1");
    let mut pool = cfg.pools["studio"].clone();
    route(&mut pool, "studio", &cfg.slots).unwrap();
    pool.validate(&super::super::slot::PoolId::keyed("space", "studio"))
        .unwrap();
    assert_eq!(pool.slots, 1);
}

#[test]
fn rejects_duplicate_names_relative_dirs_and_malformed_repositories() {
    for replacement in [
        SLOT.to_string() + SLOT,
        SLOT.replace("/vivi/trees", "trees"),
        SLOT.replace("faberlang/radix'", "radix'"),
        SLOT.replace("faberlang/radix'", "../radix'"),
        SLOT.replace("['studio']", "['elsewhere']"),
    ] {
        let cfg = config(&replacement);
        assert!(validate(&cfg.slots, &cfg.pools).is_err(), "{replacement}");
    }
}

#[test]
fn repository_matching_accepts_git_url_forms_not_target_aliases() {
    for url in [
        "git@github.com:faberlang/radix.git",
        "https://github.com/faberlang/radix.git",
        "ssh://git@host:2222/faberlang/radix",
        "file:///srv/git/faberlang/radix.git",
    ] {
        assert_eq!(repository(url).as_deref(), Some("faberlang/radix"));
    }
    assert_eq!(repository("alias"), None);
}

#[test]
fn a_pool_without_routed_declarations_retains_its_count_and_provider() {
    let cfg = config("slots = 7\n");
    let mut pool = cfg.pools["studio"].clone();
    route(&mut pool, "studio", &[]).unwrap();
    assert_eq!(pool.slots, 7);
    assert!(pool.explicit_slots.is_empty());
    assert_eq!(pool.provider, "ssh");
}
