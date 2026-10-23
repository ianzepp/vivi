//! Routing of a finished step along the flow's edges.
//!
//! One answer word takes every edge whose word list contains it. Successor
//! step tasks are sent (blocked on the finished step) and bound together with
//! the run sentinel before the finished step is closed with its verdict, so
//! the need never looks complete in between.

use std::collections::BTreeSet;

use crate::error::VivariumError;
use crate::pump::flow::def::{FlowDef, FlowEdge, FlowNode, NodeClass, STEP_MARKER};
use crate::pump::flow::index::{StepRow, StepState};
use crate::pump::flow::mail::{MailReader, MailWriter};
use crate::pump::flow::outcome::Outcome;
use crate::pump::host::Ctx;
use crate::pump::store::PumpStore;
use crate::pump::store::step_payload::StepPayloadRow;

/// One finished input of a code decision (a join or a raise check).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JoinInput {
    pub node: String,
    pub task: String,
    pub word: String,
}

/// Supplies the answer word of a code decision node (`join`, `raisecheck`).
pub(crate) type Decide<'a> = &'a dyn Fn(&FlowNode, &[JoinInput]) -> Result<String, VivariumError>;

/// A step or open task that routing sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Sent {
    pub node: String,
    pub task: String,
}

/// A terminal node routing reached; the caller handles it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Terminal {
    pub node: String,
    pub class: NodeClass,
}

/// What routing did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Routed {
    /// Set when the word is outside the step's word set; nothing was changed.
    pub rejected: Option<String>,
    /// Tasks sent, in send order.
    pub sent: Vec<Sent>,
    /// Join nodes still waiting for another input.
    pub waiting: Vec<String>,
    /// Terminal targets reached. Not handled here.
    pub terminal: Vec<Terminal>,
}

impl Routed {
    fn is_empty(&self) -> bool {
        self.sent.is_empty() && self.waiting.is_empty() && self.terminal.is_empty()
    }
}

/// Route a finished step: take every matching edge, send and bind the
/// successors, then close the finished step with its word as verdict.
///
/// The run sentinel is the run's step row whose node is not a node of `def`.
///
/// # Errors
/// Returns a [`VivariumError`] when the node is unknown, no edge takes the
/// word, a code decision answers a word outside its set, or the mailspace or
/// the index fails.
pub(crate) fn route<M: MailWriter + MailReader>(
    ctx: &Ctx,
    store: &PumpStore,
    mail: &mut M,
    def: &FlowDef,
    outcome: &Outcome,
    decide: Decide<'_>,
) -> Result<Routed, VivariumError> {
    let node = find_node(def, &outcome.node)?;
    if !allows(&node.class, &outcome.word) {
        return Ok(Routed {
            rejected: Some(format!(
                "word `{}` is not an answer of node `{}`; allowed: {}",
                outcome.word,
                node.id,
                node.class.words().join(", ")
            )),
            ..Routed::default()
        });
    }
    // Recorded before expansion: a join fed by this step decides inside this
    // call and must already see the payload.
    if node.class.is_step() {
        store.record_step_payload(&StepPayloadRow {
            need: outcome.need.clone(),
            run: outcome.run,
            node: outcome.node.clone(),
            class: node.class.name(),
            task: outcome.task.clone(),
            payload: outcome.payload.clone(),
        })?;
    }
    let rows = store.steps_of(&outcome.need, outcome.run)?;
    let mut walk = Walk {
        ctx,
        store,
        mail,
        def,
        outcome,
        decide,
        rows,
        routed: Routed::default(),
        seen: BTreeSet::new(),
    };
    walk.expand(node, &outcome.word)?;
    if walk.routed.is_empty() {
        return Err(VivariumError::Parse(format!(
            "{} flow: node `{}` has no edge for word `{}`; {}",
            def.kind.name(),
            node.id,
            outcome.word,
            expected_labels(def, node)
        )));
    }
    walk.bind()?;
    walk.close_predecessor()?;
    Ok(walk.routed)
}

