//! A signed-in browser over the real router, for page tests.

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use tower::ServiceExt;

use crate::host::Principal;
use crate::router::router;
use crate::test_support::FakeHost;

const KEY: &str = "vivi_test_key_123";

/// An app and the session cookie of a signed-in principal.
pub struct Browser {
    app: Router,
    cookie: String,
}

impl Browser {
    /// Sign `principal` in.
    pub async fn open(principal: Principal) -> Self {
        let app = router(Arc::new(FakeHost {
            keys: vec![(KEY.to_string(), principal)],
        }));
        let login = Request::builder()
            .method(Method::POST)
            .uri("/ui/login")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(format!("key={KEY}")))
            .unwrap();
        let reply = app.clone().oneshot(login).await.unwrap();
        assert_eq!(reply.status(), StatusCode::SEE_OTHER);
        let cookie = reply
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .unwrap()
            .to_string();
        Self { app, cookie }
    }

    /// GET `uri`: the status and the body text.
    pub async fn get(&self, uri: &str) -> (StatusCode, String) {
        let request = Request::builder()
            .method(Method::GET)
            .uri(uri)
            .header(header::COOKIE, &self.cookie)
            .body(Body::empty())
            .unwrap();
        let reply = self.app.clone().oneshot(request).await.unwrap();
        let status = reply.status();
        let bytes = to_bytes(reply.into_body(), 4 * 1024 * 1024).await.unwrap();
        (status, String::from_utf8_lossy(&bytes).to_string())
    }
}
