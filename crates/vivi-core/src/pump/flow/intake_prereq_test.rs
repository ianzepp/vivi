//! Prerequisite admission through the real mailspace: a queued build enters
//! no ranking and consumes no slot until every prerequisite has landed, and a
//! lower-ranked eligible need fills the slot meanwhile. The dependency
//! letters here are real `dependency-added` / `dependency-removed` events.

use std::collections::BTreeMap;

use super::register;
use crate::mailspace::SendRequest;
use crate::pump::fixture::{Project, project};
use crate::pump::flow::handlers::Registry;
use crate::pump::flow::mail::{MailReader, MailWriter, MailspacePort};
use crate::pump::flow::tick::tick_with;
use crate::pump::host::Ctx;
use crate::pump::jobs::{JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, PumpStore};
use vivi_error::VivariumError;
use vivi_mail::letter::fixture::{need_body_with, record_body};
use vivi_mail::letter::work::NeedFront;

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

struct Rig {
    project: Project,
    ctx: Ctx,
    store: PumpStore,
    port: MailspacePort,
    jobs: Jobs,
    registry: Registry<Jobs, MailspacePort>,
}

fn rig() -> Result<Rig, VivariumError> {
    let project = project();
    rig_over(project)
}

fn rig_over(project: Project) -> Result<Rig, VivariumError> {
    let ctx = project.pump_ctx();
    let store = PumpStore::open(&ctx.home.join(DB_FILENAME))?;
    let port = MailspacePort {
        mailspace: crate::pump::record::PumpMailspace::new(project.mailspace.clone()),
        pump_identity: PUMP.into(),
    };
    let mut registry = Registry::new();
    register(&mut registry);
    Ok(Rig {
        project,
        ctx,
        store,
        port,
        jobs: Jobs::default(),
        registry,
    })
}

