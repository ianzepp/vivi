//! In-memory console sessions.
//!
//! A session id is 32 random bytes in hex. The map holds the principal the
//! key resolved to, never the key. A session lives 12 hours from its last
//! use. Sessions do not survive a restart; sign in again.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use crate::host::Principal;

/// The cookie that carries the session id.
pub const COOKIE_NAME: &str = "vivi_console";

/// How long an unused session lives.
pub const SESSION_TTL: Duration = Duration::from_secs(12 * 60 * 60);

const ID_BYTES: usize = 32;
const MAX_SESSIONS: usize = 512;

struct Entry {
    principal: Principal,
    expires: Instant,
}

/// The session table.
pub struct SessionStore {
    ttl: Duration,
    entries: Mutex<HashMap<String, Entry>>,
}

impl SessionStore {
    #[must_use]
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            entries: Mutex::new(HashMap::new()),
        }
    }

    fn entries(&self) -> MutexGuard<'_, HashMap<String, Entry>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Start a session for `principal`; returns its id, or `None` when the
    /// system has no entropy.
    pub fn create(&self, principal: Principal) -> Option<String> {
        let mut raw = [0_u8; ID_BYTES];
        if getrandom::fill(&mut raw).is_err() {
            return None;
        }
        let id = hex(&raw);
        let now = Instant::now();
        let mut entries = self.entries();
        entries.retain(|_, entry| entry.expires > now);
        if entries.len() >= MAX_SESSIONS {
            let oldest = entries
                .iter()
                .min_by_key(|(_, entry)| entry.expires)
                .map(|(key, _)| key.clone());
            if let Some(key) = oldest {
                entries.remove(&key);
            }
        }
        entries.insert(
            id.clone(),
            Entry {
                principal,
                expires: now + self.ttl,
            },
        );
        Some(id)
    }

    /// The principal behind `id`, extending the session. `None` when the
    /// session is unknown or expired.
    pub fn touch(&self, id: &str) -> Option<Principal> {
        let now = Instant::now();
        let mut entries = self.entries();
        let live = entries.get(id).is_some_and(|entry| entry.expires > now);
        if !live {
            entries.remove(id);
            return None;
        }
        let entry = entries.get_mut(id)?;
        entry.expires = now + self.ttl;
        Some(entry.principal.clone())
    }

    /// End a session. An unknown id is not an error.
    pub fn destroy(&self, id: &str) {
        self.entries().remove(id);
    }

    /// How many sessions are held, expired ones included.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries().len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The `Set-Cookie` value that starts a session. No `Secure`: the console
/// is served over plain http on a LAN.
#[must_use]
pub fn set_cookie(id: &str) -> String {
    format!(
        "{COOKIE_NAME}={id}; HttpOnly; SameSite=Strict; Path=/ui; Max-Age={}",
        SESSION_TTL.as_secs()
    )
}

/// The `Set-Cookie` value that clears the session cookie.
#[must_use]
pub fn clear_cookie() -> String {
    format!("{COOKIE_NAME}=; HttpOnly; SameSite=Strict; Path=/ui; Max-Age=0")
}

/// The session id in a `Cookie` header value, if any.
#[must_use]
pub fn id_from_cookie_header(header: &str) -> Option<&str> {
    header.split(';').find_map(|pair| {
        let (name, value) = pair.trim().split_once('=')?;
        (name == COOKIE_NAME && !value.is_empty()).then_some(value)
    })
}

#[cfg(test)]
#[path = "session_test.rs"]
mod tests;
