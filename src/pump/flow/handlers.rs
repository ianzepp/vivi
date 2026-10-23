//! The flow registry: step handlers, result collectors, built-slice hooks, the decide slot and tick phases.
//!
//! Handlers are keyed by `(FlowKind, NodeClass)`; a node override keyed by
//! `(FlowKind, node id)` wins over the class handler for that node only. Later
//! slices fill the five module hooks `shipped` calls and never edit this file
//! for them.

use std::collections::BTreeMap;

use crate::error::VivariumError;
use crate::pump::flow::advance::restart;
use crate::pump::flow::advance::route::JoinInput;
use crate::pump::flow::collect_slice::{self, BuiltHook, PostBuiltNotes};
use crate::pump::flow::collect_step::{self, StepCollector};
use crate::pump::flow::def::{FlowKind, FlowNode, NodeClass};
use crate::pump::flow::fanout::SliceEdges;
use crate::pump::flow::mail::{MailWriter, OpenRecords};
use crate::pump::flow::outcome::StepHandler;
use crate::pump::flow::slice_dispatch::SliceMail;
use crate::pump::flow::{intake, need_branch, prune, rung_step};
use crate::pump::flow::{integration, main_rung, slice_merge};
use crate::pump::host::Ctx;
use crate::pump::jobs::JobStore;
use crate::pump::store::PumpStore;

/// Everything the flow engine asks of a mail port.
pub(crate) trait FlowMail: MailWriter + SliceMail + SliceEdges + OpenRecords {}

impl<T: MailWriter + SliceMail + SliceEdges + OpenRecords> FlowMail for T {}

/// One whole-tick job (intake, restart, prune, ...) run after the live runs.
pub(crate) trait TickPhase<S, M> {
    /// The phase's name, used for its error boundary.
    fn name(&self) -> &'static str;

    /// Run the phase once.
    ///
    /// # Errors
    /// Returns a [`VivariumError`]; the tick holds it at the phase's boundary.
    fn run(
        &self,
        ctx: &Ctx,
        st: &mut S,
        store: &PumpStore,
        mail: &mut M,
    ) -> Result<(), VivariumError>;
}

/// The code decision of a join or raise check.
type DecideFn = dyn Fn(&PumpStore, &FlowNode, &[JoinInput]) -> Result<String, VivariumError>;

/// Handlers, the decide slot and the tick phases.
pub(crate) struct Registry<S, M> {
    classes: BTreeMap<(FlowKind, NodeClass), Box<dyn StepHandler>>,
    nodes: BTreeMap<(FlowKind, String), Box<dyn StepHandler>>,
    collectors: BTreeMap<(FlowKind, NodeClass), Box<dyn StepCollector<S, M>>>,
    built_hooks: Vec<Box<dyn BuiltHook<S, M>>>,
    decide: Box<DecideFn>,
    phases: Vec<Box<dyn TickPhase<S, M>>>,
}

impl<S, M> Registry<S, M> {
    pub(crate) fn new() -> Self {
        Self {
            classes: BTreeMap::new(),
            nodes: BTreeMap::new(),
            collectors: BTreeMap::new(),
            built_hooks: Vec::new(),
            decide: Box::new(no_decision_rule),
            phases: Vec::new(),
        }
    }

    /// Register the handler of every node of `class` in `kind`.
    pub(crate) fn handler(&mut self, kind: FlowKind, class: NodeClass, h: Box<dyn StepHandler>) {
        self.classes.insert((kind, class), h);
    }

    /// Register the handler of one node of `kind`; it wins over the class handler.
    pub(crate) fn node_handler(&mut self, kind: FlowKind, node: &str, h: Box<dyn StepHandler>) {
        self.nodes.insert((kind, node.to_owned()), h);
    }

    /// The handler for `node` of `kind`: the node override, else the class handler.
    pub(crate) fn handler_for(&self, kind: FlowKind, node: &FlowNode) -> Option<&dyn StepHandler> {
        self.nodes
            .get(&(kind, node.id.clone()))
            .or_else(|| self.classes.get(&(kind, node.class.clone())))
            .map(Box::as_ref)
    }

    /// Register the result collector of every node of `class` in `kind`; a
    /// later registration replaces an earlier one.
    pub(crate) fn collector(
        &mut self,
        kind: FlowKind,
        class: NodeClass,
        c: Box<dyn StepCollector<S, M>>,
    ) {
        self.collectors.insert((kind, class), c);
    }

    /// Add a built-slice hook; hooks run in registration order.
    pub(crate) fn built_hook(&mut self, h: Box<dyn BuiltHook<S, M>>) {
        self.built_hooks.push(h);
    }

    /// Replace the decide slot.
    pub(crate) fn set_decide<F>(&mut self, f: F)
    where
        F: Fn(&PumpStore, &FlowNode, &[JoinInput]) -> Result<String, VivariumError> + 'static,
    {
        self.decide = Box::new(f);
    }

    /// The answer word of a code decision node.
    ///
    /// # Errors
    /// Returns the decide slot's error; the default has no rule.
    pub(crate) fn decide(
        &self,
        store: &PumpStore,
        node: &FlowNode,
        inputs: &[JoinInput],
    ) -> Result<String, VivariumError> {
        (self.decide)(store, node, inputs)
    }

    /// Add a tick phase; phases run in registration order.
    pub(crate) fn phase(&mut self, p: Box<dyn TickPhase<S, M>>) {
        self.phases.push(p);
    }

    pub(crate) fn phases(&self) -> &[Box<dyn TickPhase<S, M>>] {
        &self.phases
    }
}

impl<S: JobStore, M: FlowMail> Registry<S, M> {
    /// The result collector for steps of `class` in `kind`: the registered
    /// one, else the class's default (a registry built by hand with
    /// [`Registry::new`] still collects every step class).
    pub(crate) fn collector_for(
        &self,
        kind: FlowKind,
        class: &NodeClass,
    ) -> Option<&dyn StepCollector<S, M>> {
        self.collectors
            .get(&(kind, class.clone()))
            .map(Box::as_ref)
            .or_else(|| collect_step::default_for(class))
    }

    /// The hooks to run on a built slice, in registration order; a registry
    /// built by hand with [`Registry::new`] runs the default notes hook.
    pub(crate) fn built_hooks(&self) -> Vec<&dyn BuiltHook<S, M>> {
        if self.built_hooks.is_empty() {
            return vec![&PostBuiltNotes];
        }
        self.built_hooks.iter().map(Box::as_ref).collect()
    }
}

/// No code decision has a rule yet: a join or raise check that routing reaches
/// is an error held on its run.
fn no_decision_rule(
    _store: &PumpStore,
    node: &FlowNode,
    _inputs: &[JoinInput],
) -> Result<String, VivariumError> {
    Err(VivariumError::Parse(format!(
        "decision node `{}` has no decision rule yet",
        node.id
    )))
}

/// The registry the pump ships.
pub(crate) fn shipped<S: JobStore, M: FlowMail>(_ctx: &Ctx) -> Registry<S, M> {
    let mut registry = Registry::new();
    collect_step::register_defaults(&mut registry);
    collect_slice::register_defaults(&mut registry);
    restart::register(&mut registry);
    intake::register(&mut registry);
    need_branch::register(&mut registry);
    prune::register(&mut registry);
    rung_step::register(&mut registry);
    slice_merge::register(&mut registry);
    integration::register(&mut registry);
    main_rung::register(&mut registry);
    registry
}

#[cfg(test)]
#[path = "handlers_test.rs"]
mod tests;
