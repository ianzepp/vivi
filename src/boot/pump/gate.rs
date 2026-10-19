//! Which pump snapshot a booting role sees.

use serde::Serialize;

use crate::mailspace::MailspaceConfig;
use crate::routing::{RoleRouting, RoutedRole};

/// Depth of the pump snapshot. Counts always print when a `pump.db` exists.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum PumpView {
    /// In-flight units, merge queue, stuck, pools, lanes.
    #[default]
    Full,
    /// Only the booting role's own unit(s).
    Own,
    /// Counts only.
    None,
}

impl PumpView {
    /// Stable slug for text and JSON.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Own => "own",
            Self::None => "none",
        }
    }
}

const COORD_KINDS: [&str; 12] = [
    "cadence",
    "merge",
    "cto",
    "lead",
    "auditor",
    "triage",
    "orchestrator",
    "mind",
    "operator",
    "steward",
    "head",
    "gate",
];

const COORD_NAMES: [&str; 7] = [
    "cto",
    "lead",
    "cadence",
    "merge",
    "auditor",
    "triage",
    "orchestrator",
];

/// `--pump` wins; else the role's default.
#[must_use]
pub fn choose_view(
    flag: Option<PumpView>,
    role: Option<&str>,
    config: &MailspaceConfig,
    in_slot: bool,
) -> PumpView {
    if let Some(view) = flag {
        return view;
    }
    let routing = crate::routing::from_mailspace(config);
    default_view(lookup(&routing, role), in_slot)
}

/// Coordinating seats and unknown identity see the full pump; a hand in a
/// pool (or a process inside a slot) sees only its own units.
#[must_use]
pub fn default_view(role: Option<&RoutedRole>, in_slot: bool) -> PumpView {
    match role {
        None if in_slot => PumpView::Own,
        None => PumpView::Full,
        Some(role) if coordinating(role) || role.pool.is_none() => PumpView::Full,
        Some(_) => PumpView::Own,
    }
}

fn lookup<'a>(routing: &'a RoleRouting, name: Option<&str>) -> Option<&'a RoutedRole> {
    name.and_then(|name| routing.lookup(name))
}

fn coordinating(role: &RoutedRole) -> bool {
    kind_in(role.kind.as_deref(), &COORD_KINDS) || name_hints(&role.name)
}

fn kind_in(kind: Option<&str>, known: &[&str]) -> bool {
    kind.is_some_and(|kind| known.iter().any(|want| kind.eq_ignore_ascii_case(want)))
}

fn name_hints(name: &str) -> bool {
    COORD_NAMES.iter().any(|hint| {
        name.eq_ignore_ascii_case(hint)
            || name
                .to_ascii_lowercase()
                .split(|c: char| !c.is_ascii_alphanumeric())
                .any(|part| part == *hint)
    })
}
