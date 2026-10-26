//! The server key store: principals, grants, digests, reload, and the
//! legacy per-project key retirement.

use std::path::PathBuf;

use vivi::server::auth::sha256_hex;
use vivi::server::grants::{
    GrantLevel, GrantRecord, GrantsError, ServerKeyStore, retire_legacy_project_keys,
};

fn scratch(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("vivi-server-grants-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn member_grant(project: &str, level: GrantLevel) -> GrantRecord {
    GrantRecord {
        project_id: project.to_owned(),
        member: "operator".to_owned(),
        identity: Some("operator".to_owned()),
        level,
    }
}

fn legacy_keys_file(count: usize) -> String {
    let keys: Vec<String> = (0..count)
        .map(|i| {
            format!(
                r#"{{"key_id":"key_{i}","label":"k{i}","digest":"{i:064}","created_by":"operator","created_at":"2026-01-01T00:00:00Z"}}"#
            )
        })
        .collect();
    format!("{{\"keys\":[{}]}}", keys.join(","))
}

#[test]
fn mint_then_verify_round_trips_principal_and_grants() {
    let dir = scratch("round-trip");
    let store = ServerKeyStore::new(&dir).unwrap();
    let minted = store
        .mint(
            "operator@p1",
            false,
            vec![member_grant("p1", GrantLevel::Operator)],
            "ops",
            "operator",
        )
        .unwrap();
    assert!(minted.key.starts_with("vivi_"), "raw key is vivi_-prefixed");
    let verified = store.verify(&minted.key).unwrap();
    assert_eq!(verified.principal, "operator@p1");
    assert_eq!(verified.key_id, minted.key_id);
    assert!(!verified.host);
    assert_eq!(
        verified.grants,
        vec![member_grant("p1", GrantLevel::Operator)]
    );
}

#[test]
fn the_raw_secret_is_never_persisted() {
    let dir = scratch("digest-only");
    let store = ServerKeyStore::new(&dir).unwrap();
    let minted = store
        .mint("operator@p1", false, vec![], "ops", "operator")
        .unwrap();
    let stored = std::fs::read_to_string(dir.join("keys.json")).unwrap();
    assert!(
        !stored.contains(&minted.key),
        "keys.json leaks the raw secret"
    );
    assert!(
        stored.contains(&sha256_hex(minted.key.as_bytes())),
        "keys.json stores the digest"
    );
}

#[test]
fn verify_of_an_unknown_key_is_none() {
    let dir = scratch("unknown");
    let store = ServerKeyStore::new(&dir).unwrap();
    store
        .mint(
            "operator@p1",
            false,
            vec![member_grant("p1", GrantLevel::Member)],
            "ops",
            "operator",
        )
        .unwrap();
    assert!(store.verify("vivi_wrong").is_none());
    assert!(store.verify("").is_none());
}

#[test]
fn verify_of_a_revoked_key_is_none() {
    let dir = scratch("revoked-key");
    let store = ServerKeyStore::new(&dir).unwrap();
    let minted = store
        .mint(
            "operator@p1",
            false,
            vec![member_grant("p1", GrantLevel::Member)],
            "ops",
            "operator",
        )
        .unwrap();
    store.revoke_key(&minted.key_id).unwrap();
    assert!(store.verify(&minted.key).is_none());
    store.revoke_key(&minted.key_id).unwrap(); // already-revoked is success
    assert!(matches!(
        store.revoke_key("key_missing"),
        Err(GrantsError::NotFound)
    ));
}

#[test]
fn grants_survive_reload_from_disk() {
    let dir = scratch("reload");
    let store = ServerKeyStore::new(&dir).unwrap();
    let minted = store
        .mint(
            "operator@p1",
            false,
            vec![member_grant("p1", GrantLevel::Operator)],
            "ops",
            "operator",
        )
        .unwrap();
    drop(store);
    let reopened = ServerKeyStore::new(&dir).unwrap();
    let verified = reopened.verify(&minted.key).unwrap();
    assert_eq!(verified.principal, "operator@p1");
    assert_eq!(
        verified.grants,
        vec![member_grant("p1", GrantLevel::Operator)]
    );
    let listed = reopened.list();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].principal, "operator@p1");
    assert_eq!(
        listed[0].grants,
        vec![member_grant("p1", GrantLevel::Operator)]
    );
    assert_eq!(listed[0].keys[0].key_id, minted.key_id);
}

