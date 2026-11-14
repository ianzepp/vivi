//! A small signed-in client for the needs pages' tests.

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use tower::ServiceExt;

use crate::test_support::{FakeHost, Seeded};

const KEY: &str = "vivi_test_key_123";

pub(crate) struct Reply {
    pub status: StatusCode,
    pub body: String,
}

pub(crate) struct Client {
    app: Router,
    cookie: String,
}

async fn send(app: &Router, request: Request<Body>) -> (StatusCode, axum::http::HeaderMap, String) {
    let response = app.clone().oneshot(request).await.unwrap();
    let (parts, body) = response.into_parts();
    let bytes = to_bytes(body, 4 * 1024 * 1024).await.unwrap();
    (
        parts.status,
        parts.headers,
        String::from_utf8_lossy(&bytes).to_string(),
    )
}

/// Sign in to a router over the seeded project.
pub(crate) async fn client(store: &Seeded) -> Client {
    let app = crate::router(Arc::new(FakeHost {
        keys: vec![(KEY.to_string(), store.principal())],
    }));
    let login = Request::builder()
        .method(Method::POST)
        .uri("/ui/login")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(format!("key={KEY}")))
        .unwrap();
    let (status, headers, _) = send(&app, login).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let cookie = headers
        .get(header::SET_COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .unwrap()
        .to_string();
    Client { app, cookie }
}

impl Client {
    pub(crate) async fn get(&self, uri: &str) -> Reply {
        let request = Request::builder()
            .uri(uri)
            .header(header::COOKIE, &self.cookie)
            .body(Body::empty())
            .unwrap();
        let (status, _, body) = send(&self.app, request).await;
        Reply { status, body }
    }
}
