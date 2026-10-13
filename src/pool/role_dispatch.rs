//! Role-based dispatch, the pure half of `vivi pool dispatch`: take the
//! standing prompt, the read-only mode and the model from the role a task is
//! addressed to.
//!
//! The dispatch command looks the task's addressee up with
//! [`crate::mailspace::Mailspace::role_view`] and hands [`RoleFacts`] to
//! [`resolve`] together with the operator's flags and the task body. The
//! rules, in order of precedence:
//!
//! 1. Read-only: the `--read-only` flag, or the role's kind is `auditor`,
//!    `triage` or `cadence` (case-insensitive).
//! 2. Band: the body's `band:` line, else the role's `model` band string
//!    parsed with [`parse_band`].
//! 3. Model: `--model` wins; otherwise the band must resolve through the
//!    pool's bands table with [`choose`]. With `--model`, the band's
//!    reasoning is taken only when the pool's choice for that band names the
//!    same model.
//!
//! There is no failover, no health tracking and no retry on another model:
//! the pool's first choice for a band is the only choice. [`standing_prompt`]
//! builds the worker's standing prompt: the role's charter above the pool
//! rules, with a note on how the slot reaches the mailspace.

use crate::error::VivariumError;

use super::bands::{self, choose, parse_band};
use super::config::PoolConfig;

/// Role kinds that may not write: a dispatch to one is read-only whatever
/// the flags say. Compared case-insensitively.
const READ_ONLY_KINDS: [&str; 3] = ["auditor", "triage", "cadence"];

/// What the dispatch command knows about the role a task is addressed to,
/// taken from its role view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleFacts {
    /// The role's name, as it is addressed.
    pub name: String,
    /// The role's kind, e.g. `hand`, `auditor`, `triage`, `cadence`.
    pub kind: Option<String>,
    /// The role's `model` field: a band string such as `P2-S0`, not a model
    /// slug.
    pub band: Option<String>,
    /// The role's charter text, possibly blank.
    pub charter: String,
}

/// The dispatch command's inputs for one job: the operator's flags, the
/// role, and the task body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Input<'a> {
    /// The `--model` flag, when given. Always a model slug.
    pub model_flag: Option<&'a str>,
    /// The `--read-only` flag.
    pub read_only_flag: bool,
    /// The role the task is addressed to, when it has one.
    pub role: Option<&'a RoleFacts>,
    /// The task body, which may carry a `band:` line.
    pub body: &'a str,
}

/// How one job is to run: the model-and-reasoning pair, the band it came
/// from, the read-only mode, and the role it serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// Model slug the harness is run with.
    pub model: String,
    /// Reasoning effort the harness is run at, when a band chose the model.
    pub reasoning: Option<String>,
    /// The band the job runs at, when one was given.
    pub band: Option<u8>,
    /// Whether the job publishes nothing.
    pub read_only: bool,
    /// The role the job serves, when the task has one.
    pub role: Option<String>,
}

/// Resolve one dispatch: read-only, band and model from the role, the body
/// and the flags. Pure: no I/O and no CLI.
///
/// # Errors
/// Returns an error when the role's band string does not parse, when neither
/// `--model` nor any band names a model, when the pool has nothing for the
/// band, or when the chosen model is not one the pool harness offers.
pub fn resolve(config: &PoolConfig, pool: &str, input: &Input) -> Result<Resolved, VivariumError> {
    let band = resolve_band(input)?;
    let (model, reasoning) = match input.model_flag {
        Some(slug) => (
            slug.to_string(),
            matching_reasoning(config, pool, band, slug),
        ),
        None => {
            let band = band.ok_or_else(|| no_model(input.role))?;
            let choice = choose(config, pool, band)?;
            (choice.model, Some(choice.reasoning))
        }
    };
    check_model(config, &model)?;
    Ok(Resolved {
        model,
        reasoning,
        band,
        read_only: is_read_only(input),
        role: input.role.map(|role| role.name.clone()),
    })
}