#[test]
fn a_remint_reuses_the_principal_and_adds_no_duplicate_grant() {
    let dir = scratch("remint");
    let store = ServerKeyStore::new(&dir).unwrap();
    let grant = member_grant("p1", GrantLevel::Member);
    store
        .mint("operator@p1", false, vec![grant.clone()], "one", "operator")
        .unwrap();
    store
        .mint("operator@p1", false, vec![grant.clone()], "two", "operator")
        .unwrap();
    let listed = store.list();
    assert_eq!(listed.len(), 1, "one principal, two keys");
    assert_eq!(listed[0].keys.len(), 2);
    assert_eq!(listed[0].grants, vec![grant]);
}

#[test]
fn revoke_principal_kills_every_key_and_reports_the_count() {
    let dir = scratch("revoke-principal");
    let store = ServerKeyStore::new(&dir).unwrap();
    let one = store
        .mint("a@p1", false, vec![], "one", "operator")
        .unwrap();
    let two = store
        .mint("a@p1", false, vec![], "two", "operator")
        .unwrap();
    store
        .mint("b@p1", false, vec![], "other", "operator")
        .unwrap();
    assert_eq!(store.revoke_principal("a@p1").unwrap(), 2);
    assert!(store.verify(&one.key).is_none());
    assert!(store.verify(&two.key).is_none());
    assert_eq!(store.revoke_principal("a@p1").unwrap(), 0); // idempotent
    assert!(matches!(
        store.revoke_principal("nope@p1"),
        Err(GrantsError::NotFound)
    ));
}

#[test]
fn a_host_principal_has_no_project_grants() {
    let dir = scratch("host");
    let store = ServerKeyStore::new(&dir).unwrap();
    let minted = store
        .mint("host", true, vec![], "host key", "operator")
        .unwrap();
    let verified = store.verify(&minted.key).unwrap();
    assert!(verified.host);
    assert!(verified.grants.is_empty());
    assert!(matches!(
        store.mint(
            "host",
            true,
            vec![member_grant("p1", GrantLevel::Member)],
            "bad",
            "operator"
        ),
        Err(GrantsError::HostWithGrants)
    ));
    assert!(matches!(
        store.mint("host", false, vec![], "bad", "operator"),
        Err(GrantsError::HostMismatch)
    ));
}

#[test]
fn a_principal_caps_at_32_live_keys() {
    let dir = scratch("limit");
    let store = ServerKeyStore::new(&dir).unwrap();
    for i in 0..32 {
        store
            .mint("operator@p1", false, vec![], &format!("k{i}"), "operator")
            .unwrap();
    }
    assert!(matches!(
        store.mint("operator@p1", false, vec![], "one more", "operator"),
        Err(GrantsError::LimitReached)
    ));
    let listed = store.list();
    assert_eq!(listed[0].keys.len(), 32);
}

#[test]
fn bad_input_is_refused_and_nothing_is_stored() {
    let dir = scratch("invalid");
    let store = ServerKeyStore::new(&dir).unwrap();
    for bad in ["", "two words", "a/b", &"x".repeat(65)] {
        assert!(matches!(
            store.mint(bad, false, vec![], "ops", "operator"),
            Err(GrantsError::InvalidPrincipal)
        ));
    }
    for bad in ["", "   ", &"x".repeat(65), "bell\u{7}"] {
        assert!(matches!(
            store.mint("operator@p1", false, vec![], bad, "operator"),
            Err(GrantsError::InvalidLabel)
        ));
    }
    let bad_grants = [
        GrantRecord {
            project_id: String::new(),
            member: "operator".into(),
            identity: None,
            level: GrantLevel::Member,
        },
        GrantRecord {
            project_id: "p1".into(),
            member: "two words".into(),
            identity: None,
            level: GrantLevel::Member,
        },
        GrantRecord {
            project_id: "p1".into(),
            member: "operator".into(),
            identity: Some("a@b".into()),
            level: GrantLevel::Member,
        },
    ];
    for grant in bad_grants {
        assert!(matches!(
            store.mint("operator@p1", false, vec![grant], "ops", "operator"),
            Err(GrantsError::InvalidGrant)
        ));
    }
    assert!(store.list().is_empty());
    assert!(
        !dir.join("keys.json").exists(),
        "no file written on refusal"
    );
}

