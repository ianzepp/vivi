//! Tests for the `thread-change` phase, on a real mailspace: a reply by another
//! member after the run's start post restarts a live or parked run at `plan`,
//! raises once in a locked run, and a reply by the pump changes nothing.

use std::thread::sleep;
use std::time::Duration;

use super::{THREAD_CHANGED, THREAD_CHECK_FAILED, ThreadChange};
use crate::error::VivariumError;
use crate::letter::fixture::{record_body, step_body};
use crate::letter::{FORMAT_HEADER, FORMAT_VERSION};
use crate::mailspace::{Mailspace, Recipient};
use crate::pump::fixture::{Project, project};
use crate::pump::flow::advance::start::start;
use crate::pump::flow::handlers::TickPhase;
use crate::pump::flow::index::{StepRow, StepState};
use crate::pump::flow::mail::{MailWriter, MailspacePort};
use crate::pump::flow::scope_request::{RULING_MARKER, ScopeRequest, adjudicate};
use crate::pump::flow::slice_dispatch::SliceMail;
use crate::pump::host::Ctx;
use crate::pump::store::scope_ruling::RulingDecision;
use crate::pump::store::{DB_FILENAME, PumpStore};

type R = Result<(), VivariumError>;

const PUMP: &str = "mind";

struct Rig {
    _project: Project,
    ctx: Ctx,
    store: PumpStore,
    port: MailspacePort,
    need: String,
}

/// A need with an open pump record and a build run started at `plan`.
fn rig() -> Result<Rig, VivariumError> {
    let p = project();
    let ctx = p.pump_ctx();
    let need = p.need("change me");
    let store = PumpStore::open(&ctx.home.join(DB_FILENAME))?;
    let mut port = MailspacePort {
        mailspace: p.mailspace.clone(),
        pump_identity: PUMP.into(),
    };
    MailWriter::open_record(&mut port, &need, &record_body(&need, "build"))?;
    start(&ctx, &store, &mut port, &need, Some("plan"))?;
    Ok(Rig {
        _project: p,
        ctx,
        store,
        port,
        need,
    })
}

impl Rig {
    /// A reply on the need's thread, one second after the run's start post.
    fn reply_as(&self, from: &str) -> R {
        sleep(Duration::from_millis(1100));
        let to = if from == PUMP { "operator" } else { PUMP };
        self.port.mailspace.reply(
            &self.need,
            from,
            vec![to.into()],
            Vec::new(),
            None,
            "actually, do it differently".into(),
        )?;
        Ok(())
    }

    fn phase(&mut self) -> R {
        ThreadChange.run(&self.ctx, &mut (), &self.store, &mut self.port)
    }

    fn runs(&self) -> Result<Vec<(u32, String, String)>, VivariumError> {
        Ok(self
            .store
            .runs_of(&self.need)?
            .into_iter()
            .map(|run| (run.run, run.entry, run.state))
            .collect())
    }

    /// A waiting step row for `node` on run 1, over a real task.
    fn hold(&mut self, node: &str) -> R {
        let task =
            self.port
                .send_step(PUMP, PUMP, node, &step_body(&self.need, 1, node, ""), &[])?;
        self.store.insert_step(
            &self.need,
            &StepRow {
                run: 1,
                node: node.into(),
                task,
                state: StepState::Waiting,
                verdict: None,
            },
        )
    }

    fn exceptions(&self) -> String {
        std::fs::read_to_string(self.ctx.home.join("exceptions.txt")).unwrap_or_default()
    }
}

#[test]
fn a_reply_by_another_member_restarts_a_live_run_at_plan() -> R {
    let mut rig = rig()?;
    rig.reply_as("operator")?;
    rig.phase()?;
    let runs = rig.runs()?;
    assert_eq!(runs.len(), 2, "{runs:?}");
    assert_eq!(runs[0], (1, "plan".into(), "superseded".into()));
    assert_eq!(runs[1], (2, "plan".into(), "live".into()));
    assert_eq!(rig.exceptions(), "");
    // The next pass sees the reply as older than run 2's start: no third run.
    rig.phase()?;
    assert_eq!(rig.runs()?.len(), 2);
    Ok(())
}

