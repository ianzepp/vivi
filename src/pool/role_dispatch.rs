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
//! 2. Band: the task front matter's `band` key, else the role's `model` band string
//!    parsed with [`parse_band`]. A role `model` that is not a band but is a
//!    model in the pool's `harness.models` is a model slug, not a band.
//! 3. Model: `--model` wins; then the task front matter's `band` key; then the role's slug;
//!    then the role's band, which must resolve through the pool's bands table
//!    with a weighted draw ([`choose`], or [`choose_eligible`] once
//!    eligibility applies). With `--model`, the reasoning is the first entry
//!    of the applicable band's list that names the same model, else none. A
//!    role's slug takes the role's `thinking`, else the reasoning of the
//!    first band entry (P0 to P5, any position) that names the slug, else
//!    none.
//!
//! There is no failover and no retry on another model: one decision — a
//! pinned slug or one weighted band draw — resolves the job, and nothing
//! falls back to a second choice. [`resolve`] is the routing contract and
//! never consults health; [`resolve_eligible`] layers the pool's
//! [`Availability`] view on top and answers [`DispatchResolution::Waiting`]
//! — never another entry — when a pinned slug is blocked or a band's every
//! entry is. [`standing_prompt`] builds the worker's standing prompt: the
//! role's charter above the pool rules, with a note on how the slot reaches
//! the mailspace.

use crate::error::VivariumError;
use crate::mailspace::Mailspace;
use crate::routing::resolve_read_only;

use super::bands::availability::{Availability, Unavailable, WaitingReason};
use super::bands::{self, choose, choose_eligible, parse_band};
use super::config::PoolConfig;

/// What the dispatch command knows about the role a task is addressed to,
/// taken from its role view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleFacts {
    /// The role's name, as it is addressed.
    pub name: String,
    /// The role's kind, e.g. `hand`, `auditor`, `triage`, `cadence`.
    pub kind: Option<String>,
    /// The role's `model` field: a band string such as `P2-S0`, or a model
    /// slug that is in the pool's `harness.models`.
    pub band: Option<String>,
    /// The role's reasoning effort, used with a model slug in `band`.
    pub thinking: Option<String>,
    /// The role's charter text, possibly blank.
    pub charter: String,
    /// Explicit `read_only` on the role, when set.
    pub read_only: Option<bool>,
}

impl RoleFacts {
    /// The shipped Auditor role used by an explicit built-in fallback when a
    /// mailspace has no local auditor identity.
    #[must_use]
    pub fn builtin_auditor() -> Self {
        Self {
            name: String::from("auditor"),
            kind: Some(String::from("auditor")),
            band: None,
            thinking: None,
            charter: crate::mailspace::shipped::AUDITOR_CHARTER.to_string(),
            read_only: Some(true),
        }
    }
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
    /// The task body, which may carry a `band` key in its front matter.
    pub body: &'a str,
}

/// How one job is to run: the model-and-reasoning pair, the band it came
/// from, the read-only mode, and the role it serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// Model slug the harness is run with.
    pub model: String,
    /// Reasoning effort the harness is run at, when a band or a role slug
    /// chose the model.
    pub reasoning: Option<String>,
    /// The band the job runs at, when one was given. None for a role slug.
    pub band: Option<u8>,
    /// Whether the job publishes nothing.
    pub read_only: bool,
    /// The role the job serves, when the task has one.
    pub role: Option<String>,
}

/// The outcome of an eligibility-aware resolution: a job to dispatch, or a
/// reason to wait.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchResolution {
    /// The job can run now: the same [`Resolved`] [`resolve`] answers.
    Ready(Resolved),
    /// Every path to a model this input allows is blocked at the view's
    /// instant: a pinned slug, or a band whose every entry is blocked.
    Waiting(Unavailable),
}

