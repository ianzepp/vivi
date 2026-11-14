use super::*;
use crate::host::ProjectRef;

fn principal() -> Principal {
    Principal {
        name: "alice".into(),
        projects: vec![ProjectRef {
            id: "p1".into(),
            name: "demo".into(),
            mailspace_root: "/tmp/demo".into(),
        }],
    }
}

#[test]
fn ids_are_64_hex_characters_and_unique() {
    let store = SessionStore::new(SESSION_TTL);
    let a = store.create(principal()).unwrap();
    let b = store.create(principal()).unwrap();
    assert_eq!(a.len(), 64);
    assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    assert_ne!(a, b);
}

#[test]
fn touch_returns_the_principal_and_destroy_ends_the_session() {
    let store = SessionStore::new(SESSION_TTL);
    let id = store.create(principal()).unwrap();
    assert_eq!(store.touch(&id).unwrap().name, "alice");
    store.destroy(&id);
    assert!(store.touch(&id).is_none());
    assert!(store.touch("nope").is_none());
}

#[test]
fn an_expired_session_is_gone() {
    let store = SessionStore::new(Duration::ZERO);
    let id = store.create(principal()).unwrap();
    assert!(store.touch(&id).is_none());
    assert!(store.is_empty());
}

#[test]
fn use_extends_the_session() {
    let store = SessionStore::new(Duration::from_millis(400));
    let id = store.create(principal()).unwrap();
    std::thread::sleep(Duration::from_millis(250));
    assert!(store.touch(&id).is_some());
    std::thread::sleep(Duration::from_millis(250));
    assert!(
        store.touch(&id).is_some(),
        "the touch must have slid the expiry"
    );
}

#[test]
fn the_cookie_is_http_only_strict_and_scoped_to_ui() {
    let cookie = set_cookie("abc");
    assert!(cookie.starts_with("vivi_console=abc;"));
    assert!(cookie.contains("HttpOnly"));
    assert!(cookie.contains("SameSite=Strict"));
    assert!(cookie.contains("Path=/ui"));
    assert!(!cookie.contains("Secure"));
    assert!(clear_cookie().contains("Max-Age=0"));
}

#[test]
fn the_session_id_is_found_among_other_cookies() {
    assert_eq!(
        id_from_cookie_header("a=1; vivi_console=xyz; b=2"),
        Some("xyz")
    );
    assert_eq!(id_from_cookie_header("a=1"), None);
    assert_eq!(id_from_cookie_header("vivi_console="), None);
}
