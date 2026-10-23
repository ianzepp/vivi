//! A step's outcome and the handlers that start each class of step.

use std::collections::BTreeMap;

use crate::error::VivariumError;
use crate::pump::flow::def::NodeClass;
use crate::pump::flow::index::StepRow;

/// What a finished step answered.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub need: String,
    pub run: u32,
    pub node: String,
    pub task: String,
    pub word: String,
    pub payload: serde_json::Value,
}

/// How a step begins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartAction {
    /// Enqueue a pool job with this brief.
    EnqueueJob { kind: String, brief: String },
    /// Open the step task and wait for someone to close it.
    OpenAndWait,
    /// Code decides at once with this word.
    Decide { word: String },
}

/// Starts one class of step.
pub trait StepHandler {
    fn start(&self, step: &StepRow) -> Result<StartAction, VivariumError>;
}

/// Handlers by node class; later needs register theirs here.
#[derive(Default)]
pub struct StepHandlers {
    handlers: BTreeMap<NodeClass, Box<dyn StepHandler>>,
}

impl StepHandlers {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, class: NodeClass, handler: Box<dyn StepHandler>) {
        self.handlers.insert(class, handler);
    }

    pub fn get(&self, class: &NodeClass) -> Option<&dyn StepHandler> {
        self.handlers.get(class).map(Box::as_ref)
    }
}