fn find_node<'d>(def: &'d FlowDef, id: &str) -> Result<&'d FlowNode, VivariumError> {
    def.nodes
        .iter()
        .find(|n| n.id == id)
        .ok_or_else(|| VivariumError::Parse(format!("{} flow: no node `{id}`", def.kind.name())))
}

/// The edge labels that leave `node`, following edges into decisions the
/// way `Walk::expand` does; `no outgoing edges` when there are none.
fn expected_labels(def: &FlowDef, node: &FlowNode) -> String {
    let mut labels: Vec<String> = Vec::new();
    let mut pending = vec![node.id.as_str()];
    let mut visited: BTreeSet<&str> = BTreeSet::new();
    while let Some(from) = pending.pop() {
        if !visited.insert(from) {
            continue;
        }
        for edge in def.edges.iter().filter(|e| e.from == from) {
            if edge.words.is_empty() {
                labels.push("(any word)".to_string());
            } else {
                labels.extend(edge.words.iter().cloned());
            }
            let feeds_decision = def
                .nodes
                .iter()
                .any(|n| n.id == edge.to && n.class == NodeClass::Decision);
            if feeds_decision {
                pending.push(edge.to.as_str());
            }
        }
    }
    labels.sort();
    labels.dedup();
    if labels.is_empty() {
        "no outgoing edges".to_string()
    } else {
        format!("expected one of: {}", labels.join(", "))
    }
}

/// A class with no fixed word set takes any word.
fn allows(class: &NodeClass, word: &str) -> bool {
    let words = class.words();
    words.is_empty() || words.contains(&word)
}

struct Walk<'a, M> {
    ctx: &'a Ctx,
    store: &'a PumpStore,
    mail: &'a mut M,
    def: &'a FlowDef,
    outcome: &'a Outcome,
    decide: Decide<'a>,
    rows: Vec<StepRow>,
    routed: Routed,
    seen: BTreeSet<String>,
}

