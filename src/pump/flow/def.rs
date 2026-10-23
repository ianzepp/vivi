//! Flow definition types: kinds, node classes, answer words, nodes and edges.

/// The line a role worker ends its reply with: `ANSWER: <word>`.
pub const ANSWER_LINE: &str = "ANSWER:";
/// A step task's body starts with `pump-step: <need> r<run> <node>`.
pub const STEP_MARKER: &str = "pump-step:";
/// A run sentinel's body starts with `pump-run-sentinel: <need> r<run>`.
pub const SENTINEL_MARKER: &str = "pump-run-sentinel:";
/// Pump attempt-context replies start with `pump-attempt:`.
pub const ATTEMPT_MARKER: &str = "pump-attempt:";

/// Verdict word: the task was withdrawn.
pub const VERDICT_WITHDRAWN: &str = "withdrawn";
/// Verdict word: the task was replaced.
pub const VERDICT_REPLACED: &str = "replaced";
/// Verdict word: the task was superseded.
pub const VERDICT_SUPERSEDED: &str = "superseded";

/// The word every step also answers when its job failed or gave no valid word.
pub const FAILED: &str = "failed";

/// Which shipped flow a run follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FlowKind {
    Build,
    Research,
    Release,
}

impl FlowKind {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "build" => Some(Self::Build),
            "research" => Some(Self::Research),
            "release" => Some(Self::Release),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Build => "build",
            Self::Research => "research",
            Self::Release => "release",
        }
    }
}

/// What a flow node is (its `:::class` in the Mermaid source).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum NodeClass {
    Researcher,
    Planner,
    Admitter,
    Slicer,
    Auditor,
    Judge,
    Cto,
    Merge,
    MergeMain,
    Fanout,
    RungNeed,
    RungMain,
    RungRelease,
    RungRaised,
    Join,
    Raisecheck,
    Milestone,
    Park,
    Lock,
    /// Terminal that starts a new run at the named entry.
    Restart(String),
    /// A plain decision rhombus: routing only, never a task.
    Decision,
    /// Terminal that ends the run.
    Done,
}

impl NodeClass {
    pub fn parse(text: &str) -> Option<Self> {
        if let Some(entry) = text.strip_prefix("restart-") {
            return (!entry.is_empty()).then(|| Self::Restart(entry.to_string()));
        }
        Some(match text {
            "researcher" => Self::Researcher,
            "planner" => Self::Planner,
            "admitter" => Self::Admitter,
            "slicer" => Self::Slicer,
            "auditor" => Self::Auditor,
            "judge" => Self::Judge,
            "cto" => Self::Cto,
            "merge" => Self::Merge,
            "merge-main" => Self::MergeMain,
            "fanout" => Self::Fanout,
            "rung-need" => Self::RungNeed,
            "rung-main" => Self::RungMain,
            "rung-release" => Self::RungRelease,
            "rung-raised" => Self::RungRaised,
            "join" => Self::Join,
            "raisecheck" => Self::Raisecheck,
            "milestone" => Self::Milestone,
            "park" => Self::Park,
            "lock" => Self::Lock,
            "decision" => Self::Decision,
            "done" => Self::Done,
            _ => return None,
        })
    }

    /// A step is a role, rung, merge or fanout node: it has a fixed answer
    /// word set and always also answers `failed`.
    pub fn is_step(&self) -> bool {
        matches!(
            self,
            Self::Researcher
                | Self::Planner
                | Self::Admitter
                | Self::Slicer
                | Self::Auditor
                | Self::Judge
                | Self::Cto
                | Self::Merge
                | Self::MergeMain
                | Self::Fanout
                | Self::RungNeed
                | Self::RungMain
                | Self::RungRelease
                | Self::RungRaised
        )
    }

    /// The answer words this class can give (plan section 3.5), `failed`
    /// included for every step. Milestone, park, lock, decisions and
    /// terminals have no fixed word set.
    pub fn words(&self) -> &'static [&'static str] {
        match self {
            Self::Researcher => &["READY", "NOT READY", "NEEDS FURTHER REVIEW", FAILED],
            Self::Planner => &["planned", "delivered", "operator", "hold", "design", FAILED],
            Self::Admitter => &["admitted", "revise", "escalate", FAILED],
            Self::Slicer => &["sliced", "escalate", FAILED],
            Self::Auditor => &[
                "clean_pass",
                "residual",
                "block_range",
                "block_ship",
                FAILED,
            ],
            Self::Judge => &["block", "residual", FAILED],
            Self::Cto => &["research", "plan", "slice", "repair", "lock", FAILED],
            Self::Merge | Self::MergeMain => &["merged", "conflict", FAILED],
            Self::Fanout => &["built", FAILED],
            Self::RungNeed | Self::RungMain | Self::RungRelease | Self::RungRaised => {
                &["pass", "fail", FAILED]
            }
            Self::Join => &["verified", "raised", "block", "judge", FAILED],
            Self::Raisecheck => &["raised", "none"],
            Self::Milestone
            | Self::Park
            | Self::Lock
            | Self::Restart(_)
            | Self::Decision
            | Self::Done => &[],
        }
    }
}

/// One node of a flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowNode {
    pub id: String,
    pub class: NodeClass,
    pub label: String,
}

/// One edge of a flow, taken on any of its answer words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowEdge {
    pub from: String,
    pub to: String,
    pub words: Vec<String>,
}

/// A loaded flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowDef {
    pub kind: FlowKind,
    pub nodes: Vec<FlowNode>,
    pub edges: Vec<FlowEdge>,
    /// Node ids a run may start at.
    pub entries: Vec<String>,
}

#[cfg(test)]
#[path = "def_test.rs"]
mod tests;