/// A need sent from `operator` to the pump identity, depending on `deps`.
fn send_need(project: &Project, subject: &str, deps: &[String]) -> String {
    let front = NeedFront {
        depends_on: deps.to_vec(),
        ..NeedFront::new(vec![String::from("it works")])
    };
    project
        .mailspace
        .send(SendRequest {
            from: "operator".into(),
            to: vec![PUMP.into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.into(),
            body: need_body_with(&front, ""),
            role: "needs".into(),
            kind: Some("need".into()),
            reply_to: None,
        })
        .unwrap()
        .delivered
        .remove(0)
        .handle
}

impl Rig {
    fn need(&self, subject: &str) -> String {
        send_need(&self.project, subject, &[])
    }

    /// A need that waits for every handle in `deps` from its front matter.
    fn dependent(&self, subject: &str, deps: &[&str]) -> String {
        let deps: Vec<String> = deps.iter().map(ToString::to_string).collect();
        send_need(&self.project, subject, &deps)
    }

    fn record(&mut self, need: &str, kind: &str) -> R {
        MailWriter::open_record(&mut self.port, need, &record_body(need, kind)).map(drop)
    }

    fn depend(&self, need: &str, add: &[&str], remove: &[&str]) -> R {
        let add: Vec<String> = add.iter().map(ToString::to_string).collect();
        let remove: Vec<String> = remove.iter().map(ToString::to_string).collect();
        self.project
            .mailspace
            .set_dependencies(PUMP, need, "need", &add, &remove)
            .map(drop)
    }

    fn land(&mut self, need: &str) -> R {
        MailWriter::close_need(&mut self.port, need, "landed")
    }

    fn tick(&mut self) -> R {
        tick_with(
            &self.ctx,
            &mut self.jobs,
            &self.store,
            &mut self.port,
            &self.registry,
        )
    }

    fn runs(&self, need: &str) -> Result<usize, VivariumError> {
        Ok(self.store.runs_of(need)?.len())
    }

    /// Assert `need` got no run, sentinel, bound unit, step or job.
    fn assert_waits(&self, need: &str) -> R {
        assert_eq!(self.runs(need)?, 0, "{need} started a run");
        assert_eq!(self.port.sentinel_count(need)?, 0, "{need} got a sentinel");
        assert!(
            self.port.bound_units(need)?.is_empty(),
            "{need} got a bound unit"
        );
        assert!(
            self.store.steps_of(need, 1)?.is_empty(),
            "{need} got a step"
        );
        assert!(self.jobs.jobs.is_empty(), "{need} started a job");
        Ok(())
    }

    /// Assert `need` got exactly one run at its entry node.
    fn assert_started(&self, need: &str) -> R {
        let runs = self.store.runs_of(need)?;
        assert_eq!(runs.len(), 1, "{need} runs: {runs:?}");
        assert_eq!(runs[0].run, 1);
        assert_eq!(runs[0].state, "live");
        Ok(())
    }
}

#[test]
fn front_matter_dependency_waits_then_starts() -> R {
    let mut rig = rig()?;
    let prereq = rig.need("prerequisite");
    let build = rig.dependent("dependent", &[&prereq]);
    rig.record(&build, "build")?;

    rig.tick()?;
    rig.assert_waits(&build)?;
    let waits = rig.store.build_waits()?;
    assert_eq!(waits.len(), 1);
    assert_eq!(waits[0].need, build);

    // A second tick while the prerequisite is still open changes nothing.
    rig.tick()?;
    rig.assert_waits(&build)?;

    rig.land(&prereq)?;
    rig.tick()?;
    rig.assert_started(&build)?;
    assert!(rig.store.build_waits()?.is_empty());
    Ok(())
}

#[test]
fn letter_dependency_waits_then_starts() -> R {
    let mut rig = rig()?;
    let prereq = rig.need("prerequisite");
    let build = rig.dependent("dependent", &[]);
    rig.record(&build, "build")?;
    rig.depend(&build, &[&prereq], &[])?;

    rig.tick()?;
    rig.assert_waits(&build)?;

    rig.land(&prereq)?;
    rig.tick()?;
    rig.assert_started(&build)?;
    Ok(())
}

#[test]
fn last_prerequisite_controls_admission() -> R {
    let mut rig = rig()?;
    let first = rig.need("first prerequisite");
    let second = rig.need("second prerequisite");
    let build = rig.dependent("dependent", &[&first, &second]);
    rig.record(&build, "build")?;

    rig.land(&first)?;
    rig.tick()?;
    rig.assert_waits(&build)?;
    let waits = rig.store.build_waits()?;
    assert_eq!(waits.len(), 1);
    let left: Vec<&str> = waits[0]
        .prerequisites
        .iter()
        .map(|p| p.handle.as_str())
        .collect();
    assert_eq!(left, [second.as_str()]);

    rig.land(&second)?;
    rig.tick()?;
    rig.assert_started(&build)?;
    Ok(())
}

#[test]
fn newest_dependency_letter_controls_admission() -> R {
    let mut rig = rig()?;
    let landed = rig.need("landed prerequisite");
    let open = rig.need("open prerequisite");
    let build = rig.dependent("dependent", &[&open]);
    rig.record(&build, "build")?;
    rig.land(&landed)?;

    // The newest letters win over the front matter: `open` is dropped and
    // `landed` added, delivered as one letter pair.
    rig.depend(&build, &[&landed], &[&open])?;

    rig.tick()?;
    rig.assert_started(&build)?;

    // Re-adding `open` after the start queued nothing more: the run stays
    // the one that already crossed admission.
    rig.depend(&build, &[&open], &[])?;
    rig.tick()?;
    assert_eq!(rig.runs(&build)?, 1);
    Ok(())
}

#[test]
fn dependency_removal_releases_waiter() -> R {
    let mut rig = rig()?;
    let prereq = rig.need("prerequisite");
    let build = rig.dependent("dependent", &[&prereq]);
    rig.record(&build, "build")?;

    rig.tick()?;
    rig.assert_waits(&build)?;
    assert_eq!(rig.store.build_waits()?.len(), 1);

    // The removal letter overrides the front matter and releases the build.
    rig.depend(&build, &[], &[&prereq])?;
    rig.tick()?;
    rig.assert_started(&build)?;
    assert!(rig.store.build_waits()?.is_empty());

    // A re-added dependency does not queue a run that already started.
    rig.depend(&build, &[&prereq], &[])?;
    rig.tick()?;
    assert_eq!(rig.runs(&build)?, 1);
    Ok(())
}