#[test]
fn the_run_started_post_is_stored_as_written_and_still_counts() -> R {
    let mut rig = rig()?;
    let thread = rig
        .port
        .mailspace
        .thread(&rig.need, false, usize::MAX, usize::MAX)?;
    let start = thread
        .iter()
        .find(|m| crate::letter::prose_of(&m.body).starts_with("pump: run r1 started"))
        .expect("the run-started post");
    assert!(!start.body.contains("+++"), "{}", start.body);
    rig.reply_as("operator")?;
    rig.phase()?;
    assert_eq!(rig.runs()?.len(), 2);
    Ok(())
}

#[test]
fn a_reply_by_the_pump_identity_changes_nothing() -> R {
    let mut rig = rig()?;
    rig.reply_as(PUMP)?;
    rig.phase()?;
    assert_eq!(rig.runs()?, [(1, "plan".into(), "live".into())]);
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

#[test]
fn a_parked_run_restarts_at_plan() -> R {
    let mut rig = rig()?;
    rig.hold("parked")?;
    rig.reply_as("operator")?;
    rig.phase()?;
    let runs = rig.runs()?;
    assert_eq!(runs.len(), 2, "{runs:?}");
    assert_eq!(runs[1], (2, "plan".into(), "live".into()));
    Ok(())
}

#[test]
fn a_reply_in_a_locked_run_raises_once_and_starts_nothing() -> R {
    let mut rig = rig()?;
    rig.hold("locked")?;
    rig.reply_as("operator")?;
    rig.phase()?;
    rig.phase()?;
    assert_eq!(rig.runs()?, [(1, "plan".into(), "live".into())]);
    let raised = rig.exceptions();
    assert_eq!(raised.matches(THREAD_CHANGED).count(), 1, "{raised}");
    assert!(raised.contains(&rig.need), "{raised}");
    Ok(())
}

#[test]
fn a_reply_after_merge_main_started_raises_and_does_not_restart() -> R {
    let mut rig = rig()?;
    rig.hold("merge")?;
    rig.reply_as("operator")?;
    rig.phase()?;
    assert_eq!(rig.runs()?, [(1, "plan".into(), "live".into())]);
    assert_eq!(rig.exceptions().matches(THREAD_CHANGED).count(), 1);
    Ok(())
}

/// Corrupt the stored date of every letter the pump sent, the run-start post
/// included.
fn corrupt_pump_dates(rig: &Rig) {
    let db = rig.ctx.project.join(".vivi").join("mail.sqlite");
    let conn = rusqlite::Connection::open(&db).unwrap();
    let changed = conn
        .execute(
            "UPDATE message_metadata SET date = 'last tuesday' WHERE from_addr LIKE '%mind%'",
            [],
        )
        .unwrap();
    assert!(changed > 0, "no pump letter to corrupt");
}

#[test]
fn an_unparsable_start_date_raises_once_and_restarts_nothing() -> R {
    let mut rig = rig()?;
    rig.reply_as("operator")?;
    corrupt_pump_dates(&rig);
    rig.phase()?;
    rig.phase()?;
    assert_eq!(rig.runs()?, [(1, "plan".into(), "live".into())]);
    let raised = rig.exceptions();
    assert_eq!(raised.matches(THREAD_CHECK_FAILED).count(), 1, "{raised}");
    assert!(raised.contains("last tuesday"), "{raised}");
    assert!(raised.contains(&rig.need), "{raised}");
    assert_eq!(raised.matches(THREAD_CHANGED).count(), 0, "{raised}");
    Ok(())
}

#[test]
fn a_valid_run_raises_no_check_failure() -> R {
    let mut rig = rig()?;
    rig.phase()?;
    rig.phase()?;
    assert_eq!(rig.runs()?, [(1, "plan".into(), "live".into())]);
    assert_eq!(rig.exceptions().matches(THREAD_CHECK_FAILED).count(), 0);
    Ok(())
}

/// The raw stored bytes of the letter `handle` names.
fn raw_message(mailspace: &Mailspace, handle: &str) -> Result<String, VivariumError> {
    let storage = mailspace.storage()?;
    let id = storage.resolve_message_token(handle)?;
    Ok(String::from_utf8_lossy(&storage.read_message(&id)?).into_owned())
}

/// An approved scope request against the rig's need, posted after the run's
/// start post. The default target's whole-repository policy grants the path.
fn post_approved_ruling(rig: &mut Rig) -> R {
    sleep(Duration::from_millis(1100));
    let configs = rig.ctx.cfg.target_configs();
    let request = ScopeRequest {
        job: String::from("job-scope-1"),
        job_kind: String::from("hand"),
        need: rig.need.clone(),
        run: 1,
        node: String::from("slice"),
        task: String::from("task-scope-1"),
        role: String::from("hand"),
        target: String::from("default"),
        paths: vec![String::from("assets/notes.md")],
        reason: String::from("the notes live outside the slice write scope"),
    };
    adjudicate(&rig.ctx, &configs, &rig.store, &mut rig.port, &request)?;
    Ok(())
}

#[test]
fn approved_scope_note_does_not_restart_run() -> R {
    let mut rig = rig()?;
    post_approved_ruling(&mut rig)?;

    // The database ruling is the grant authority, not the thread string.
    let stored = rig
        .store
        .scope_ruling("job-scope-1")?
        .expect("stored ruling");
    assert_eq!(stored.decision, RulingDecision::Approved);
    assert_eq!(stored.paths, ["assets/notes.md"]);

    // The reply the pump actually stored on the need thread.
    let thread = rig
        .port
        .mailspace
        .thread(&rig.need, false, usize::MAX, usize::MAX)?;
    let reply = thread
        .iter()
        .find(|m| crate::letter::prose_of(&m.body).starts_with(RULING_MARKER))
        .expect("ruling reply on the need thread");
    let pump = rig.ctx.cfg.reply_from.clone();
    assert!(
        matches!(
            rig.port.mailspace.classify_recipient(&reply.from),
            Ok(Recipient::Local(id)) if id == pump
        ),
        "{}",
        reply.from
    );
    let raw = raw_message(&rig.port.mailspace, &reply.handle)?;
    assert!(
        raw.contains(&format!("{FORMAT_HEADER}: {FORMAT_VERSION}")),
        "{raw}"
    );
    let body = crate::letter::prose_of(&reply.body);
    assert_eq!(
        body.trim_end(),
        stored.reply_body.as_deref().unwrap_or_default().trim_end()
    );
    assert!(body.contains(stored.decision_ref.as_deref().unwrap_or_default()));
    assert!(body.contains("decision: approved"), "{body}");
    assert!(
        body.contains("effective additions: assets/notes.md"),
        "{body}"
    );

    // The real phase twice: the pump-authored ruling restarts nothing.
    rig.phase()?;
    rig.phase()?;
    assert_eq!(rig.runs()?, [(1, "plan".into(), "live".into())]);
    assert_eq!(rig.exceptions(), "");

    // The grant it left behind is exactly the approved addition.
    let scope = rig.store.effective_need_scope(&rig.need, "default", &[])?;
    assert_eq!(scope.additions, ["assets/notes.md"]);

    // A replay through the real port recognizes the letter: still one reply.
    post_approved_ruling(&mut rig)?;
    assert_eq!(rig.port.marked_replies(&rig.need, RULING_MARKER)?.len(), 1);

    // Contrast with the operator-reply behavior above, left as it is: a
    // reply by another member still restarts the run, so the ruling was
    // exempted because the pump wrote it, not because the check went dead.
    rig.reply_as("operator")?;
    rig.phase()?;
    let runs = rig.runs()?;
    assert_eq!(runs.len(), 2, "{runs:?}");
    assert_eq!(runs[1], (2, "plan".into(), "live".into()));
    Ok(())
}
