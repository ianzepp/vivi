//! `POST /v1/reply/check/{kind}`: check a role reply with the pump's own
//! parser, so a pool agent learns of a malformed reply inside its session
//! (`vivi-submit` in the slot calls this) instead of after it has exited.
//!
//! Stateless and read-only: nothing is stored, so no key is needed. The
//! server listens on the tailnet address only.

use axum::extract::Path;
use axum::http::StatusCode;
use vivi_mail::letter::roles::{RoleKind, parse_for};

/// `200` with `accepted: answer <word>` when `body` parses as a `kind` reply,
/// `422` with the parser's one-line error otherwise, `404` for an unknown kind.
pub async fn reply_check_handler(Path(kind): Path<String>, body: String) -> (StatusCode, String) {
    check(&kind, &body)
}

/// The status and text of checking `body` as a reply of the role named `kind`.
#[must_use]
pub fn check(kind: &str, body: &str) -> (StatusCode, String) {
    let Some(role) = RoleKind::from_name(kind) else {
        return (
            StatusCode::NOT_FOUND,
            format!("unknown reply kind `{kind}`\n"),
        );
    };
    match parse_for(role, body) {
        Ok(parsed) => (
            StatusCode::OK,
            format!("accepted: answer {}\n", parsed.answer),
        ),
        Err(error) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("rejected: {error}\n"),
        ),
    }
}

#[cfg(test)]
#[path = "reply_check_test.rs"]
mod tests;
