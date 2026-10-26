//! The mind's human-only policy: the categories of action the mind must
//! hand to a human, and the structured escalation it sends when it stops.
//!
//! The list is policy, not habit: it lives in `mind.toml` as top-level
//! `human_only` and loads through [`crate::mind::config::MindConfig`]. A
//! category in the policy means the mind stops and escalates with an
//! [`Escalation`] instead of acting. Membership grants none of those
//! execution powers: the mind may not spend, rotate credentials, touch the
//! host root, cut releases, or settle product intent — asking is all the
//! policy does.
//!
//! The categories and what they cover:
//!
//! - `spending-accounts`: subscriptions, purchases, purchased quotas,
//!   account administration, GitHub or Tailscale account settings.
//! - `credentials`: keys, secrets, credential creation or rotation.
//! - `host-root`: root on the host machine.
//! - `releases`: release authorization, tags, promotion.
//! - `product-intent`: whether a need is wanted at all, or a design
//!   question the thread cannot settle.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::VivariumError;

/// Longest accepted `category` text.
pub const MAX_CATEGORY_LEN: usize = 64;
/// Longest accepted `why` text.
pub const MAX_WHY_LEN: usize = 1024;
/// Most accepted alternatives.
pub const MAX_OPTIONS: usize = 8;
/// Longest accepted alternative text.
pub const MAX_OPTION_LEN: usize = 256;

/// A category of action only a human may take.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum HumanCategory {
    /// Subscriptions, purchases, purchased quotas, account
    /// administration, GitHub or Tailscale account settings.
    SpendingAccounts,
    /// Keys, secrets, credential creation or rotation.
    Credentials,
    /// Root on the host machine.
    HostRoot,
    /// Release authorization, tags, promotion.
    Releases,
    /// Whether a need is wanted at all, or a design question the thread
    /// cannot settle.
    ProductIntent,
}

impl HumanCategory {
    /// Every category, in `mind.toml` name order.
    pub const ALL: [HumanCategory; 5] = [
        HumanCategory::SpendingAccounts,
        HumanCategory::Credentials,
        HumanCategory::HostRoot,
        HumanCategory::Releases,
        HumanCategory::ProductIntent,
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

    /// The category for a `mind.toml` name, if it is one. Action
    /// categories that are not human-only (say `flow-lock`) have none.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == name)
    }
}

impl fmt::Display for HumanCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The categories the mind must escalate to a human instead of acting on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HumanOnlyPolicy {
    categories: BTreeSet<HumanCategory>,
}

impl Default for HumanOnlyPolicy {
    fn default() -> Self {
        Self {
            categories: HumanCategory::ALL.iter().copied().collect(),
        }
    }
}

impl HumanOnlyPolicy {
    /// Build from the `human_only` configuration list: `None` keeps the
    /// default categories, an explicit list replaces them, and `[]`
    /// permits no escalation at all.
    ///
    /// # Errors
    /// Fails on a duplicate or an unknown category name.
    pub fn from_config(names: Option<Vec<String>>) -> Result<Self, VivariumError> {
        let Some(names) = names else {
            return Ok(Self::default());
        };
        let mut categories = BTreeSet::new();
        for name in names {
            let category = HumanCategory::parse(&name).ok_or_else(|| {
                VivariumError::Config(format!(
                    "human_only: unknown category {name:?}, expected one of {}",
                    names_of(HumanCategory::ALL)
                ))
            })?;
            if !categories.insert(category) {
                return Err(VivariumError::Config(format!(
                    "human_only: duplicate category {name:?}"
                )));
            }
        }
        Ok(Self { categories })
    }

    /// Whether `category` is in the policy and must go to a human.
    #[must_use]
    pub fn requires_human(&self, category: HumanCategory) -> bool {
        self.categories.contains(&category)
    }

    /// The categories in the policy.
    pub fn categories(&self) -> impl Iterator<Item = HumanCategory> + '_ {
        self.categories.iter().copied()
    }

    /// Whether no category escalates: nothing may be handed to a human.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.categories.is_empty()
    }
}

