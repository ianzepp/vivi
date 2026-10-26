//! The action envelope a mind turn may end with, and the plan built from it.
//!
//! The model never runs commands itself. A turn ends with a TOML envelope
//! between `+++` lines, the same shape cadence head roles use:
//!
//! ```text
//! +++
//! [[action]]
//! kind = "requeue"
//! project = "/work/app"
//! need = "3f9a1c2e"
//! step = "b71d04aa"
//! note = "gate flaked on a network timeout; first requeue"
//! +++
//! ```
//!
//! [`plan`] checks each request against the watched roots and the authority
//! gate, and [`ActionRequest::argv`] renders the existing `vivi` command the
//! request maps to. Nothing in the envelope is trusted: fields are length
//! capped, handles must be plain hex-ish tokens, projects must be watched
//! roots, and every refusal is reported back to the turn and the operator.
//! Permitted requests are carried out by an [`Actuator`]: [`DryRun`] records
//! what would run and runs nothing, and the MCP actuator turns permitted
//! actions into server tool calls.

mod fields;
mod filing_fields;

use std::path::PathBuf;

use serde::Deserialize;

use super::authority::{self, Action, BudgetClass, Denial, Gate};
use fields::{drop_blank_fields, validate_fields};

/// Longest free-text note accepted from a turn.
pub const MAX_NOTE: usize = 500;
/// Most actions honoured from one envelope.
pub const MAX_ACTIONS: usize = 8;

/// One requested action, as the turn wrote it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionRequest {
    pub kind: Action,
    #[serde(default)]
    pub project: Option<PathBuf>,
    #[serde(default)]
    pub need: Option<String>,
    #[serde(default)]
    pub step: Option<String>,
    #[serde(default)]
    pub word: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    /// Dispatch entry to restart a flow at.
    #[serde(default)]
    pub entry: Option<String>,
    /// Status filter for reads.
    #[serde(default)]
    pub status: Option<String>,
    /// Most records a read may return.
    #[serde(default)]
    pub limit: Option<i64>,
    /// Which file a read names (the log, say).
    #[serde(default)]
    pub file: Option<String>,
    /// How many lines of a file to read.
    #[serde(default)]
    pub lines: Option<i64>,
    /// Free-text filter for reads.
    #[serde(default)]
    pub filter: Option<String>,
    /// The flow a read or action names.
    #[serde(default)]
    pub flow: Option<String>,
    /// The pool a read or action names.
    #[serde(default)]
    pub pool: Option<String>,
    /// Read cursor: records before this one.
    #[serde(default)]
    pub before: Option<String>,
    /// Priority to file or set.
    #[serde(default)]
    pub priority: Option<i64>,
    /// The role a set-role action names.
    #[serde(default)]
    pub role: Option<String>,
    /// The model a set-role action names.
    #[serde(default)]
    pub model: Option<String>,
    /// The pool cap to set.
    #[serde(default)]
    pub cap: Option<i64>,
    /// The compute band to set.
    #[serde(default)]
    pub band: Option<String>,
    /// The reasoning effort to set.
    #[serde(default)]
    pub reasoning: Option<String>,
    /// Who a letter goes to.
    #[serde(default)]
    pub to: Option<String>,
    /// The letter's subject.
    #[serde(default)]
    pub subject: Option<String>,
    /// The letter's body.
    #[serde(default)]
    pub body: Option<String>,
    /// Done-when clauses for a filed need.
    #[serde(default)]
    pub done_when: Vec<String>,
    /// The file or path an action targets.
    #[serde(default)]
    pub target: Option<String>,
    /// Paths an action may write.
    #[serde(default)]
    pub write_scope: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    #[serde(default, rename = "action")]
    actions: Vec<ActionRequest>,
}

/// Why a request was refused before it reached the gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    Gate(Denial),
    UnwatchedProject,
    MissingField(&'static str),
    BadHandle(&'static str),
    NoteTooLong,
    TooMany,
    /// A field's value is malformed for what it names (a negative count,
    /// say). Typed so the turn can fix the one field.
    InvalidField(&'static str),
    /// A field this kind of action never reads.
    UnexpectedField(&'static str),
    /// A filing field exceeds what may be filed.
    FilingTooLarge(&'static str),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Gate(denial) => write!(f, "{denial}"),
            Self::UnwatchedProject => write!(f, "project is not a watched root"),
            Self::MissingField(field) => write!(f, "missing {field}"),
            Self::BadHandle(field) => write!(f, "{field} is not a plain handle"),
            Self::NoteTooLong => write!(f, "note longer than {MAX_NOTE} characters"),
            Self::TooMany => write!(f, "more than {MAX_ACTIONS} actions in one envelope"),
            Self::InvalidField(field) => write!(f, "{field} has an invalid value"),
            Self::UnexpectedField(field) => write!(f, "{field} does not apply to this action"),
            Self::FilingTooLarge(field) => write!(f, "{field} is too large to file"),
        }
    }
}

/// One request with its verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planned {
    pub request: ActionRequest,
    pub verdict: Result<(), Refusal>,
    /// What the actuator answered, once a permitted action has run: its
    /// receipt, or the failure text.
    pub executed: Option<Result<String, String>>,
}

/// A `+++` fence line. A live model opened its envelope with `+++ TOML`
/// (markdown habit), so a `toml` tag after the marker is accepted too.
fn is_fence(line: &str) -> bool {
    line.trim()
        .strip_prefix("+++")
        .is_some_and(|tag| tag.trim().is_empty() || tag.trim().eq_ignore_ascii_case("toml"))
}

/// Pull the last `+++` envelope out of a turn's reply. No envelope is an
/// empty list; a malformed one is an error the turn is told about.
///
/// # Errors
/// Returns the TOML parse error text.
pub fn parse_envelope(reply: &str) -> Result<Vec<ActionRequest>, String> {
    let lines: Vec<&str> = reply.lines().collect();
    let fences: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| is_fence(line))
        .map(|(index, _)| index)
        .collect();
    let [.., open, close] = fences.as_slice() else {
        return Ok(Vec::new());
    };
    let body = lines[open + 1..*close].join("\n");
    toml::from_str::<Envelope>(&body)
        .map(|envelope| {
            envelope
                .actions
                .into_iter()
                .map(drop_blank_fields)
                .collect()
        })
        .map_err(|error| error.to_string())
}

