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
use super::mcp_actuator::ACTION_RESULT_ALLOWANCE;
use super::prompt::clipped;
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

/// What one turn's envelope produced: the planned actions, and, when the
/// `+++` block could not be parsed at all, the bounded failure text. An
/// empty plan with no failure is an empty but valid envelope.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnvelopeOutcome {
    pub planned: Vec<Planned>,
    /// Why the envelope could not be read, when it could not be.
    pub parse_failure: Option<String>,
}

impl EnvelopeOutcome {
    /// The outcome for a reply whose envelope could not be parsed. The
    /// failure is kept within the action-report allowance and its fence
    /// markers defused, so a hostile reply can neither bloat the outcome
    /// nor break out of the report's fencing.
    #[must_use]
    pub fn parse_failed(error: &str) -> Self {
        Self {
            planned: Vec::new(),
            parse_failure: Some(clipped(error, ACTION_RESULT_ALLOWANCE)),
        }
    }

    /// Whether there is nothing to tell the next turn about: no plans and
    /// no parse failure.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.planned.is_empty() && self.parse_failure.is_none()
    }
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
    /// control. The unlock control carries `--entry` and its value when the
    /// request names the entry to restart at; the flow's default first
    /// entry otherwise.
    #[must_use]
    pub fn argv(&self) -> Option<Vec<String>> {
        let project = self.project.as_deref()?;
        let mut argv: Vec<String> = match self.kind {
            Action::PausePump => words(&["pump", "pause"]),
            Action::ResumePump => words(&["pump", "resume"]),
            Action::Requeue => with(&["pump", "requeue"], &[self.step.as_deref()?]),
            Action::Replan => with(&["pump", "replan"], &[self.need.as_deref()?]),
            Action::Unlock => {
                let mut parts = with(&["pump", "unlock"], &[self.need.as_deref()?]);
                if let Some(entry) = self.entry.as_deref() {
                    parts.push(String::from("--entry"));
                    parts.push(entry.to_string());
                }
                parts
            }
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
        let line = describe(request);
        self.applied.push(line.clone());
        Ok(line)
    }
}

/// The intended MCP operation one permitted request maps to, as a dry run
/// records it: the server tool and the arguments that matter, with the
/// project and note context. A pure description — nothing here calls a
/// tool or writes to a project — and the not-yet-wired filing, dispatch
/// and release kinds say so rather than imply a run.
fn describe(request: &ActionRequest) -> String {
    let mut line = describe_read(request)
        .or_else(|| describe_view(request))
        .or_else(|| describe_letter(request))
        .or_else(|| describe_control(request))
        .or_else(|| describe_move(request))
        .or_else(|| describe_tuning(request))
        .unwrap_or_default();
    let project = request
        .project
        .as_deref()
        .map_or_else(|| String::from("-"), |path| path.display().to_string());
    line.push_str(" project=");
    line.push_str(&project);
    if let Some(note) = request.note.as_deref() {
        line.push_str(&format!(" note={note:?}"));
    }
    line
}

/// The pump's own records: the records-only status read and the log read.
fn describe_read(request: &ActionRequest) -> Option<String> {
    match request.kind {
        Action::ReadStatus => Some(op(
            "would read pump records via pump_list",
            &[
                ("need", request.need.as_deref()),
                ("status", request.status.as_deref()),
                ("limit", number(request.limit).as_deref()),
            ],
        )),
        Action::ReadLog => Some(op(
            "would read the pump log via pump_log",
            &[
                ("file", request.file.as_deref()),
                ("lines", number(request.lines).as_deref()),
                ("filter", request.filter.as_deref()),
            ],
        )),
        _ => None,
    }
}

/// The project-state views: need and task diagnostics and the flow,
/// config and pool reads.
fn describe_view(request: &ActionRequest) -> Option<String> {
    match request.kind {
        Action::ReadNeed => Some(op(
            "would show one need via need_show",
            &[("need", request.need.as_deref())],
        )),
        Action::ReadNeedThread => Some(op(
            "would read a need's thread via need_thread",
            &[
                ("need", request.need.as_deref()),
                ("limit", number(request.limit).as_deref()),
                ("before", request.before.as_deref()),
            ],
        )),
        Action::ReadTaskThread => Some(op(
            "would read a task's thread via task_thread",
            &[
                ("step", request.step.as_deref()),
                ("limit", number(request.limit).as_deref()),
                ("before", request.before.as_deref()),
            ],
        )),
        Action::ReadFlow => Some(op(
            "would show a flow via flow_show",
            &[
                ("flow", request.flow.as_deref()),
                ("need", request.need.as_deref()),
            ],
        )),
        Action::ReadConfig => Some(op(
            "would read the mailspace configuration via config_show",
            &[],
        )),
        Action::ReadPool => Some(op(
            "would show one pool via pool_show",
            &[("pool", request.pool.as_deref())],
        )),
        _ => None,
    }
}