/// Resolve one dispatch: read-only, band and model from the role, the body
/// and the flags. Pure: no I/O and no CLI. The routing contract: eligibility
/// never enters, [`resolve_eligible`] layers it on top.
///
/// # Errors
/// Returns an error when the role's band string does not parse, when neither
/// `--model` nor any band names a model, when the pool has nothing for the
/// band, or when the chosen model is not one the pool harness offers.
pub fn resolve(config: &PoolConfig, pool: &str, input: &Input) -> Result<Resolved, VivariumError> {
    let (band, pick) = precedence(config, input)?;
    let (model, reasoning) = match pick {
        Pick::Pinned { model, reasoning } => (model, reasoning),
        Pick::Band(band) => choose_pair(config, pool, band)?,
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

/// Resolve one dispatch the way [`resolve`] does, then hold it against the
/// pool's [`Availability`] view: a pinned model (`--model` or the role's
/// slug) that is blocked waits rather than substituting another entry, and a
/// band whose every entry is blocked waits rather than drawing. Configuration
/// errors stay errors whatever the view says: a band the pool does not serve,
/// a role band that does not parse, a model the harness does not offer.
///
/// # Errors
/// Returns the same errors [`resolve`] returns: eligibility never turns a
/// configuration error into a wait.
pub fn resolve_eligible(
    config: &PoolConfig,
    pool: &str,
    input: &Input,
    availability: &Availability,
) -> Result<DispatchResolution, VivariumError> {
    let (band, pick) = precedence(config, input)?;
    let (model, reasoning) = match pick {
        Pick::Pinned { model, reasoning } => {
            check_model(config, &model)?;
            if let Some(unavailable) = availability.unavailable([&model]) {
                return Ok(DispatchResolution::Waiting(unavailable));
            }
            (model, reasoning)
        }
        Pick::Band(band) => match choose_eligible(config, pool, band, availability)? {
            Some(choice) => {
                check_model(config, &choice.model)?;
                (choice.model, Some(choice.reasoning))
            }
            None => {
                return Ok(DispatchResolution::Waiting(all_blocked(
                    config,
                    band,
                    availability,
                )));
            }
        },
    };
    Ok(DispatchResolution::Ready(Resolved {
        model,
        reasoning,
        band,
        read_only: is_read_only(input),
        role: input.role.map(|role| role.name.clone()),
    }))
}

/// What the precedence rules pick before eligibility is applied: a pinned
/// model that is never substituted for another entry, or a band whose entry
/// the pool draws (or, under [`resolve_eligible`], filters).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Pick {
    /// `--model`, or the role's own slug: blocked means wait, never another
    /// entry.
    Pinned {
        /// Model slug the harness is run with.
        model: String,
        /// Reasoning effort the harness is run at, when one is known.
        reasoning: Option<String>,
    },
    /// The band's entry comes from the pool's bands table.
    Band(u8),
}

/// The precedence core shared by [`resolve`] and [`resolve_eligible`]:
/// read-only aside, the band the job runs at and what picks its model.
///
/// # Errors
/// Returns an error when the role's band string does not parse or when
/// neither `--model` nor any band names a model.
fn precedence(config: &PoolConfig, input: &Input) -> Result<(Option<u8>, Pick), VivariumError> {
    let band = resolve_band(config, input)?;
    let slug = role_slug(config, input.role);
    let pick = match input.model_flag {
        Some(slug_flag) if slug.is_some_and(|role| role.0 == slug_flag) => Pick::Pinned {
            model: slug_flag.to_string(),
            reasoning: slug_reasoning(config, slug),
        },
        Some(flag) => Pick::Pinned {
            model: flag.to_string(),
            reasoning: matching_reasoning(config, band, flag),
        },
        None => match (band, slug) {
            (Some(band), _) => Pick::Band(band),
            (None, Some(role)) => Pick::Pinned {
                model: role.0.to_string(),
                reasoning: slug_reasoning(config, Some(role)),
            },
            (None, None) => return Err(no_model(input.role)),
        },
    };
    Ok((band, pick))
}

/// The [`Unavailable`] behind a band whose every entry the view blocks: the
/// same entries [`choose_eligible`] found nothing left of, aggregated by
/// [`Availability::unavailable`]. That aggregation cannot come back `None`
/// here — an entry survives only when it is neither capped nor cooling, and
/// a band with a surviving entry is never all-blocked — so the empty
/// fallback is a shape to stay total, never a state the pool reaches.
fn all_blocked(config: &PoolConfig, band: u8, availability: &Availability) -> Unavailable {
    let models: Vec<&str> = config
        .bands
        .entries
        .get(&format!("P{band}"))
        .into_iter()
        .flatten()
        .map(|entry| entry.model.as_str())
        .collect();
    availability
        .unavailable(models)
        .unwrap_or_else(|| Unavailable {
            blocked_models: Vec::new(),
            reason: WaitingReason::ModelsUnavailable,
            until: None,
        })
}

/// Require privileged publication to be explicitly addressed to merge/release
/// roles, to a hand job publishing only its unit-namespace branch, or to a
/// repairer job publishing only one `vivi/repair/<segment>` branch. A merge
/// role publishes only a configured main (`mains`, one branch name per
/// target), a `vivi/need/<segment>` or `vivi/need-prior/<segment>` branch, or
/// a `pump/merge-main/<segment>` scratch branch.
/// A release role binds publication to one expected full commit SHA.
pub fn validate_publish(
    role: Option<&RoleFacts>,
    publish_ref: Option<&str>,
    publish_expect: Option<&str>,
    mains: &[String],
) -> Result<(), VivariumError> {
    let Some(reference) = publish_ref else {
        return if publish_expect.is_none() {
            Ok(())
        } else {
            Err(VivariumError::Config(
                "publish expectation requires a publish ref".into(),
            ))
        };
    };
    let kind = role.and_then(|role| role.kind.as_deref());
    match kind {
        Some(kind) if kind.eq_ignore_ascii_case("merge") && merge_publish_ref(reference, mains) => {
            Ok(())
        }
        Some(kind) if kind.eq_ignore_ascii_case("release") && publish_expect.is_some() => Ok(()),
        Some(kind) if kind.eq_ignore_ascii_case("release") => Err(VivariumError::Config(
            "release publication requires an exact expected commit SHA".into(),
        )),
        Some(kind) if kind.eq_ignore_ascii_case("hand") && unit_publish_ref(reference) => Ok(()),
        Some(kind) if kind.eq_ignore_ascii_case("repairer") && repair_publish_ref(reference) => {
            Ok(())
        }
        _ => Err(VivariumError::Config(format!(
            "role {} is not authorized to publish {reference}",
            role.map_or("(none)", |role| role.name.as_str())
        ))),
    }
}

fn merge_publish_ref(reference: &str, mains: &[String]) -> bool {
    one_segment(reference, "refs/heads/vivi/need/")
        || one_segment(reference, "refs/heads/vivi/need-prior/")
        || one_segment(reference, "refs/heads/pump/merge-main/")
        || mains
            .iter()
            .any(|main| reference.strip_prefix("refs/heads/") == Some(main.as_str()))
}

fn one_segment(reference: &str, prefix: &str) -> bool {
    reference
        .strip_prefix(prefix)
        .is_some_and(|rest| !rest.is_empty() && !rest.contains('/'))
}

fn unit_publish_ref(reference: &str) -> bool {
    reference
        .strip_prefix("refs/heads/vivi/unit/")
        .is_some_and(|rest| !rest.is_empty() && !rest.contains('/'))
}

fn repair_publish_ref(reference: &str) -> bool {
    reference
        .strip_prefix("refs/heads/vivi/repair/")
        .is_some_and(|rest| !rest.is_empty() && !rest.contains('/'))
}

/// The `--read-only` flag, the role's `read_only`, or the kind default.
fn is_read_only(input: &Input) -> bool {
    resolve_read_only(
        input.read_only_flag,
        input.role.and_then(|role| role.read_only),
        input.role.and_then(|role| role.kind.as_deref()),
    )
}

/// The task front matter's `band` key, else the role's band string. The role's band is
/// only consulted (and so only refused when neither a band nor a harness
/// model) when the body gives no band of its own. A role model slug is no
/// band.
fn resolve_band(config: &PoolConfig, input: &Input) -> Result<Option<u8>, VivariumError> {
    if let Some(band) = bands::body_band(input.body)? {
        return Ok(Some(band));
    }
    let Some(role) = input.role else {
        return Ok(None);
    };
    let Some(band) = role.band.as_deref() else {
        return Ok(None);
    };
    match parse_band(band) {
        Some(band) => Ok(Some(band)),
        None if offers(config, band) => Ok(None),
        None => Err(bad_role_band(config, &role.name, band)),
    }
}

/// Whether the pool harness offers `model`.
fn offers(config: &PoolConfig, model: &str) -> bool {
    config
        .harness
        .as_ref()
        .is_some_and(|harness| harness.models.iter().any(|offered| offered == model))
}

/// The role's model slug and its facts: the role's `model` when it is not a
/// band but is a model the pool harness offers.
fn role_slug<'a>(
    config: &PoolConfig,
    role: Option<&'a RoleFacts>,
) -> Option<(&'a str, &'a RoleFacts)> {
    let role = role?;
    let value = role.band.as_deref()?;
    (parse_band(value).is_none() && offers(config, value)).then_some((value, role))
}

