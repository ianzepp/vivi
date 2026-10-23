//! Flow definition types: kinds, node classes, answer words, nodes and edges.

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
    Repair,
}

impl FlowKind {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "build" => Some(Self::Build),
            "research" => Some(Self::Research),
            "release" => Some(Self::Release),
            "repair" => Some(Self::Repair),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Build => "build",
            Self::Research => "research",
            Self::Release => "release",
            Self::Repair => "repair",
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
    Repairer,
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
            "repairer" => Self::Repairer,
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

    /// The class word as written in the Mermaid source: the inverse of
    /// [`NodeClass::parse`].
    pub fn name(&self) -> String {
        match self {
            Self::Researcher => "researcher".to_string(),
            Self::Planner => "planner".to_string(),
            Self::Admitter => "admitter".to_string(),
            Self::Slicer => "slicer".to_string(),
            Self::Auditor => "auditor".to_string(),
            Self::Judge => "judge".to_string(),
            Self::Cto => "cto".to_string(),
            Self::Merge => "merge".to_string(),
            Self::MergeMain => "merge-main".to_string(),
            Self::Fanout => "fanout".to_string(),
            Self::Repairer => "repairer".to_string(),
            Self::RungNeed => "rung-need".to_string(),
            Self::RungMain => "rung-main".to_string(),
            Self::RungRelease => "rung-release".to_string(),
            Self::RungRaised => "rung-raised".to_string(),
            Self::Join => "join".to_string(),
            Self::Raisecheck => "raisecheck".to_string(),
            Self::Milestone => "milestone".to_string(),
            Self::Park => "park".to_string(),
            Self::Lock => "lock".to_string(),
            Self::Restart(entry) => format!("restart-{entry}"),
            Self::Decision => "decision".to_string(),
            Self::Done => "done".to_string(),
        }
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
                | Self::Repairer
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
            Self::Cto => &["research", "plan", "slice", "rework", "lock", FAILED],
            Self::Merge | Self::MergeMain => &["merged", "conflict", FAILED],
            Self::Fanout => &["built", "unchanged", FAILED],
            Self::Repairer => &["built", "overtime", "unchanged", FAILED],
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
