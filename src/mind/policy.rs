//! The mind's human-only policy: action categories only the operator may
//! take, written as configuration (`human_only` in `mind.toml`) so it is
//! policy, not habit.
//!
//! A category on the list is a reservation for the operator, nothing more:
//! membership grants the mind none of those execution powers. When the mind
//! meets a reserved action it escalates instead, with a structured
//! [`EscalationRequest`]. The payload keeps `category` as submitted text —
//! any word the mind writes (a known category, or an action category such as
//! `flow-lock`) validates to a typed [`PolicyRefusal::NotHumanOnly`] instead
//! of failing envelope parsing on an unknown enum variant.

use serde::{Deserialize, Serialize};

use crate::VivariumError;

/// An action category only the operator may take.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum HumanOnlyCategory {
    /// Spending from accounts the operator owns: subscriptions, purchases,
    /// purchased quotas, account administration, GitHub or Tailscale
    /// account settings.
    SpendingAccounts,
    /// Creating or rotating keys, secrets, or other credentials.
    Credentials,
    /// Acting on the host itself, as root.
    HostRoot,
    /// Authorizing a release, cutting tags, or promoting a build.
    Releases,
    /// Deciding whether a need is wanted at all, or settling a design
    /// question the thread cannot settle by itself.
    ProductIntent,
}

impl HumanOnlyCategory {
    /// Every category, in config-documentation order. The default policy
    /// reserves all of them.
    pub const ALL: &'static [Self] = &[
        Self::SpendingAccounts,
        Self::Credentials,
        Self::HostRoot,
        Self::Releases,
        Self::ProductIntent,
    ];

    /// The kebab-case name, as written in `mind.toml`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SpendingAccounts => "spending-accounts",
            Self::Credentials => "credentials",
            Self::HostRoot => "host-root",
            Self::Releases => "releases",
            Self::ProductIntent => "product-intent",
        }
    }

    /// Parse a category name as submitted or as configured. Case-sensitive,
    /// like every other `mind.toml` value.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|c| c.as_str() == name)
    }

    /// What the category covers, for the mind's brief and operator docs.
    #[must_use]
    pub fn meaning(self) -> &'static str {
        match self {
            Self::SpendingAccounts => {
                "spending from the operator's accounts: subscriptions, \
                 purchases, purchased quotas, account administration, \
                 GitHub or Tailscale account settings"
            }
            Self::Credentials => "creating or rotating keys, secrets, or other credentials",
            Self::HostRoot => "acting on the host itself, as root",
            Self::Releases => "authorizing a release, cutting tags, or promoting a build",
            Self::ProductIntent => {
                "deciding whether a need is wanted at all, or settling a \
                 design question the thread cannot settle"
            }
        }
    }
}

/// Which action categories are reserved for the operator. An empty policy
/// permits no escalation at all: the mind acts inside its authority stage
/// or it does not act.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HumanOnlyPolicy {
    /// The reserved categories, in the order the operator listed them.
    pub categories: Vec<HumanOnlyCategory>,
}

impl Default for HumanOnlyPolicy {
    fn default() -> Self {
        Self {
            categories: Self::default_categories(),
        }
    }
}

impl HumanOnlyPolicy {
    /// The categories a missing `human_only` key applies: every known one.
    pub fn default_categories() -> Vec<HumanOnlyCategory> {
        HumanOnlyCategory::ALL.to_vec()
    }

