//! The action menu a wake publishes: every implemented action, grouped by
//! the authority stage that opens it, with the envelope fields each kind
//! requires and accepts. The field matrix here mirrors the validator in
//! `actions::fields`; `prompt_test.rs` compares the two so they cannot
//! drift apart silently.
//!
//! A stage keeps every lower power, so a wake's menu is the union of its
//! own group and every group below it, lower stages first. `upgrade` and
//! `self-release` are never advertised: their stages exist so the config
//! shape is settled, and releases stay the operator's call in this build.

use std::fmt::Write as _;

use crate::mind::authority::Action;
use crate::mind::config::AuthorityStage;

/// One advertised action: its kind, the envelope fields it requires and
/// accepts, and how to use it.
pub(super) struct Row {
    pub action: Action,
    pub required: &'static [&'static str],
    pub optional: &'static [&'static str],
    pub help: &'static str,
}

/// One stage's slice of the menu.
pub(super) struct Group {
    pub stage: AuthorityStage,
    pub label: &'static str,
    pub rows: &'static [Row],
}

/// The envelope rules every action shares, ahead of the per-stage groups.
const HEADER: &str = "Every action names project, a watched root path. need and step are bare \
     handles such as 6c02e949; leave out every field the kind does not use, and omit an optional \
     field you are not using rather than sending it blank. Status reads records only; logs come \
     through read-log. A reply or withdrawal must carry note. Higher stages keep every lower \
     power.";

/// What an unlock entry means. The omitted entry restarts at the flow's
/// first entry; `rework1` continues the need's existing branch.
const UNLOCK_HELP: &str = "restart a locked need's run. entry omitted means plan for a build \
     flow and fix1 for a repair flow. For a need whose merged build slices were useful, choose \
     entry=\"rework1\" to continue the need branch; any other entry — research, slice — starts \
     fresh branch handling, so inspect the evidence first with read-need-thread and \
     read-task-thread. note records the ruling";

/// The filing loop: file, take the returned handle, rank it, dispatch it.
const FILING_HELP: &str = "file a new need on a project: to, subject, body, done_when clauses \
     and target; write_scope bounds the paths the need may write. The filing answers a returned \
     handle: rank the handle with set-priority, then send it to the pump with dispatch-need. \
     Filing itself names no need — the returned handle is the need";

/// Reads and operator letters: the observe stage.
const OBSERVE_ROWS: &[Row] = &[
    Row {
        action: Action::ReadStatus,
        required: &[],
        optional: &["need", "status", "limit"],
        help: "pump records for a project: status lines, health, open steps. Records only — \
               the log is read-log's job. status is open|done|all; limit is 1-100",
    },
    Row {
        action: Action::ReadLog,
        required: &["file"],
        optional: &["lines", "filter"],
        help: "tail a project log. file is log|exceptions; lines is 1-500; filter narrows by \
               text",
    },
    Row {
        action: Action::ReadNeed,
        required: &["need"],
        optional: &[],
        help: "one need's record and state",
    },
    Row {
        action: Action::ReadNeedThread,
        required: &["need"],
        optional: &["limit", "before"],
        help: "a need's mail thread. before is the cursor from the last page",
    },
    Row {
        action: Action::ReadTaskThread,
        required: &["step"],
        optional: &["limit", "before"],
        help: "one task's mail thread and attempt evidence. before is the cursor from the last \
               page",
    },
    Row {
        action: Action::ReadFlow,
        required: &["flow"],
        optional: &["need"],
        help: "a work-graph flow. flow is build|research|release|repair; need narrows it to one \
               need",
    },
    Row {
        action: Action::ReadConfig,
        required: &[],
        optional: &[],
        help: "the project's mailspace configuration",
    },
    Row {
        action: Action::ReadPool,
        required: &["pool"],
        optional: &[],
        help: "one pool's state",
    },
    Row {
        action: Action::LetterToOperator,
        required: &[],
        optional: &["need", "note"],
        help: "a letter to the operator. note is the body; need links the letter to a need",
    },
    Row {
        action: Action::Escalate,
        required: &[],
        optional: &["need", "note"],
        help: "a letter the operator must decide on; use it when only the operator can decide. \
               note is the body; need links the letter to a need",
    },
];

