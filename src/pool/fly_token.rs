//! The Fly API token, fetched on demand and never held for the life of a
//! long process: a token from `fly auth token` can stop being accepted after a
//! while, and a `wait` runs far longer than that.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::github::Secret;
use crate::error::VivariumError;

/// A cached token is fetched again once it is this old.
pub const TOKEN_MAX_AGE: Duration = Duration::from_mins(1);

type Fetch = fn() -> Result<Secret, VivariumError>;

/// The current token, refreshed when old or when the API refused it.
#[derive(Debug)]
pub struct TokenCache {
    held: Mutex<Option<(Secret, Instant)>>,
    max_age: Duration,
    fetch: Fetch,
}

impl TokenCache {
    #[must_use]
    pub fn new(max_age: Duration, fetch: Fetch) -> Self {
        Self {
            held: Mutex::new(None),
            max_age,
            fetch,
        }
    }

    /// The cached token while it is fresh, else a newly fetched one.
    ///
    /// # Errors
    /// Returns the fetch error when no token can be had.
    pub fn get(&self) -> Result<Secret, VivariumError> {
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((token, at)) = held.as_ref()
            && at.elapsed() < self.max_age
        {
            return Ok(token.clone());
        }
        let token = (self.fetch)()?;
        *held = Some((token.clone(), Instant::now()));
        Ok(token)
    }

    /// Forget the cached token: the API refused it.
    pub fn invalidate(&self) {
        *self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }
}

#[cfg(test)]
#[path = "fly_token_test.rs"]
mod tests;