/// The reasoning for a role's slug: the role's `thinking`, else the first
/// band entry (P0 to P5, any position) that names the slug, else none.
fn slug_reasoning(config: &PoolConfig, slug: Option<(&str, &RoleFacts)>) -> Option<String> {
    let (model, role) = slug?;
    if let Some(thinking) = role.thinking.as_deref().filter(|t| !t.trim().is_empty()) {
        return Some(thinking.to_string());
    }
    (0..=5).find_map(|band| {
        config
            .bands
            .entries
            .get(&format!("P{band}"))?
            .iter()
            .find(|entry| entry.model == model)
            .map(|entry| entry.reasoning.clone())
    })
}

/// The pool's model and reasoning for `band`.
fn choose_pair(
    config: &PoolConfig,
    pool: &str,
    band: u8,
) -> Result<(String, Option<String>), VivariumError> {
    let choice = choose(config, pool, band)?;
    Ok((choice.model, Some(choice.reasoning)))
}

/// The reasoning of the first entry of the applicable band's list that names
/// the same model as the flag, else none. The lookup is deterministic: it
/// never depends on which entry serves the band. A band the pool does not
/// serve gives no inferred reasoning: `--model` already names a model.
fn matching_reasoning(config: &PoolConfig, band: Option<u8>, model: &str) -> Option<String> {
    let band = band?;
    config
        .bands
        .entries
        .get(&format!("P{band}"))?
        .iter()
        .find(|entry| entry.model == model)
        .map(|entry| entry.reasoning.clone())
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

fn bad_role_band(config: &PoolConfig, role: &str, band: &str) -> VivariumError {
    let models = config
        .harness
        .as_ref()
        .map_or_else(|| String::from("none"), |harness| harness.models.join(", "));
    VivariumError::Config(format!(
        "role {role} has band \"{band}\": use a band P0 to P5, optionally with a -S0 to -S3 speed, \
         or a model in harness.models ({models})"
    ))
}

fn no_model(role: Option<&RoleFacts>) -> VivariumError {
    match role {
        Some(role) => VivariumError::Config(format!(
            "no model: pass --model, give the task a `band` key in its front matter, \
             or give role {} a band (`vivi role set {} --model P2`)",
            role.name, role.name
        )),
        None => VivariumError::Config(
            "no model: pass --model, give the task a `band` key in its front matter, \
             or give the role a band (`vivi role set <name> --model P2`)"
                .to_string(),
        ),
    }
}

/// The worker's standing prompt: the role's charter above the pool rules
/// that bound it, with a note on how the slot reaches the mailspace, then the
/// project-rule snapshot when it is not blank. A blank charter gives the pool
/// rules followed by the project rules.
#[must_use]
pub fn standing_prompt(
    role: &str,
    charter: &str,
    pool_rules: &str,
    project_rules: &str,
    bridge: bool,
) -> String {
    if charter.trim().is_empty() {
        return with_project_rules(pool_rules, project_rules);
    }
    let note = if bridge {
        "You run in a pool slot with the vivi tools of the project server \
         (MCP server `vivi`). Your task handle is in your brief: read it \
         with `show` and `thread`, report with `reply`; the runner records \
         your result and the pump or Mind settles the handle."
    } else {
        "You run in a pool slot with no `vivi` command and no mailspace. \
         Where the charter tells you to read or settle your handle with \
         `vivi`, read your brief instead and end with your receipt on \
         standard output; the runner records your result and the Mind \
         settles the handle."
    };
    let prompt = format!(
        "# Standing charter (role {role})\n\n{charter}\n\n---\n\n{note}\n\n\
         # Pool rules (these bound the charter)\n\n{pool_rules}"
    );
    with_project_rules(&prompt, project_rules)
}

/// `base` followed by the `# Project rules` section, or `base` unchanged when
/// the project rules are blank.
#[must_use]
pub fn with_project_rules(base: &str, project_rules: &str) -> String {
    if project_rules.trim().is_empty() {
        return base.to_string();
    }
    format!("{base}\n\n# Project rules\n\n{}", project_rules.trim_end())
}

/// The most bytes of project rules a standing prompt carries.
pub const PROJECT_RULES_CAP: usize = 8 * 1024;

/// The project-rule snapshot as prompt text: each `(scope, subject, body)`
/// whose scope is `project` becomes a `##` section; local rules are left
/// out. Text past [`PROJECT_RULES_CAP`] is cut at a character boundary and
/// ends with a cut marker. No project rule gives an empty string.
#[must_use]
pub fn project_rules_text(rules: &[(&str, &str, &str)]) -> String {
    let mut text = String::new();
    for (_, subject, body) in rules.iter().filter(|(scope, _, _)| *scope == "project") {
        text.push_str(&format!("## {subject}\n\n{}\n\n", body.trim_end()));
    }
    if text.len() <= PROJECT_RULES_CAP {
        return text;
    }
    let mut end = PROJECT_RULES_CAP;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text.push_str("\n[project rules cut at 8192 bytes]\n");
    text
}

/// The active `project`-scope rules, read now as a dispatch's snapshot. A
/// rule that cannot be read is skipped with one warning line on stderr; an
/// unreadable rule list is still an error.
///
/// # Errors
/// Returns an error when the rules cannot be listed.
pub fn project_rules_snapshot(space: &Mailspace) -> Result<String, VivariumError> {
    let mut details = Vec::new();
    for rule in space.list_rules(None, false)? {
        if rule.scope != "project" {
            continue;
        }
        match space.show_rule(&rule.handle) {
            Ok(detail) => details.push(detail),
            Err(error) => eprintln!("warning: skipping project rule {}: {error}", rule.handle),
        }
    }
    let rows: Vec<(&str, &str, &str)> = details
        .iter()
        .map(|d| {
            (
                d.rule.scope.as_str(),
                d.rule.subject.as_str(),
                d.body.as_str(),
            )
        })
        .collect();
    Ok(project_rules_text(&rows))
}

/// The digest seat's window: what one digest run covers and the memo subject
/// it must file. The window starts at the pump's previous run, or one
/// `first_run_secs` back on the first run, and always ends at `now`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestBrief {
    /// The memo subject: `digest YYYY-MM-DD (<window>)`, fixed for the day.
    pub subject: String,
    /// The window length as a human label, from
    /// [`crate::mailspace::memos::window_label`].
    pub window_label: String,
    /// The earliest moment the digest covers.
    pub start: chrono::DateTime<chrono::Utc>,
    /// The moment the run was cut: the latest moment it covers.
    pub end: chrono::DateTime<chrono::Utc>,
}