/// Pump intervention and model/pool tuning: the unstick stage.
const UNSTICK_ROWS: &[Row] = &[
    Row {
        action: Action::PausePump,
        required: &[],
        optional: &[],
        help: "stop one project's pump. Also the guarded-tuning bracket: pause before \
               set-role-model, set-pool-cap or set-pool-band when the running pump must not \
               race the change, and resume-pump after",
    },
    Row {
        action: Action::ResumePump,
        required: &[],
        optional: &[],
        help: "restart a pump this mind paused",
    },
    Row {
        action: Action::Requeue,
        required: &["need", "step"],
        optional: &["note"],
        help: "send a step back to its pool. note says why",
    },
    Row {
        action: Action::Replan,
        required: &["need"],
        optional: &[],
        help: "ask for a replan of one need",
    },
    Row {
        action: Action::Answer,
        required: &["need", "step", "word"],
        optional: &["note"],
        help: "answer an operator question on one step. word is the ruling; note carries the \
               reasoning",
    },
    Row {
        action: Action::Unlock,
        required: &["need"],
        optional: &["entry", "note"],
        help: UNLOCK_HELP,
    },
    Row {
        action: Action::ReplyNeed,
        required: &["need", "note"],
        optional: &[],
        help: "post a reply letter on a need's thread; note is the reply. It supplies evidence \
               or a ruling but does not release a lock — releasing a lock is unlock's job",
    },
    Row {
        action: Action::SetPriority,
        required: &["need", "priority"],
        optional: &[],
        help: "set a need's dispatch priority; higher is more urgent",
    },
    Row {
        action: Action::WithdrawNeed,
        required: &["need", "note"],
        optional: &[],
        help: "take a need off the backlog. note says why",
    },
    Row {
        action: Action::SetRoleModel,
        required: &["role", "model"],
        optional: &[],
        help: "retune one role's model. Project-level tuning: it must not carry a need",
    },
    Row {
        action: Action::SetPoolCap,
        required: &["pool", "model", "cap"],
        optional: &[],
        help: "retune a pool's slot cap for one model. Project-level tuning: it must not carry \
               a need",
    },
    Row {
        action: Action::SetPoolBand,
        required: &["pool", "band", "model", "reasoning"],
        optional: &[],
        help: "retune a pool's band: band is P0-P5 and selects only a model the pool already \
               offers. Project-level tuning: it must not carry a need",
    },
];

/// Filing needs and dispatching them: the file-fixes stage.
const FILE_FIXES_ROWS: &[Row] = &[
    Row {
        action: Action::FileNeed,
        required: &["to", "subject", "body", "done_when", "target"],
        optional: &["write_scope"],
        help: FILING_HELP,
    },
    Row {
        action: Action::FileFixNeed,
        required: &["to", "subject", "body", "done_when", "target"],
        optional: &["write_scope"],
        help: "file a fix need at the vivi project itself: target must be vivi. Same fields as \
               file-need and the same returned-handle -> set-priority -> dispatch-need path",
    },
    Row {
        action: Action::DispatchNeed,
        required: &["need"],
        optional: &["note"],
        help: "send a filed need to the pump. note may say why now",
    },
];

/// The menu groups, lowest stage first; `for_stage` renders every group a
/// stage opens.
pub(super) const GROUPS: [Group; 3] = [
    Group {
        stage: AuthorityStage::Observe,
        label: "read the projects and write the operator",
        rows: OBSERVE_ROWS,
    },
    Group {
        stage: AuthorityStage::Unstick,
        label: "intervene in a stuck project and tune it",
        rows: UNSTICK_ROWS,
    },
    Group {
        stage: AuthorityStage::FileFixes,
        label: "file needs and dispatch them",
        rows: FILE_FIXES_ROWS,
    },
];

/// The menu for `stage`: the shared envelope rules, then one group per open
/// stage with every action that stage admits, lower stages first.
#[must_use]
pub(super) fn for_stage(stage: AuthorityStage) -> String {
    let mut out = String::from(HEADER);
    for group in GROUPS.iter().filter(|group| group.stage <= stage) {
        let _ignored = writeln!(out, "\nStage {} — {}:", group.stage.as_str(), group.label);
        for row in group.rows {
            write_row(&mut out, row);
        }
    }
    out
}

/// One rendered row: `- kind: help. required: a, b. optional: c, d.`
fn write_row(out: &mut String, row: &Row) {
    let _ignored = write!(out, "- {}: {}.", row.action.as_str(), row.help);
    if !row.required.is_empty() {
        let _ignored = write!(out, " required: {}.", row.required.join(", "));
    }
    if !row.optional.is_empty() {
        let _ignored = write!(out, " optional: {}.", row.optional.join(", "));
    }
    out.push('\n');
}