#[test]
fn unknown_fields_in_keys_json_are_refused_at_construction() {
    let dir = scratch("unknown-fields");
    std::fs::write(
        dir.join("keys.json"),
        r#"{"principals":[{"principal":"host","nope":true}]}"#,
    )
    .unwrap();
    assert!(matches!(
        ServerKeyStore::new(&dir),
        Err(GrantsError::Storage(_))
    ));
}

#[cfg(unix)]
#[test]
fn keys_json_is_0600_after_create() {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch("mode");
    let store = ServerKeyStore::new(&dir).unwrap();
    store
        .mint("operator@p1", false, vec![], "ops", "operator")
        .unwrap();
    let mode = std::fs::metadata(dir.join("keys.json"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[test]
fn the_atomic_save_leaves_no_tmp_file() {
    let dir = scratch("atomic");
    let store = ServerKeyStore::new(&dir).unwrap();
    store
        .mint("operator@p1", false, vec![], "one", "operator")
        .unwrap();
    store
        .mint("operator@p1", false, vec![], "two", "operator")
        .unwrap();
    store.revoke_principal("operator@p1").unwrap();
    let names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, vec!["keys.json".to_owned()], "no tmp leftovers");
}

#[test]
fn retire_legacy_project_keys_renames_once_and_counts() {
    let dir = scratch("retire");
    for (project, count) in [("p1", 2), ("p2", 1)] {
        let project = dir.join("projects").join(project);
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("api_keys.json"), legacy_keys_file(count)).unwrap();
    }
    // A project with no live key file is not reported.
    std::fs::create_dir_all(dir.join("projects").join("empty")).unwrap();

    let retired = retire_legacy_project_keys(&dir);
    assert_eq!(retired, vec![("p1".to_owned(), 2), ("p2".to_owned(), 1)]);
    assert!(!dir.join("projects/p1/api_keys.json").exists());
    assert!(dir.join("projects/p1/api_keys.json.retired").exists());
    assert_eq!(
        std::fs::read_to_string(dir.join("projects/p1/api_keys.json.retired")).unwrap(),
        legacy_keys_file(2),
        "the rename moves the exact bytes"
    );

    // Idempotent: the second run finds only .retired names.
    assert!(retire_legacy_project_keys(&dir).is_empty());
    assert!(dir.join("projects/p1/api_keys.json.retired").exists());
}

#[test]
fn retire_skips_projects_already_retired_and_counts_unreadable_as_zero() {
    let dir = scratch("retire-skip");
    // p1 was retired by an earlier boot but a live file reappeared:
    // the existing .retired name wins; the live file is left alone.
    let p1 = dir.join("projects").join("p1");
    std::fs::create_dir_all(&p1).unwrap();
    std::fs::write(p1.join("api_keys.json"), legacy_keys_file(1)).unwrap();
    std::fs::write(p1.join("api_keys.json.retired"), legacy_keys_file(9)).unwrap();
    // p2's file is not JSON: it still retires, with a count of 0.
    let p2 = dir.join("projects").join("p2");
    std::fs::create_dir_all(&p2).unwrap();
    std::fs::write(p2.join("api_keys.json"), "not json").unwrap();

    let retired = retire_legacy_project_keys(&dir);
    assert_eq!(
        retired,
        vec![("p2".to_owned(), 0)],
        "p1 is skipped, p2 retires with a zero count"
    );
    assert!(
        p1.join("api_keys.json").exists(),
        "p1's live file is untouched"
    );
    assert!(!p2.join("api_keys.json").exists());
    assert!(p2.join("api_keys.json.retired").exists());
}

#[test]
fn retire_without_a_projects_dir_retires_nothing() {
    let dir = scratch("retire-none");
    assert!(retire_legacy_project_keys(&dir).is_empty());
}
