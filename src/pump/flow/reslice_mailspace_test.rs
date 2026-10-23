//! A reslice request on a real mailspace is idempotent: the stored task body
//! round-trips through the mailspace, and a repeated request sees it.

use std::collections::BTreeMap;

use super::{KIND, Mode, RESLICE_NODE_PREFIX, request};
use crate::error::VivariumError;
use crate::letter::fixture::task_body;
use crate::mailspace::shipped::SLICER_ROLE;
use crate::pump::fixture::project;
use crate::pump::flow::advance::start::start;
use crate::pump::flow::index::{StepRow, StepState};
use crate::pump::flow::mail::{MailWriter, MailspacePort};
use crate::pump::jobs::{JobStore, PoolJob};
use crate::pump::store::flow_links::SliceLink;
use crate::pump::store::{DB_FILENAME, PumpStore};
use crate::routing::RoutedRole;

type R = Result<(), VivariumError>;

/// The pump identity of the default config's `reply_from`.
const PUMP: &str = "mind";

#[derive(Default)]
struct Jobs {
    jobs: BTreeMap<String, PoolJob>,
}

impl JobStore for Jobs {
    fn jobs(&self) -> &BTreeMap<String, PoolJob> {
        &self.jobs
    }

    fn jobs_mut(&mut self) -> &mut BTreeMap<String, PoolJob> {
        &mut self.jobs
    }
}

#[test]
fn a_repeated_request_with_multi_line_input_sends_nothing() -> R {
    let p = project();
    let need = p.need("reslice me");
    let ctx = p.pump_ctx();
    let mut routing = (*ctx.routing).clone();
    routing.roles.retain(|role| role.name != SLICER_ROLE);
    routing.roles.push(RoutedRole {
        name: SLICER_ROLE.into(),
        kind: Some(String::from("slicer")),
        status: String::from("active"),
        pool: Some(String::from("test-slicer-pool")),
        read_only: Some(true),
        model: None,
        models: Vec::new(),
    });
    let ctx = ctx.with_routing(routing);
    let store = PumpStore::open(&ctx.home.join(DB_FILENAME))?;
    let mut port = MailspacePort {
        mailspace: p.mailspace.clone(),
        pump_identity: PUMP.into(),
    };
    MailWriter::open_record(
        &mut port,
        &need,
        &crate::letter::fixture::record_body(&need, "build"),
    )?;
    let run = start(&ctx, &store, &mut port, &need, Some("plan"))?;

    let slice_task = port.send_step(
        PUMP,
        PUMP,
        "slice a",
        &task_body("slice body\nsecond line"),
        &[],
    )?;
    let slice = StepRow {
        run: run.run,
        node: "slice:a".into(),
        task: slice_task.clone(),
        state: StepState::Running,
        verdict: None,
    };
    store.insert_step(&need, &slice)?;
    store.link_slice(&SliceLink {
        task: slice_task,
        need: need.clone(),
        run: run.run,
        fanout: Some("fan1".into()),
        deps: Vec::new(),
    })?;

    let mut jobs = Jobs::default();
    let input = "split it in two\nkeep the tests with the code\n\nthanks";
    let first = request(
        &ctx,
        &mut jobs,
        &store,
        &mut port,
        &run,
        &slice,
        Mode::Split,
        input,
    )?;
    let again = request(
        &ctx,
        &mut jobs,
        &store,
        &mut port,
        &run,
        &slice,
        Mode::Split,
        input,
    )?;

    assert_eq!(
        first, again,
        "the second request must return the first task"
    );
    let reslices: Vec<StepRow> = store
        .steps_of(&need, run.run)?
        .into_iter()
        .filter(|row| row.node.starts_with(RESLICE_NODE_PREFIX))
        .collect();
    assert_eq!(reslices.len(), 1, "{reslices:?}");
    let kinds: Vec<&PoolJob> = jobs.jobs.values().filter(|j| j.kind == KIND).collect();
    assert_eq!(kinds.len(), 1, "{kinds:?}");
    Ok(())
}
