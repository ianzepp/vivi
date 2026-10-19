use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptRecord {
    pub need: String,
    pub task: Option<String>,
    pub attempt: String,
    pub actor: Option<String>,
    pub role: Option<String>,
    pub pool: Option<String>,
    pub slot: Option<String>,
    pub model: Option<String>,
    pub kind: Option<String>,
    pub start: Option<u64>,
    pub end: Option<u64>,
    pub agent_seconds: Option<u64>,
    pub outcome: String,
    pub ready_at: Option<u64>,
    pub delivered_at: Option<u64>,
    pub merged_at: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NeedWindow {
    pub need: String,
    pub opened_at: Option<u64>,
    pub done_at: Option<u64>,
    pub done: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReportData {
    pub attempts: Vec<AttemptRecord>,
    pub needs: Vec<NeedWindow>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Records,
    Throughput,
    Durations,
    Needs,
    Failures,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dimension {
    Actor,
    Role,
    Model,
    Pool,
    Kind,
    Need,
    Day,
    Hour,
}

#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub since: Option<u64>,
    pub until: Option<u64>,
    pub by: Option<Dimension>,
    pub need: Option<String>,
    pub actor: Option<String>,
    pub model: Option<String>,
    pub top: Option<usize>,
}
