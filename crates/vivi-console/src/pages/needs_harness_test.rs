//! A small client for the needs pages' tests.

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use crate::test_support::{FakeHost, Seeded};

pub(crate) struct Reply {
    pub status: StatusCode,
    pub body: String,
}

pub(crate) struct Client {
    app: Router,
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

/// A router over the seeded project.
pub(crate) async fn client(store: &Seeded) -> Client {
    let app = crate::router(Arc::new(FakeHost {
        catalog: store.catalog(),
    }));
    Client { app }
}

impl Client {
    pub(crate) async fn get(&self, uri: &str) -> Reply {
        let request = Request::builder().uri(uri).body(Body::empty()).unwrap();
        let (status, _, body) = send(&self.app, request).await;
        Reply { status, body }
    }
}
