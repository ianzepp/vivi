//! Mailspace addresses.
//!
//! Every mailspace has an id (a UUID in its uncommitted `mailspace.toml`).
//! The first 8 characters are its prefix, and `<prefix>.local` is the domain
//! of every address the mailspace mints: `mind@3f9a1c7e.local` is the `mind`
//! role of the mailspace whose id begins `3f9a1c7e`. An address with a
//! different prefix names a role in another mailspace, reachable only through
//! sync.

use std::collections::BTreeSet;

use super::Mailspace;
use super::config_file::PREFIX_LEN;
use super::identity::sanitize_identity;
use crate::error::VivariumError;

/// Longest local part the wire accepts for an address.
const MAX_LOCAL_LEN: usize = 64;

/// Where a recipient lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recipient {
    /// A role of this mailspace, by canonical name.
    Local(String),
    /// An address in another mailspace, lowercased: `role@<prefix>.local`.
    Remote(String),
}

/// The prefix a `<8 lowercase hex>.local` domain names, if it is one.
#[must_use]
pub fn prefix_of_domain(domain: &str) -> Option<&str> {
    let prefix = domain.strip_suffix(".local")?;
    let hex = prefix.len() == PREFIX_LEN
        && prefix
            .chars()
            .all(|ch| ch.is_ascii_digit() || ('a'..='f').contains(&ch));
    hex.then_some(prefix)
}

impl Mailspace {
    /// This mailspace's id (a lowercase hyphenated UUID). Every mailspace
    /// opened through [`Mailspace::init`], [`Mailspace::discover`] or
    /// [`Mailspace::open_root`] carries one; a hand-built value without an id
    /// answers the empty string and mints addresses under the legacy
    /// `<name>.local` domain.
    #[must_use]
    pub fn mailspace_id(&self) -> &str {
        self.config.id.as_deref().unwrap_or_default()
    }

    /// The first 8 characters of the id: the domain part of every address
    /// this mailspace mints.
    #[must_use]
    pub fn address_prefix(&self) -> &str {
        let id = self.mailspace_id();
        id.get(..PREFIX_LEN).unwrap_or(id)
    }

    /// The domain of this mailspace's own addresses: `<prefix>.local`.
    #[must_use]
    pub fn address_domain(&self) -> String {
        match self.address_prefix() {
            "" => self.legacy_domain(),
            prefix => format!("{prefix}.local"),
        }
    }

    /// The domain addresses carried before mailspaces had ids:
    /// `<mailspace-name>.local`.
    #[must_use]
    pub fn legacy_domain(&self) -> String {
        format!("{}.local", self.config.name)
    }

    /// Whether `domain` names this mailspace: its own prefix domain, the
    /// legacy name domain, or the bare `local`.
    #[must_use]
    pub fn is_own_domain(&self, domain: &str) -> bool {
        let domain = domain.to_ascii_lowercase();
        domain == "local"
            || domain == self.address_domain()
            || domain == self.legacy_domain().to_ascii_lowercase()
    }

    /// Sort a recipient value into a local role or a remote address.
    ///
    /// Bare names, aliases, `name@local`, and the own and legacy domains are
    /// local and must name a known role. `role@<8 lowercase hex>.local` with a
    /// prefix other than this mailspace's is remote, with no local role
    /// behind it. Anything else external is an error.
    ///
    /// # Errors
    /// Returns an error for an unknown local role, an invalid local part, or
    /// an external address that is not a mailspace address.
    pub fn classify_recipient(&self, value: &str) -> Result<Recipient, VivariumError> {
        let trimmed = value.trim();
        let Some((local, domain)) = trimmed.rsplit_once('@') else {
            return self.resolve_identity(trimmed).map(Recipient::Local);
        };
        let domain = domain.to_ascii_lowercase();
        if self.is_own_domain(&domain) || prefix_of_domain(&domain).is_none() {
            return self.resolve_identity(trimmed).map(Recipient::Local);
        }
        let local = sanitize_identity(local)?;
        if local.len() > MAX_LOCAL_LEN {
            return Err(VivariumError::Message(format!(
                "invalid remote address '{trimmed}'; the local part is over {MAX_LOCAL_LEN} characters"
            )));
        }
        Ok(Recipient::Remote(format!("{local}@{domain}")))
    }

    /// Split To and Cc values into the local roles that get a delivered copy
    /// and the remote addresses that do not, each sorted and deduplicated.
    ///
    /// # Errors
    /// Returns the first value that is neither a local role nor a remote
    /// address.
    pub(super) fn split_recipients(
        &self,
        to: &[String],
        cc: &[String],
    ) -> Result<(BTreeSet<String>, BTreeSet<String>), VivariumError> {
        let mut local = BTreeSet::new();
        let mut remote = BTreeSet::new();
        for value in to.iter().chain(cc) {
            match self.classify_recipient(value)? {
                Recipient::Local(identity) => local.insert(identity),
                Recipient::Remote(address) => remote.insert(address),
            };
        }
        Ok((local, remote))
    }

    /// The address a recipient value is written as in a header.
    ///
    /// # Errors
    /// Returns an error when the value is not a local role or a remote
    /// address.
    pub(super) fn header_address(&self, value: &str) -> Result<String, VivariumError> {
        Ok(match self.classify_recipient(value)? {
            Recipient::Local(identity) => self.address_for(&identity),
            Recipient::Remote(address) => address,
        })
    }
}

#[cfg(test)]
#[path = "address_test.rs"]
mod tests;
