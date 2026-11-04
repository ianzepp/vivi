//! Whether a project's pump can run: ready, not ready, or invalid.
//!
//! A fresh `mailspace init` project has a starter `[pump]` table and nothing
//! else: no pool, no gate command. That is not an error. It is *not ready*,
//! and the pump holds until the missing items are supplied. A value that is
//! wrong (a bad number, a pool name that points at nothing) stays an error.
//! One assessment feeds `vivi config validate`, `vivi pump run`, the service
//! registry and `vivi pump health`, so they never disagree.

use crate::mailspace::Mailspace;
use crate::mailspace::shipped::pre_v20_charter;
use crate::pump::config::PumpConfig;
use crate::routing::{self, Issue, Severity};

/// Every finding about a project's pump: wrong values, missing items and
/// warnings, in report order. Missing items are [`Severity::NotReady`]
/// issues and are listed once per item.
#[must_use]
pub fn assess(mailspace: &Mailspace) -> Vec<Issue> {
    let mut issues = Vec::new();
    if let Some(cfg) = &mailspace.config.pump {
        config_issues(mailspace, cfg, &mut issues);
        charter_issues(mailspace, &mut issues);
    }
    issues.extend(routing::validate(&mailspace.config));
    cadence_issues(mailspace, &mut issues);
    dedupe_items(issues)
}

/// Every pump role's charter must carry the v20 reply envelope; one that
/// still asks for the 19.x format makes the pump's jobs unparseable.
fn charter_issues(mailspace: &Mailspace, issues: &mut Vec<Issue>) {
    for (role, _) in routing::pump_charter_roles(&mailspace.config) {
        match mailspace.read_charter(&role) {
            Ok(text) if pre_v20_charter(&text) => issues.push(Issue::error(format!(
                "charter {role}: no v20 reply envelope (it still asks for the 19.x format); \
                 run `vivi config migrate --write` to install the shipped charter"
            ))),
            Ok(_) => {}
            Err(err) => issues.push(Issue::error(format!("charter {role}: {err}"))),
        }
    }
}

/// Cadence jobs always run read-only, so a role that has a cadence but would
/// resolve writable gets one warning: its writes would never happen.
fn cadence_issues(mailspace: &Mailspace, issues: &mut Vec<Issue>) {
    for identity in &mailspace.config.identities {
        if identity.cadence.is_some()
            && !routing::resolve_read_only(false, identity.read_only, identity.kind.as_deref())
        {
            issues.push(Issue::warning(format!(
                "role {} has a cadence but is writable; cadence runs are read-only",
                identity.name
            )));
        }
    }
}

/// Whether the pump has everything it needs: no errors and nothing missing.
#[must_use]
pub fn is_ready(issues: &[Issue]) -> bool {
    issues
        .iter()
        .all(|issue| !matches!(issue.severity, Severity::Error | Severity::NotReady))
}

/// The multi-line form of the not-ready findings, one `- item: detail` line
/// each under a count header; empty when nothing is missing.
#[must_use]
pub fn render_missing(issues: &[Issue]) -> String {
    let items: Vec<&Issue> = routing::not_ready(issues).collect();
    if items.is_empty() {
        return String::new();
    }
    let mut lines = vec![format!("not ready: {} item(s) missing", items.len())];
    for issue in items {
        let item = issue.item.as_deref().unwrap_or("item");
        lines.push(format!("  - {item}: {}", issue.message));
    }
    lines.join("\n")
}

fn config_issues(mailspace: &Mailspace, cfg: &PumpConfig, issues: &mut Vec<Issue>) {
    let pools: Vec<String> = mailspace.config.pools.keys().cloned().collect();
    let found = cfg.findings_for(&mailspace.root, &pools);
    issues.extend(found.errors.into_iter().map(Issue::error));
    issues.extend(
        found
            .missing
            .into_iter()
            .map(|item| Issue::not_ready(item.name, item.detail)),
    );
    identity_issues(mailspace, cfg, issues);
}

/// The identities the pump speaks as: `[pump] from`, `to` and `reply_from`,
/// plus `[pump.board] reply_to` when set. Each must resolve to a role or
/// alias this mailspace knows, or the pump would start and then fail every
/// send it makes.
///
/// A role the pump names by its default (`operator`, `hand`, `mind`) that the
/// roster does not have yet is a missing role: a fresh project has none of
/// them. Any other name that resolves to nothing is a wrong value, most
/// likely a typo, and an error.
fn identity_issues(mailspace: &Mailspace, cfg: &PumpConfig, issues: &mut Vec<Issue>) {
    let default = PumpConfig::default();
    let mut pairs: Vec<(&str, &str, Option<&str>)> = vec![
        ("[pump] from", &cfg.from, Some(&default.from)),
        ("[pump] to", &cfg.to, Some(&default.to)),
        (
            "[pump] reply_from",
            &cfg.reply_from,
            Some(&default.reply_from),
        ),
    ];
    if let Some(reply_to) = cfg.board.reply_to.as_deref() {
        pairs.push(("[pump.board] reply_to", reply_to, None));
    }
    for (key, value, default) in pairs {
        let Err(err) = mailspace.resolve_identity(value) else {
            continue;
        };
        // `resolve_identity` words a well-formed name with no role as
        // "unknown local role"; anything else is a malformed value.
        let unknown = err.to_string().contains("unknown local role");
        if unknown && default == Some(value) {
            issues.push(Issue::not_ready(
                format!("role {value}"),
                format!("{key} = \"{value}\": no such role; add it with `vivi role add {value}`"),
            ));
        } else {
            issues.push(Issue::error(format!("{key} = \"{value}\": {err}")));
        }
    }
}

/// Keep the first not-ready finding per item; everything else passes through.
fn dedupe_items(issues: Vec<Issue>) -> Vec<Issue> {
    let mut seen = std::collections::BTreeSet::new();
    issues
        .into_iter()
        .filter(|issue| match (&issue.severity, &issue.item) {
            (Severity::NotReady, Some(item)) => seen.insert(item.clone()),
            _ => true,
        })
        .collect()
}

#[cfg(test)]
#[path = "readiness_test.rs"]
mod tests;
