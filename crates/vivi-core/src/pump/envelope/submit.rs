//! The hand-in section of a role brief: the agent checks its reply with
//! `vivi-submit` (installed in every slot by the job runner) against the
//! server's `POST /v1/reply/check/<kind>`, inside its own session, and the
//! runner takes the accepted reply as the job's receipt. A malformed reply is
//! then a rejected call the agent fixes and repeats, never a parse failure
//! found after the agent has exited.

use vivi_mail::letter::roles::RoleKind;

use crate::pump::service::REPLY_CHECK_ENV;

/// The heredoc delimiter the section tells the agent to use.
const DELIMITER: &str = "VIVI_REPLY";

/// The reply-check address of `kind`, from the server base in
/// [`REPLY_CHECK_ENV`]; none when the pump runs without a server.
#[must_use]
pub fn check_url(kind: RoleKind) -> Option<String> {
    let Ok(base) = std::env::var(REPLY_CHECK_ENV) else {
        return None;
    };
    let base = base.trim().trim_end_matches('/');
    if base.is_empty() {
        return None;
    }
    Some(format!("{base}/v1/reply/check/{}", kind.name()))
}

/// `brief` with the hand-in section for a `kind` reply appended; `brief`
/// unchanged when the pump has no reply-check server.
#[must_use]
pub fn with_section(brief: &str, kind: RoleKind) -> String {
    match check_url(kind) {
        Some(url) => format!("{brief}\n\n{}", section(&url)),
        None => brief.to_owned(),
    }
}

/// The hand-in section naming `url`.
#[must_use]
pub fn section(url: &str) -> String {
    format!(
        "## Hand in your reply (required)\n\n\
         When your work is done, hand your reply in with `vivi-submit` (it is on your PATH):\n\n\
         ```\n\
         vivi-submit {url} <<'{DELIMITER}'\n\
         +++\n\
         answer = \"...\"\n\
         ...the rest of the front matter, every table included...\n\
         +++\n\
         \n\
         Your report as prose.\n\
         {DELIMITER}\n\
         ```\n\n\
         It checks the reply with the pump's own parser. `accepted: ...` means it is \
         recorded as your result. `rejected: <reason>` means it was not: fix what the reason \
         names and run `vivi-submit` again, until it says `accepted`. Then stop; your final \
         chat message is not read when a reply was accepted."
    )
}

#[cfg(test)]
#[path = "submit_test.rs"]
mod tests;
