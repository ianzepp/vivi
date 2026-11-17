//! A browser over the real router, for page tests.

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode};
use tower::ServiceExt;

use crate::host::Catalog;
use crate::router::router;
use crate::test_support::FakeHost;

/// An app over a fixed catalog.
pub struct Browser {
    app: Router,
}

impl Browser {
    /// A browser over a console that shows `catalog`.
    pub async fn open(catalog: Catalog) -> Self {
        let app = router(Arc::new(FakeHost { catalog }));
        Self { app }
    }

    /// GET `uri`: the status and the body text.
    pub async fn get(&self, uri: &str) -> (StatusCode, String) {
        let request = Request::builder()
            .method(Method::GET)
            .uri(uri)
            .body(Body::empty())
            .unwrap();
        let reply = self.app.clone().oneshot(request).await.unwrap();
        let status = reply.status();
        let bytes = to_bytes(reply.into_body(), 4 * 1024 * 1024).await.unwrap();
        (status, String::from_utf8_lossy(&bytes).to_string())
    }
}
