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

/// The one run-state vocabulary of the flow index.
pub mod run_state {
    pub const LIVE: &str = "live";
    pub const STARTING: &str = "starting";
    pub const FINISHING: &str = "finishing";
    pub const RESTARTING: &str = "restarting";
    pub const SUPERSEDED: &str = "superseded";
    pub const WITHDRAWN: &str = "withdrawn";
    pub const DONE: &str = "done";

    pub const ALL: [&str; 7] = [
        LIVE, STARTING, FINISHING, RESTARTING, SUPERSEDED, WITHDRAWN, DONE,
    ];

    /// Whether `word` is a stored run state.
    pub fn is_known(word: &str) -> bool {
        ALL.contains(&word)
    }
}

/// Exception kind for a job whose identity does not match the live state.
pub const JOB_STALE: &str = "flow-job-stale";

impl RunRow {
    /// A step of this run (no verdict yet).
    pub fn step(&self, node: &str, task: &str, state: StepState) -> StepRow {
        StepRow {
            run: self.run,
            node: node.to_string(),
            task: task.to_string(),
            state,
            verdict: None,
        }
    }
}

impl StepRow {
    /// A job attempt row for this step.
    pub fn job(&self, job: &str, attempt: u32) -> StepJobRow {
        StepJobRow {
            job: job.to_string(),
            run: self.run,
            node: self.node.clone(),
            task: self.task.clone(),
            attempt,
        }
    }
}

impl StepJobRow {
    /// The next attempt of the same step (a rerun or requeue at a new attempt).
    pub fn next(&self, job: &str) -> StepJobRow {
        StepJobRow {
            job: job.to_string(),
            attempt: self.attempt + 1,
            ..self.clone()
        }
    }

    /// A job at the same attempt (a correction or an operator requeue).
    pub fn again(&self, job: &str) -> StepJobRow {
        StepJobRow {
            job: job.to_string(),
            ..self.clone()
        }
    }

    /// Write this row's identity into a job payload: the one writer of
    /// `need`, `run`, `node`, `task` and `attempt`. Other keys are kept.
    pub fn stamp(&self, need: &str, payload: &mut serde_json::Value) {
        if !payload.is_object() {
            *payload = serde_json::Value::Object(serde_json::Map::new());
        }
        if let Some(map) = payload.as_object_mut() {
            map.insert("need".into(), need.into());
            map.insert("run".into(), self.run.into());
            map.insert("node".into(), self.node.clone().into());
            map.insert("task".into(), self.task.clone().into());
            map.insert("attempt".into(), self.attempt.into());
        }
    }
}

/// The checked identity read back from a job payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayloadIdentity {
    pub need: String,
    pub run: u32,
    pub task: String,
    pub attempt: Option<u32>,
}

impl PayloadIdentity {
    /// Read the identity; a missing or wrong-typed `need`, `run` or `task`
    /// names the field.
    pub fn of(payload: &serde_json::Value) -> Result<Self, String> {
        let need = text_field(payload, "need")?;
        let task = text_field(payload, "task")?;
        let run = number_field(payload, "run")?.ok_or_else(|| field_error("run", payload))?;
        let attempt = number_field(payload, "attempt")?;
        Ok(Self {
            need,
            run,
            task,
            attempt,
        })
    }
}

fn field_error(name: &str, payload: &serde_json::Value) -> String {
    let kind = if name == "run" || name == "attempt" {
        "a number"
    } else {
        "a string"
    };
    let actual = payload
        .get(name)
        .map_or_else(|| "missing".to_string(), |v| v.to_string());
    format!("payload field `{name}`: expected {kind}; actual: {actual}")
}

fn text_field(payload: &serde_json::Value, name: &str) -> Result<String, String> {
    match payload.get(name).and_then(serde_json::Value::as_str) {
        Some(text) => Ok(text.to_string()),
        None => Err(field_error(name, payload)),
    }
}

/// `Ok(None)` when absent or null; an error when present but not a u32.
fn number_field(payload: &serde_json::Value, name: &str) -> Result<Option<u32>, String> {
    match payload.get(name) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => match value.as_u64().map(u32::try_from) {
            Some(Ok(n)) => Ok(Some(n)),
            Some(Err(_)) | None => Err(field_error(name, payload)),
        },
    }
}

#[cfg(test)]
#[path = "index_test.rs"]
mod tests;