/// The `--read-only` flag, or a role kind that may not write.
fn is_read_only(input: &Input) -> bool {
    input.read_only_flag
        || input
            .role
            .and_then(|role| role.kind.as_deref())
            .is_some_and(|kind| {
                READ_ONLY_KINDS
                    .iter()
                    .any(|ro| ro.eq_ignore_ascii_case(kind))
            })
}

/// The body's `band:` line, else the role's band string. The role's band is
/// only consulted (and so only refused when unparseable) when the body gives
/// no band of its own.
fn resolve_band(input: &Input) -> Result<Option<u8>, VivariumError> {
    if let Some(band) = bands::body_band(input.body)? {
        return Ok(Some(band));
    }
    let Some(role) = input.role else {
        return Ok(None);
    };
    let Some(band) = role.band.as_deref() else {
        return Ok(None);
    };
    parse_band(band)
        .map(Some)
        .ok_or_else(|| bad_role_band(&role.name, band))
}

/// The band choice's reasoning when the pool's choice for the band names the
/// same model as the flag, else none. A band the pool does not serve is not
/// an error here: `--model` already names a model.
fn matching_reasoning(
    config: &PoolConfig,
    pool: &str,
    band: Option<u8>,
    model: &str,
) -> Option<String> {
    let choice = choose(config, pool, band?).ok()?;
    (choice.model == model).then_some(choice.reasoning)
}

/// The chosen model must be one the harness offers. The bands table is
/// validated at load, but `--model` arrives straight from the operator.
fn check_model(config: &PoolConfig, model: &str) -> Result<(), VivariumError> {
    let Some(harness) = &config.harness else {
        return Ok(());
    };
    if harness.models.iter().any(|offered| offered == model) {
        return Ok(());
    }
    Err(VivariumError::Config(format!(
        "model \"{model}\" is not in harness.models ({})",
        harness.models.join(", ")
    )))
}

fn bad_role_band(role: &str, band: &str) -> VivariumError {
    VivariumError::Config(format!(
        "role {role} has band \"{band}\": use a band P0 to P5, optionally with a -S0 to -S3 speed"
    ))
}

fn no_model(role: Option<&RoleFacts>) -> VivariumError {
    match role {
        Some(role) => VivariumError::Config(format!(
            "no model: pass --model, give the task a `band:` line, \
             or give role {} a band (`vivi role set {} --model P2`)",
            role.name, role.name
        )),
        None => VivariumError::Config(
            "no model: pass --model, give the task a `band:` line, \
             or give the role a band (`vivi role set <name> --model P2`)"
                .to_string(),
        ),
    }
}

/// The worker's standing prompt: the role's charter above the pool rules
/// that bound it, with a note on how the slot reaches the mailspace. A blank
/// charter leaves the pool rules unchanged.
#[must_use]
pub fn standing_prompt(role: &str, charter: &str, pool_rules: &str, bridge: bool) -> String {
    if charter.trim().is_empty() {
        return pool_rules.to_string();
    }
    let note = if bridge {
        "You run in a pool slot with the vivi tools of the project server \
         (MCP server `vivi`). Your task handle is in your brief: read it \
         with `show` and `thread`, report with `reply`, and settle your \
         handle with `done` when the charter says so. The runner also \
         records your result."
    } else {
        "You run in a pool slot with no `vivi` command and no mailspace. \
         Where the charter tells you to read or settle your handle with \
         `vivi`, read your brief instead and end with your receipt on \
         standard output; the runner records your result and the Mind \
         settles the handle."
    };
    format!(
        "# Standing charter (role {role})\n\n{charter}\n\n---\n\n{note}\n\n\
         # Pool rules (these bound the charter)\n\n{pool_rules}"
    )
}

#[cfg(test)]
#[path = "role_dispatch_test.rs"]
mod tests;