/// The letters and thread replies: `send` to the operator, whose subject
/// names the need when the letter carries one, and `reply` on a need's
/// thread, whose body is the note.
fn describe_letter(request: &ActionRequest) -> Option<String> {
    match request.kind {
        Action::LetterToOperator => Some(op(
            "would send the operator a letter via send",
            &[("need", request.need.as_deref())],
        )),
        Action::Escalate => Some(op(
            "would send the operator an escalation via send",
            &[("need", request.need.as_deref())],
        )),
        Action::ReplyNeed => Some(op(
            "would reply on a need's thread via reply",
            &[("need", request.need.as_deref())],
        )),
        _ => None,
    }
}

/// The pump controls and actions: `pump_control` for pause and resume,
/// `pump_action` for the moves one need or step takes.
fn describe_control(request: &ActionRequest) -> Option<String> {
    match request.kind {
        Action::PausePump => Some(op("would pause the pump via pump_control", &[])),
        Action::ResumePump => Some(op("would resume the pump via pump_control", &[])),
        Action::Requeue => Some(op(
            "would requeue a step via pump_action",
            &[("step", request.step.as_deref())],
        )),
        Action::Replan => Some(op(
            "would ask for a replan via pump_action",
            &[("need", request.need.as_deref())],
        )),
        Action::Answer => Some(op(
            "would answer an operator question via pump_action",
            &[
                ("step", request.step.as_deref()),
                ("word", request.word.as_deref()),
            ],
        )),
        Action::Unlock => Some(op(
            "would unlock a stuck need via pump_action",
            &[
                ("need", request.need.as_deref()),
                ("entry", request.entry.as_deref()),
            ],
        )),
        Action::WithdrawNeed => Some(op(
            "would withdraw a need via pump_action",
            &[("need", request.need.as_deref())],
        )),
        _ => None,
    }
}

/// Priority, dispatch and filing. Filing, dispatch and the release stubs
/// are not wired to the server's tools yet: the description says so
/// instead of implying a call would run.
fn describe_move(request: &ActionRequest) -> Option<String> {
    match request.kind {
        Action::SetPriority => Some(op(
            "would set a need's priority via pump_priority action=set",
            &[
                ("need", request.need.as_deref()),
                ("level", number(request.priority).as_deref()),
            ],
        )),
        Action::DispatchNeed => Some(op(
            "would dispatch a need to the pump (dispatch is not wired to the server yet)",
            &[("need", request.need.as_deref())],
        )),
        Action::FileNeed => Some(op(
            "would file a new need (filing is not wired to the server yet)",
            &[],
        )),
        Action::FileFixNeed => Some(op(
            "would file a fix need (filing is not wired to the server yet)",
            &[],
        )),
        Action::Upgrade => Some(op(
            "would take the operator's release (upgrade is not wired to the server yet)",
            &[],
        )),
        Action::SelfRelease => Some(op(
            "would cut a release (self-release is not wired to the server yet)",
            &[],
        )),
        _ => None,
    }
}

/// The typed role/pool tuning: `role_set` for a role's model, the narrow
/// per-model `config_set` key for a pool cap, and one guarded
/// `pool_bands action=set` replacing a band's whole choice entry.
fn describe_tuning(request: &ActionRequest) -> Option<String> {
    match request.kind {
        Action::SetRoleModel => Some(op(
            "would retune a role's model via role_set",
            &[
                ("role", request.role.as_deref()),
                ("model", request.model.as_deref()),
            ],
        )),
        Action::SetPoolCap => Some(op(
            "would set a pool's per-model cap via config_set",
            &[
                ("pool", request.pool.as_deref()),
                ("model", request.model.as_deref()),
                ("cap", number(request.cap).as_deref()),
            ],
        )),
        Action::SetPoolBand => Some(op(
            "would set a pool's model band via pool_bands action=set",
            &[
                ("pool", request.pool.as_deref()),
                ("band", request.band.as_deref()),
                ("model", request.model.as_deref()),
                ("reasoning", request.reasoning.as_deref()),
            ],
        )),
        _ => None,
    }
}

/// One description line: the operation head, then ` key=value` for each
/// argument the request carries. Absent arguments stay out, the way the
/// real call leaves unset arguments out.
fn op(head: &str, fields: &[(&str, Option<&str>)]) -> String {
    let mut line = String::from(head);
    for &(key, value) in fields {
        if let Some(value) = value {
            line.push_str(&format!(" {key}={value}"));
        }
    }
    line
}

/// A numeric argument as description text, absent when the request has none.
fn number(value: Option<i64>) -> Option<String> {
    value.map(|value| value.to_string())
}

#[cfg(test)]
#[path = "actions_test.rs"]
mod tests;
