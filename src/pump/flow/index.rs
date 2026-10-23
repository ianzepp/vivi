//! Row types of the flow run index (stored beside the v8 tables).

/// Where a step is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepState {
    Pending,
    Running,
    Waiting,
    Done,
    Failed,
}

/// One flow run on a need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRow {
    pub need: String,
    pub run: u32,
    pub flow: String,
    pub flow_version: u32,
    pub kind: String,
    pub entry: String,
    pub state: String,
}

/// One step of a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepRow {
    pub run: u32,
    pub node: String,
    pub task: String,
    pub state: StepState,
    pub verdict: Option<String>,
}

/// One job attempt for a step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepJobRow {
    pub job: String,
    pub run: u32,
    pub node: String,
    pub task: String,
    pub attempt: u32,
}
