//! Tests for the flow registry: node overrides, the decide slot and phase order.

use std::sync::{Arc, Mutex};

use tempfile::{TempDir, tempdir};

use super::{Registry, TickPhase};
use crate::error::VivariumError;
use crate::pump::config::PumpConfig;
use crate::pump::flow::def::{FlowKind, FlowNode, NodeClass};
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::load::load;
use crate::pump::flow::outcome::{StartAction, StartCx, StepHandler};
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::jobs::JobStore;
use crate::pump::store::{DB_FILENAME, PumpStore};

type Reg = Registry<(), FakeMail>;

/// A handler that names itself when polled.
struct Named(&'static str);

impl StepHandler for Named {
    fn start(&self, _cx: &StartCx<'_>, _step: &StepRow) -> Result<StartAction, VivariumError> {
        Ok(StartAction::OpenAndWait)
    }

    fn poll(&self, _cx: &StartCx<'_>, _step: &StepRow) -> Result<Option<String>, VivariumError> {
        Ok(Some(self.0.to_owned()))
    }
}

struct Rig {
    _dir: TempDir,
    ctx: Ctx,
    store: PumpStore,
    mail: FakeMail,
    run: RunRow,
}

fn rig() -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    Ok(Rig {
        _dir: dir,
        ctx,
        store,
        mail: FakeMail::new("mind"),
        run: RunRow {
            need: "n1".into(),
            run: 1,
            flow: "build".into(),
            flow_version: 2,
            kind: "build".into(),
            entry: "plan".into(),
            state: "live".into(),
        },
    })
}

fn node(id: &str, class: NodeClass) -> FlowNode {
    FlowNode {
        id: id.into(),
        class,
        label: String::new(),
    }
}

fn step(node: &str) -> StepRow {
    StepRow {
        run: 1,
        node: node.into(),
        task: format!("t-{node}"),
        state: StepState::Waiting,
        verdict: None,
    }
}

/// The name of the handler `registry` picks for `kind` and `node`, if any.
fn picked(
    rig: &Rig,
    registry: &Reg,
    kind: FlowKind,
    node: &FlowNode,
) -> Result<Option<String>, VivariumError> {
    let def = load(FlowKind::Build)?;
    let cx = StartCx {
        ctx: &rig.ctx,
        store: &rig.store,
        mail: &rig.mail,
        run: &rig.run,
        def: &def,
    };
    match registry.handler_for(kind, node) {
        Some(handler) => handler.poll(&cx, &step(&node.id)),
        None => Ok(None),
    }
}

#[test]
fn node_override_wins_for_its_node_and_kind_only() -> Result<(), VivariumError> {
    let rig = rig()?;
    let mut registry = Reg::new();
    registry.handler(
        FlowKind::Build,
        NodeClass::Planner,
        Box::new(Named("class")),
    );
    registry.handler(
        FlowKind::Research,
        NodeClass::Planner,
        Box::new(Named("research-class")),
    );
    registry.node_handler(FlowKind::Build, "plan", Box::new(Named("node")));

    let plan = node("plan", NodeClass::Planner);
    let other = node("plan2", NodeClass::Planner);
    assert_eq!(
        picked(&rig, &registry, FlowKind::Build, &plan)?.as_deref(),
        Some("node")
    );
    assert_eq!(
        picked(&rig, &registry, FlowKind::Build, &other)?.as_deref(),
        Some("class")
    );
    // The override belongs to the build kind: research falls back to its class.
    assert_eq!(
        picked(&rig, &registry, FlowKind::Research, &plan)?.as_deref(),
        Some("research-class")
    );
    // No handler for another class, and none for another kind.
    let judge = node("judge", NodeClass::Judge);
    assert_eq!(picked(&rig, &registry, FlowKind::Build, &judge)?, None);
    assert_eq!(picked(&rig, &registry, FlowKind::Release, &plan)?, None);
    Ok(())
}

#[test]
fn default_decide_slot_errors_and_can_be_replaced() -> Result<(), VivariumError> {
    let rig = rig()?;
    let join = node("join", NodeClass::Join);
    let mut registry = Reg::new();
    let Err(error) = registry.decide(&rig.store, &join, &[]) else {
        return Err(VivariumError::Other(
            "the default decide slot answered".into(),
        ));
    };
    assert!(
        error.to_string().contains("no decision rule yet"),
        "unexpected error: {error}"
    );
    registry.set_decide(|_store, node, _inputs| Ok(format!("decided {}", node.id)));
    assert_eq!(registry.decide(&rig.store, &join, &[])?, "decided join");
    Ok(())
}

struct Mark {
    name: &'static str,
    log: Arc<Mutex<Vec<&'static str>>>,
}

impl TickPhase<(), FakeMail> for Mark {
    fn name(&self) -> &'static str {
        self.name
    }

    fn run(
        &self,
        _ctx: &Ctx,
        _st: &mut (),
        _store: &PumpStore,
        _mail: &mut FakeMail,
    ) -> Result<(), VivariumError> {
        self.log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(self.name);
        Ok(())
    }
}

#[test]
fn phases_come_back_in_registration_order() -> Result<(), VivariumError> {
    let mut rig = rig()?;
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut registry = Reg::new();
    for name in ["intake", "restart", "prune"] {
        registry.phase(Box::new(Mark {
            name,
            log: Arc::clone(&log),
        }));
    }
    let names: Vec<&str> = registry.phases().iter().map(|p| p.name()).collect();
    assert_eq!(names, ["intake", "restart", "prune"]);
    for phase in registry.phases() {
        phase.run(&rig.ctx, &mut (), &rig.store, &mut rig.mail)?;
    }
    let ran = log
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    assert_eq!(ran, ["intake", "restart", "prune"]);
    Ok(())
}

#[test]
fn shipped_registers_a_collector_for_every_step_class() -> Result<(), VivariumError> {
    let rig = rig()?;
    struct Jobs(std::collections::BTreeMap<String, crate::pump::jobs::PoolJob>);
    impl JobStore for Jobs {
        fn jobs(&self) -> &std::collections::BTreeMap<String, crate::pump::jobs::PoolJob> {
            &self.0
        }

        fn jobs_mut(
            &mut self,
        ) -> &mut std::collections::BTreeMap<String, crate::pump::jobs::PoolJob> {
            &mut self.0
        }
    }
    let registry = super::shipped::<Jobs, FakeMail>(&rig.ctx);
    for kind in [
        FlowKind::Build,
        FlowKind::Research,
        FlowKind::Release,
        FlowKind::Repair,
    ] {
        let def = load(kind)?;
        for node in def.nodes.iter().filter(|n| n.class.is_step()) {
            assert!(
                registry.collector_for(kind, &node.class).is_some(),
                "no collector for {} ({}) of {}",
                node.id,
                node.class.name(),
                kind.name()
            );
        }
    }
    Ok(())
}
