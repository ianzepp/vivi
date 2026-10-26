//! The API key registry: the optional identity a key acts as.

use std::path::PathBuf;

use vivi::server::auth::sha256_hex;
use vivi::server::keys::{ApiKeyRegistry, KeyError};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vivi-server-keys-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn verify_returns_the_identity_a_key_was_minted_with() {
    let registry = ApiKeyRegistry::new(&scratch("with"));
    let minted = registry
        .mint("p1", "creator", "ops", Some("operator"))
        .unwrap();
    assert_eq!(minted.identity.as_deref(), Some("operator"));
    let verified = registry.verify("p1", &minted.key).unwrap();
    assert_eq!(verified.created_by, "creator");
    assert_eq!(verified.identity.as_deref(), Some("operator"));
    assert_eq!(registry.list("p1")[0].identity.as_deref(), Some("operator"));
}

#[test]
fn a_key_minted_without_an_identity_verifies_with_none() {
    let registry = ApiKeyRegistry::new(&scratch("without"));
    let minted = registry.mint("p1", "creator", "bot", None).unwrap();
    assert_eq!(minted.identity, None);
    let verified = registry.verify("p1", &minted.key).unwrap();
    assert_eq!(verified.created_by, "creator");
    assert_eq!(verified.identity, None);
    assert!(registry.verify("p1", "vivi_wrong").is_none());
}

#[test]
fn an_invalid_identity_is_refused_and_nothing_is_stored() {
    let registry = ApiKeyRegistry::new(&scratch("invalid"));
    for bad in ["", "two words", "a@b", &"x".repeat(65)] {
        assert!(matches!(
            registry.mint("p1", "creator", "bot", Some(bad)),
            Err(KeyError::InvalidIdentity)
        ));
    }
    assert!(registry.list("p1").is_empty());
}

#[test]
fn a_key_file_written_before_identities_still_loads() {
    let dir = scratch("legacy");
    let key = "vivi_legacykey";
    let file = format!(
        r#"{{"keys":[{{"key_id":"key_old","label":"old","digest":"{}","created_by":"creator","created_at":"2026-01-01T00:00:00Z"}}]}}"#,
        sha256_hex(key.as_bytes())
    );
    let project = dir.join("projects").join("p1");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("api_keys.json"), file).unwrap();

    let registry = ApiKeyRegistry::new(&dir);
    let verified = registry.verify("p1", key).unwrap();
    assert_eq!(verified.created_by, "creator");
    assert_eq!(verified.identity, None);
    assert_eq!(registry.list("p1")[0].key_id, "key_old");
    // A new key alongside the old one keeps both.
    registry
        .mint("p1", "creator", "new", Some("operator"))
        .unwrap();
    assert_eq!(registry.list("p1").len(), 2);
}