/// Cut one digest window and name its memo. Pure: no I/O. The start is the
/// previous run, else `first_run_secs` before `now`, clamped to never lie
/// after `now` (a stale clock must not open a negative window).
#[must_use]
pub fn digest_brief(
    now: chrono::DateTime<chrono::Utc>,
    last_run: Option<chrono::DateTime<chrono::Utc>>,
    first_run_secs: u64,
) -> DigestBrief {
    let start = last_run
        .unwrap_or(now - chrono::Duration::seconds(first_run_secs as i64))
        .min(now);
    let window_label =
        crate::mailspace::memos::window_label((now - start).num_seconds().max(0) as u64);
    let subject = format!("digest {} ({})", now.format("%Y-%m-%d"), window_label);
    DigestBrief {
        subject,
        window_label,
        start,
        end: now,
    }
}

/// The digest seat's instructions: what to read in the window and the one
/// memo it files. Pure: no I/O, the seat does the reading.
#[must_use]
pub fn digest_run_text(brief: &DigestBrief, project_root: &std::path::Path, role: &str) -> String {
    let root = project_root.display();
    format!(
        "## Digest run\n\
         \n\
         Window: {} to {} ({}).\n\
         \n\
         Project root: {root}\n\
         \n\
         Run every `vivi` command below with `--project {root}`.\n\
         \n\
         Cover everything the window holds: every need, task and want \
         created, replied to or closed in it (`vivi board`, the `vivi need`, \
         `vivi task` and `vivi want` lists, and the shows of the items those \
         lists name), plus pump activity -- flow runs and their steps \
         (`vivi flow list`, `vivi flow show <kind> --need <need>`), the pump \
         records (`vivi pump list`, `vivi pump show`), pump exceptions, and \
         failed or timed-out jobs. Read `.vivi/pump/status.txt` and \
         `.vivi/pump/exceptions.txt`.\n\
         \n\
         When you see a pattern (repeated failures, a cluster of related \
         bugs, a step that keeps timing out), read the underlying records and add that \
         context to the digest.\n\
         \n\
         File exactly one memo, then stop:\n\
         \n\
         ```\n\
         vivi --project {root} memo save --for {role} --subject \"{}\" \
         --tag digest --body-file <path>\n\
         ```\n\
         \n\
         Files nothing else: no needs, tasks, wants or mail. The memo save \
         is your only write.\n",
        brief.start.to_rfc3339(),
        brief.end.to_rfc3339(),
        brief.window_label,
        brief.subject,
    )
}

#[cfg(test)]
#[path = "role_dispatch_test.rs"]
mod tests;

#[cfg(test)]
#[path = "role_dispatch_rules_test.rs"]
mod rules_tests;