fn names_of(categories: [HumanCategory; 5]) -> String {
    categories
        .iter()
        .map(HumanCategory::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The structured escalation the mind sends a human when the policy stops
/// it: what kind of thing it needs, why, and the alternatives it sees.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Escalation {
    /// The action category, as submitted text. Held verbatim rather than
    /// parsed so an action category that is not human-only (say
    /// `flow-lock`) gets a typed [`EscalationRefusal`] from
    /// [`Escalation::validate`] instead of failing payload parsing.
    pub category: String,
    /// Why the mind cannot proceed without a human.
    pub why: String,
    /// The alternatives the mind sees, for the human to pick one or
    /// answer differently.
    pub options: Vec<String>,
}

/// Why the policy gate refused an [`Escalation`]. The category and
/// incomplete-request refusals are distinct variants so the action gate
/// can map them to different follow-ups.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EscalationRefusal {
    /// The category is not one the policy escalates: it is not a
    /// human-only category, or the policy does not cover it.
    Category {
        /// The category text as submitted.
        category: String,
    },
    /// The request is incomplete: a blank `why`, or fewer than two
    /// nonblank alternatives.
    Incomplete {
        /// What is missing, for the operator's letter.
        reason: String,
    },
    /// A field or alternative exceeds its finite limit. Refused, never
    /// clipped.
    TooLong {
        /// Which field: `category`, `why`, or `options[i]`.
        field: String,
        /// The submitted length in characters.
        len: usize,
        /// The limit.
        max: usize,
    },
    /// More alternatives than the finite cap.
    TooManyOptions {
        /// The submitted count.
        count: usize,
        /// The limit.
        max: usize,
    },
}

impl fmt::Display for EscalationRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Category { category } => write!(
                f,
                "category {category:?} is not a human-only category in the policy"
            ),
            Self::Incomplete { reason } => write!(f, "incomplete escalation: {reason}"),
            Self::TooLong { field, len, max } => {
                write!(f, "{field} is {len} characters, at most {max}")
            }
            Self::TooManyOptions { count, max } => write!(f, "{count} options, at most {max}"),
        }
    }
}

impl Escalation {
    /// Validate against `policy`: the category must be one the policy
    /// escalates, the request must be complete, and every field must fit
    /// its finite limit.
    pub fn validate(&self, policy: &HumanOnlyPolicy) -> Result<(), EscalationRefusal> {
        if self.category.chars().count() > MAX_CATEGORY_LEN {
            return Err(self.too_long("category", self.category.chars().count(), MAX_CATEGORY_LEN));
        }
        let Some(category) = HumanCategory::parse(&self.category) else {
            return Err(EscalationRefusal::Category {
                category: self.category.clone(),
            });
        };
        if !policy.requires_human(category) {
            return Err(EscalationRefusal::Category {
                category: self.category.clone(),
            });
        }
        if self.why.trim().is_empty() {
            return Err(EscalationRefusal::Incomplete {
                reason: "why is blank".into(),
            });
        }
        let why_len = self.why.chars().count();
        if why_len > MAX_WHY_LEN {
            return Err(self.too_long("why", why_len, MAX_WHY_LEN));
        }
        if self.options.len() > MAX_OPTIONS {
            return Err(EscalationRefusal::TooManyOptions {
                count: self.options.len(),
                max: MAX_OPTIONS,
            });
        }
        if self.options.iter().filter(|o| !o.trim().is_empty()).count() < 2 {
            return Err(EscalationRefusal::Incomplete {
                reason: "needs at least two nonblank options".into(),
            });
        }
        for (index, option) in self.options.iter().enumerate() {
            let len = option.chars().count();
            if len > MAX_OPTION_LEN {
                return Err(self.too_long(&format!("options[{index}]"), len, MAX_OPTION_LEN));
            }
        }
        Ok(())
    }

    fn too_long(&self, field: &str, len: usize, max: usize) -> EscalationRefusal {
        EscalationRefusal::TooLong {
            field: field.to_string(),
            len,
            max,
        }
    }
}

#[cfg(test)]
#[path = "policy_test.rs"]
mod tests;