/// Check every request. Permitted actions spend their budget as they pass,
/// so two requeues of one need — or two filings to one project — in one
/// envelope count twice.
#[must_use]
pub fn plan(
    requests: Vec<ActionRequest>,
    gate: &Gate<'_>,
    watched: &[PathBuf],
    ledger: &mut authority::Ledger,
) -> Vec<Planned> {
    let mut out = Vec::new();
    for (index, request) in requests.into_iter().enumerate() {
        let verdict = if index >= MAX_ACTIONS {
            Err(Refusal::TooMany)
        } else {
            check(&request, gate, watched, ledger)
        };
        if verdict.is_ok() {
            charge(&request, gate, ledger);
        }
        out.push(Planned {
            request,
            verdict,
            executed: None,
        });
    }
    out
}

/// Charge one permitted request to its budget bucket before actuation: the
/// bare need handle for need-scoped moves, the shared project window for
/// filing and tuning. A server-side refusal later on does not refund it.
fn charge(request: &ActionRequest, gate: &Gate<'_>, ledger: &mut authority::Ledger) {
    match request.kind.budget() {
        BudgetClass::Unbudgeted => {}
        BudgetClass::PerNeed => {
            if let Some(need) = request.need.as_deref() {
                authority::spend(ledger, need, gate.now, gate.budgets.per_need_window_secs);
            }
        }
        BudgetClass::PerProject => {
            if let Some(project) = request.project.as_deref() {
                let key = authority::project_key(project);
                authority::spend(ledger, &key, gate.now, gate.budgets.per_need_window_secs);
            }
        }
    }
}

/// Check one request: the per-kind field matrix, then the project rule —
/// every action names a watched root, reads and letters included — then the
/// authority gate: the stage and pause gates plus the applicable budget
/// bucket.
fn check(
    request: &ActionRequest,
    gate: &Gate<'_>,
    watched: &[PathBuf],
    ledger: &authority::Ledger,
) -> Result<(), Refusal> {
    validate_fields(request)?;
    let project = request
        .project
        .as_deref()
        .ok_or(Refusal::MissingField("project"))?;
    if !watched.iter().any(|root| root == project) {
        return Err(Refusal::UnwatchedProject);
    }
    if request.kind == Action::FileFixNeed {
        filing_fields::check_fix_policy(request, project)?;
    }
    authority::permit(gate, ledger, request.kind, request.need.as_deref())
        .map_err(Refusal::Gate)?;
    if request.kind.budget() == BudgetClass::PerProject {
        authority::permit_project(gate, ledger, project).map_err(Refusal::Gate)?;
    }
    Ok(())
}

impl ActionRequest {
    /// The existing `vivi` command line this request maps to, without the
    /// binary name. `None` for letters and stubs, which do not map to a pump
    /// control.
    #[must_use]
    pub fn argv(&self) -> Option<Vec<String>> {
        let project = self.project.as_deref()?;
        let mut argv: Vec<String> = match self.kind {
            Action::PausePump => words(&["pump", "pause"]),
            Action::ResumePump => words(&["pump", "resume"]),
            Action::Requeue => with(&["pump", "requeue"], &[self.step.as_deref()?]),
            Action::Replan => with(&["pump", "replan"], &[self.need.as_deref()?]),
            Action::Unlock => with(&["pump", "unlock"], &[self.need.as_deref()?]),
            Action::Answer => with(
                &["pump", "answer"],
                &[self.step.as_deref()?, self.word.as_deref()?],
            ),
            _ => return None,
        };
        argv.push(String::from("--project"));
        argv.push(project.to_string_lossy().into_owned());
        Some(argv)
    }
}

fn words(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_string()).collect()
}

fn with(parts: &[&str], tail: &[&str]) -> Vec<String> {
    let mut out = words(parts);
    out.extend(tail.iter().map(|part| (*part).to_string()));
    out
}

/// Carries out permitted actions. The spike ships only [`DryRun`].
pub trait Actuator {
    /// Carry out one permitted request; returns a one-line receipt.
    ///
    /// # Errors
    /// Returns the failure text.
    fn apply(&mut self, request: &ActionRequest) -> Result<String, String>;

    /// Whether receipts are real answers the session should see on a
    /// follow-up turn. A dry run's are not.
    fn feeds_back(&self) -> bool {
        false
    }
}

/// Records what would run and runs nothing.
#[derive(Debug, Default)]
pub struct DryRun {
    pub applied: Vec<String>,
}

impl Actuator for DryRun {
    fn apply(&mut self, request: &ActionRequest) -> Result<String, String> {
        let line = request.argv().map_or_else(
            || {
                format!(
                    "would send {:?} (letter or stub) project={} need={} note={:?}",
                    request.kind,
                    request
                        .project
                        .as_deref()
                        .map_or_else(|| String::from("-"), |path| path.display().to_string()),
                    request.need.as_deref().unwrap_or("-"),
                    request.note.as_deref().unwrap_or_default(),
                )
            },
            |argv| format!("would run vivi {}", argv.join(" ")),
        );
        self.applied.push(line.clone());
        Ok(line)
    }
}

#[cfg(test)]
#[path = "actions_test.rs"]
mod tests;
