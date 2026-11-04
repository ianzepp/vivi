//! Host / Origin / Content-Type / optional bearer checks.

use std::net::SocketAddr;

use axum::http::{HeaderMap, Method};

/// Why a request was refused at the front door.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardDeny {
    Host,
    Origin,
    ContentType,
    Auth,
}

/// Front-door policy derived from the loaded config.
#[derive(Debug, Clone)]
pub struct Guard {
    pub listen: SocketAddr,
    pub allow_remote: bool,
    pub require_bearer: bool,
}

impl Guard {
    /// Check headers for one inbound request.
    #[must_use]
    pub fn check(
        &self,
        method: &Method,
        headers: &HeaderMap,
        bearer: Option<&str>,
    ) -> Option<GuardDeny> {
        if !host_allowed(headers, self.listen, self.allow_remote) {
            return Some(GuardDeny::Host);
        }
        if headers.contains_key(axum::http::header::ORIGIN) {
            return Some(GuardDeny::Origin);
        }
        if needs_json(method) && !json_content_type(headers) {
            return Some(GuardDeny::ContentType);
        }
        if self.require_bearer && !bearer_matches(headers, bearer) {
            return Some(GuardDeny::Auth);
        }
        None
    }
}

fn needs_json(method: &Method) -> bool {
    matches!(*method, Method::POST | Method::PUT | Method::PATCH)
}

fn json_content_type(headers: &HeaderMap) -> bool {
    let Some(value) = headers.get(axum::http::header::CONTENT_TYPE) else {
        return false;
    };
    let Ok(text) = value.to_str() else {
        return false;
    };
    let media = text.split(';').next().unwrap_or(text).trim();
    media.eq_ignore_ascii_case("application/json")
}

fn bearer_matches(headers: &HeaderMap, expected: Option<&str>) -> bool {
    let Some(expected) = expected.filter(|token| !token.is_empty()) else {
        return false;
    };
    let Some(value) = headers.get(axum::http::header::AUTHORIZATION) else {
        return false;
    };
    let Ok(text) = value.to_str() else {
        return false;
    };
    let Some(got) = text.strip_prefix("Bearer ") else {
        return false;
    };
    got == expected
}

fn host_allowed(headers: &HeaderMap, listen: SocketAddr, allow_remote: bool) -> bool {
    let Some(value) = headers.get(axum::http::header::HOST) else {
        return false;
    };
    let Ok(host) = value.to_str() else {
        return false;
    };
    let hostname = host.split(':').next().unwrap_or(host).trim();
    if hostname.is_empty() {
        return false;
    }
    if is_loopback_host(hostname) {
        return true;
    }
    if hostname.eq_ignore_ascii_case(&listen.ip().to_string()) {
        return true;
    }
    allow_remote
}

fn is_loopback_host(hostname: &str) -> bool {
    hostname.eq_ignore_ascii_case("localhost")
        || hostname == "127.0.0.1"
        || hostname == "[::1]"
        || hostname == "::1"
}

impl GuardDeny {
    /// Fixed client message. Never includes header values.
    #[must_use]
    pub fn message(self) -> &'static str {
        match self {
            Self::Host => "invalid host",
            Self::Origin => "browser origin refused",
            Self::ContentType => "content-type must be application/json",
            Self::Auth => "unauthorized",
        }
    }

    /// Stable error code for the JSON body.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::Host => "bad_host",
            Self::Origin => "bad_origin",
            Self::ContentType => "bad_content_type",
            Self::Auth => "unauthorized",
        }
    }

    /// HTTP status for this denial.
    #[must_use]
    pub fn status(self) -> u16 {
        match self {
            Self::Auth => 401,
            Self::Host | Self::Origin | Self::ContentType => 403,
        }
    }
}
