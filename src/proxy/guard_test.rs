use std::net::SocketAddr;

use axum::http::{HeaderMap, HeaderValue, Method};

use super::guard::{Guard, GuardDeny};

fn guard() -> Guard {
    Guard {
        listen: "127.0.0.1:18180".parse::<SocketAddr>().unwrap(),
        allow_remote: false,
        require_bearer: false,
    }
}

fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (name, value) in pairs {
        map.insert(
            axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
            HeaderValue::from_str(value).unwrap(),
        );
    }
    map
}

#[test]
fn accepts_loopback_json_post() {
    let headers = headers(&[
        ("host", "127.0.0.1:18180"),
        ("content-type", "application/json"),
    ]);
    assert_eq!(guard().check(&Method::POST, &headers, None), None);
}

#[test]
fn rejects_origin() {
    let headers = headers(&[
        ("host", "127.0.0.1:18180"),
        ("content-type", "application/json"),
        ("origin", "https://evil.example"),
    ]);
    assert_eq!(
        guard().check(&Method::POST, &headers, None),
        Some(GuardDeny::Origin)
    );
}

#[test]
fn rejects_wrong_host() {
    let headers = headers(&[
        ("host", "evil.example"),
        ("content-type", "application/json"),
    ]);
    assert_eq!(
        guard().check(&Method::POST, &headers, None),
        Some(GuardDeny::Host)
    );
}

#[test]
fn rejects_missing_json_content_type() {
    let headers = headers(&[("host", "localhost")]);
    assert_eq!(
        guard().check(&Method::POST, &headers, None),
        Some(GuardDeny::ContentType)
    );
}

#[test]
fn requires_bearer_when_configured() {
    let mut policy = guard();
    policy.require_bearer = true;
    let headers = headers(&[
        ("host", "127.0.0.1:18180"),
        ("content-type", "application/json"),
        ("authorization", "Bearer secret"),
    ]);
    assert_eq!(policy.check(&Method::POST, &headers, Some("secret")), None);
    assert_eq!(
        policy.check(&Method::POST, &headers, Some("other")),
        Some(GuardDeny::Auth)
    );
}