    /// Build a policy from the names `mind.toml` listed. An explicit list
    /// replaces the defaults; duplicates and unknown names are refused.
    ///
    /// # Errors
    /// Fails on a duplicate or unknown category name.
    pub fn from_names(names: &[String]) -> Result<Self, VivariumError> {
        let mut categories = Vec::with_capacity(names.len());
        for name in names {
            let category = HumanOnlyCategory::parse(name).ok_or_else(|| {
                VivariumError::Config(format!(
                    "human_only: unknown category {name:?}; known: {}",
                    HumanOnlyCategory::ALL
                        .iter()
                        .map(|c| c.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            })?;
            if categories.contains(&category) {
                return Err(VivariumError::Config(format!(
                    "human_only: duplicate category {name:?}"
                )));
            }
            categories.push(category);
        }
        Ok(Self { categories })
    }

    /// Whether `category`, as submitted text, is reserved for the operator.
    /// Unknown text is never reserved.
    #[must_use]
    pub fn contains(&self, category: &str) -> bool {
        HumanOnlyCategory::parse(category).is_some_and(|c| self.categories.contains(&c))
    }

    /// Typed membership check for the action gate: a reserved category
    /// comes back parsed; anything else — unknown text, or a known category
    /// this policy does not reserve — comes back as
    /// [`PolicyRefusal::NotHumanOnly`] carrying the text verbatim.
    ///
    /// # Errors
    /// Fails with [`PolicyRefusal::NotHumanOnly`] when not reserved.
    pub fn ensure_human_only(&self, category: &str) -> Result<HumanOnlyCategory, PolicyRefusal> {
        HumanOnlyCategory::parse(category)
            .filter(|c| self.categories.contains(c))
            .ok_or_else(|| PolicyRefusal::NotHumanOnly {
                category: category.to_string(),
            })
    }
}

/// Longest `request` or `why` text, in characters.
pub const MAX_TEXT_CHARS: usize = 2_000;
/// Most alternatives one escalation may offer.
pub const MAX_OPTIONS: usize = 8;
/// Longest one alternative, in characters.
pub const MAX_OPTION_CHARS: usize = 200;
/// Fewest nonblank alternatives a complete escalation offers.
pub const MIN_OPTIONS: usize = 2;

/// Why an escalation was refused. The action gate maps
/// [`PolicyRefusal::NotHumanOnly`] to a typed policy refusal carrying the
/// category exactly as submitted, and [`PolicyRefusal::Incomplete`] to a
/// bad-request reply naming the field.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PolicyRefusal {
    /// The category is not reserved by this policy: unknown text, or a
    /// known category the operator did not list.
    #[error("category {category:?} is not human-only under this policy")]
    NotHumanOnly {
        /// The category exactly as submitted.
        category: String,
    },
    /// The escalation payload is incomplete or oversized.
    #[error("escalation {field} is incomplete or oversized")]
    Incomplete {
        /// Which field failed: `category`, `request`, `why`, or `options`.
        field: &'static str,
    },
}

/// The structured payload the mind sends when it needs the operator to act.
/// The category is retained as submitted text so any action category —
/// known or not — reaches the gate without envelope-parsing failures.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EscalationRequest {
    /// The action category, as submitted text (`credentials`, `flow-lock`).
    pub category: String,
    /// What the operator is asked to do, complete enough to act on.
    pub request: String,
    /// Why the mind cannot or must not do it itself.
    pub why: String,
    /// The alternatives the operator chooses between; at least
    /// [`MIN_OPTIONS`], each nonblank.
    pub options: Vec<String>,
}

impl EscalationRequest {
    /// Validate the payload against `policy`: shape first, then category
    /// membership. Nothing is clipped — an oversized payload is refused.
    ///
    /// # Errors
    /// Fails with [`PolicyRefusal::Incomplete`] on a blank or oversized
    /// field, or too few alternatives; [`PolicyRefusal::NotHumanOnly`] when
    /// the category is not reserved.
    pub fn validate(&self, policy: &HumanOnlyPolicy) -> Result<HumanOnlyCategory, PolicyRefusal> {
        let nonblank = |text: &str| !text.trim().is_empty();
        let fits = |text: &str, max: usize| text.chars().count() <= max;
        if !nonblank(&self.category) {
            return Err(PolicyRefusal::Incomplete { field: "category" });
        }
        if !nonblank(&self.request) || !fits(&self.request, MAX_TEXT_CHARS) {
            return Err(PolicyRefusal::Incomplete { field: "request" });
        }
        if !nonblank(&self.why) || !fits(&self.why, MAX_TEXT_CHARS) {
            return Err(PolicyRefusal::Incomplete { field: "why" });
        }
        if self.options.len() < MIN_OPTIONS
            || self.options.len() > MAX_OPTIONS
            || !self
                .options
                .iter()
                .all(|option| nonblank(option) && fits(option, MAX_OPTION_CHARS))
        {
            return Err(PolicyRefusal::Incomplete { field: "options" });
        }
        policy.ensure_human_only(&self.category)
    }
}

#[cfg(test)]
#[path = "policy_test.rs"]
mod tests;