impl<M: MailWriter + MailReader> Walk<'_, M> {
    /// Follow every edge out of `from` that takes `word` (an unlabelled edge
    /// takes any word).
    fn expand(&mut self, from: &FlowNode, word: &str) -> Result<(), VivariumError> {
        let def = self.def;
        let edges: Vec<&FlowEdge> = def
            .edges
            .iter()
            .filter(|e| {
                e.from == from.id && (e.words.is_empty() || e.words.iter().any(|w| w == word))
            })
            .collect();
        for edge in edges {
            let target = find_node(def, &edge.to)?;
            self.arrive(target, word)?;
        }
        Ok(())
    }

    fn arrive(&mut self, node: &FlowNode, word: &str) -> Result<(), VivariumError> {
        match &node.class {
            NodeClass::Decision => self.expand(node, word),
            NodeClass::Join => self.join(node),
            NodeClass::Raisecheck => {
                let inputs = [self.current(word)];
                self.code_decision(node, &inputs)
            }
            NodeClass::Done | NodeClass::Restart(_) => {
                if self.seen.insert(node.id.clone()) {
                    self.routed.terminal.push(Terminal {
                        node: node.id.clone(),
                        class: node.class.clone(),
                    });
                }
                Ok(())
            }
            NodeClass::Lock | NodeClass::Park | NodeClass::Milestone => {
                self.open_task(node, StepState::Waiting)
            }
            _ => self.open_task(node, StepState::Pending),
        }
    }

    /// A join fires once every predecessor is closed; the finished step is
    /// one of them.
    fn join(&mut self, node: &FlowNode) -> Result<(), VivariumError> {
        let mut inputs = Vec::new();
        for pred in self.def.edges.iter().filter(|e| e.to == node.id) {
            if pred.from == self.outcome.node {
                inputs.push(self.current(&self.outcome.word));
                continue;
            }
            match self.closed_row(&pred.from) {
                Some(input) => inputs.push(input),
                None => {
                    if self.seen.insert(node.id.clone()) {
                        self.routed.waiting.push(node.id.clone());
                    }
                    return Ok(());
                }
            }
        }
        inputs.dedup_by(|a, b| a.node == b.node);
        self.code_decision(node, &inputs)
    }

    fn code_decision(
        &mut self,
        node: &FlowNode,
        inputs: &[JoinInput],
    ) -> Result<(), VivariumError> {
        if !self.seen.insert(node.id.clone()) {
            return Ok(());
        }
        let word = (self.decide)(node, inputs)?;
        if !allows(&node.class, &word) {
            return Err(VivariumError::Parse(format!(
                "{} flow: decision `{}` answered `{word}`, outside its word set",
                self.def.kind.name(),
                node.id
            )));
        }
        self.expand(node, &word)
    }

    fn current(&self, word: &str) -> JoinInput {
        JoinInput {
            node: self.outcome.node.clone(),
            task: self.outcome.task.clone(),
            word: word.to_string(),
        }
    }

    fn closed_row(&self, node: &str) -> Option<JoinInput> {
        let row = self
            .rows
            .iter()
            .rev()
            .find(|r| r.node == node && r.state == StepState::Done)?;
        Some(JoinInput {
            node: row.node.clone(),
            task: row.task.clone(),
            word: row.verdict.clone().unwrap_or_default(),
        })
    }

    /// Send a task for `node`, blocked on the finished step, and index it.
    fn open_task(&mut self, node: &FlowNode, state: StepState) -> Result<(), VivariumError> {
        if !self.seen.insert(node.id.clone()) {
            return Ok(());
        }
        let o = self.outcome;
        // An earlier route that failed part way already sent this node: reuse
        // its row so the retry binds it instead of sending a second task.
        if let Some(row) = self
            .rows
            .iter()
            .find(|r| r.node == node.id && r.state != StepState::Done && r.task != o.task)
        {
            self.routed.sent.push(Sent {
                node: node.id.clone(),
                task: row.task.clone(),
            });
            return Ok(());
        }
        let body = format!(
            "{STEP_MARKER} {} r{} {}\n\n{}",
            o.need, o.run, node.id, node.label
        );
        let subject = format!("{} ({} r{})", node.label, o.need, o.run);
        // A task sent before a crash but never indexed is adopted, not sent twice.
        let first_line = body.lines().next().unwrap_or_default();
        let task = match self.mail.sent_step(&subject, first_line)? {
            Some(task) => task,
            None => self.mail.send_step(
                &self.ctx.cfg.reply_from,
                &self.ctx.cfg.reply_from,
                &subject,
                &body,
                std::slice::from_ref(&o.task),
            )?,
        };
        self.store.insert_step(
            &o.need,
            &StepRow {
                run: o.run,
                node: node.id.clone(),
                task: task.clone(),
                state,
                verdict: None,
            },
        )?;
        self.routed.sent.push(Sent {
            node: node.id.clone(),
            task,
        });
        Ok(())
    }

    /// Bind the new tasks together with the run sentinel, in one call.
    fn bind(&mut self) -> Result<(), VivariumError> {
        if self.routed.sent.is_empty() {
            return Ok(());
        }
        let def = self.def;
        let mut units: Vec<String> = self
            .rows
            .iter()
            .filter(|r| !def.nodes.iter().any(|n| n.id == r.node))
            .map(|r| r.task.clone())
            .take(1)
            .collect();
        units.extend(self.routed.sent.iter().map(|s| s.task.clone()));
        self.mail.bind(&self.outcome.need, &units)
    }

    /// Close the finished step with its word as verdict.
    fn close_predecessor(&mut self) -> Result<(), VivariumError> {
        let o = self.outcome;
        let field = |name: &str| o.payload.get(name).and_then(|v| v.as_str());
        self.mail
            .close_task(&o.task, &o.word, field("branch"), field("commit"))?;
        self.store
            .set_step_state(&o.task, StepState::Done, Some(&o.word))
    }
}

#[cfg(test)]
#[path = "route_test.rs"]
mod tests;
