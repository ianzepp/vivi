//! End to end: the research flow on a real mailspace, one test per answer
//! edge of `flows/research.mmd` (READY, NOT READY, NEEDS FURTHER REVIEW and
//! failed). Each test only finishes the researcher job and ticks.

use std::sync::Arc;

use crate::error::VivariumError;
use crate::letter::fixture::{need_body_with, want_body};
use crate::letter::work::NeedFront;
use crate::mailspace::{Mailspace, SendRequest};
use crate::pump::config::PumpConfig;
use crate::pump::fixture::project;
use crate::pump::flow::advance::start::start;
use crate::pump::flow::e2e_rig::{Jobs, Rig, agent_receipt};
use crate::pump::flow::handlers::shipped;
use crate::pump::flow::mail::{MailReader, MailWriter, MailspacePort, NeedStatus};
use crate::pump::jobs::JobStatus;
use crate::pump::store::{DB_FILENAME, PumpStore};
use crate::routing::RoutedRole;

type R = Result<(), VivariumError>;

const PUMP: &str = "mind";
const RESEARCH_FAILED: &str = "flow-research-failed";

/// A research rig whose need names a real want with a `for:` line. The shared
/// `rig()` sends a fixed need body, and the `for:` line is read from the need
/// body, so the rig is assembled here from the same parts.
fn research_rig() -> Result<(Rig, String), VivariumError> {
    let project = project();
    let want = send(
        &project.mailspace,
        "wants",
        "want",
        "the want",
        &want_body("wanted", ""),
    )?;
    let body = need_body_with(
        &NeedFront {
            explores: Some(want.clone()),
            ..NeedFront::new(vec!["it works".into()])
        },
        "",
    );
    let need = send(
        &project.mailspace,
        "needs",
        "need",
        "the research need",
        &body,
    )?;
    let mut ctx = project.pump_ctx();
    ctx.cfg = Arc::new(PumpConfig::default());
    let mut routing = (*ctx.routing).clone();
    let pool = routing
        .roles
        .first()
        .and_then(|role| role.pool.clone())
        .ok_or_else(|| VivariumError::Message("fixture routing has no pool".into()))?;
    routing.roles.push(RoutedRole {
        name: "researcher".into(),
        kind: Some("researcher".into()),
        status: String::from("active"),
        pool: Some(pool),
        read_only: None,
        model: None,
        models: Vec::new(),
    });
    ctx = ctx.with_routing(routing);
    let store = PumpStore::open(&ctx.home.join(DB_FILENAME))?;
    let mut port = MailspacePort {
        mailspace: project.mailspace.clone(),
        pump_identity: PUMP.into(),
    };
    MailWriter::open_record(
        &mut port,
        &need,
        &crate::letter::fixture::record_body(&need, "research"),
    )?;
    start(&ctx, &store, &mut port, &need, Some("research"))?;
    let registry = shipped::<Jobs, MailspacePort>(&ctx);
    let rig = Rig {
        project,
        ctx,
        store,
        port,
        jobs: Jobs::default(),
        registry,
        need,
    };
    Ok((rig, want))
}

fn send(
    mailspace: &Mailspace,
    role: &str,
    kind: &str,
    subject: &str,
    body: &str,
) -> Result<String, VivariumError> {
    let mut result = mailspace.send(SendRequest {
        from: "operator".into(),
        to: vec![PUMP.into()],
        cc: Vec::new(),
        bcc: Vec::new(),
        subject: subject.into(),
        body: body.into(),
        role: role.into(),
        kind: Some(kind.into()),
        reply_to: None,
    })?;
    if result.delivered.is_empty() {
        return Err(VivariumError::Message(format!("{subject}: not delivered")));
    }
    Ok(result.delivered.remove(0).handle)
}

/// The bodies of the pump's research replies on the want, oldest first.
fn research_replies(rig: &Rig, want: &str) -> Result<Vec<String>, VivariumError> {
    Ok(rig
        .project
        .mailspace
        .thread(want, false, 50, 8)?
        .into_iter()
        .map(|message| message.body)
        .filter(|body| crate::letter::prose_of(body).starts_with("pump: research"))
        .collect())
}

fn envelope(word: &str) -> String {
    format!("+++\nanswer = \"{word}\"\n+++\n\n## Verdict\nAnalysis for {word}.")
}

fn landed(rig: &Rig) -> bool {
    matches!(rig.runs(), Ok(runs) if runs.iter().any(|run| run.state == "done"))
}

fn replied(rig: &Rig, want: &str) -> bool {
    matches!(research_replies(rig, want), Ok(replies) if !replies.is_empty())
}

/// Drive one answer edge: finish the researcher job with `word`, tick until
/// the run lands, and check the need, the want reply and the alarms.
fn answered(word: &str) -> R {
    let (mut rig, want) = research_rig()?;
    rig.tick_until("research job", |rig| rig.open_job("research").is_some())?;
    let job = rig
        .open_job("research")
        .ok_or_else(|| VivariumError::Message("no research job".into()))?;
    rig.finish(
        &job.id,
        JobStatus::Finished,
        &agent_receipt(0, &envelope(word)),
    )?;
    rig.tick_until("run done", landed)?;
    rig.tick_until("want reply", |rig| replied(rig, &want))?;
    rig.tick()?;
    assert_eq!(rig.port.need_state(&rig.need)?, NeedStatus::Done);
    let replies = research_replies(&rig, &want)?;
    assert_eq!(replies.len(), 1, "{replies:?}");
    assert!(
        replies[0].contains(&format!("(answer {word})")),
        "{}",
        replies[0]
    );
    assert!(
        replies[0].contains(&format!("Analysis for {word}.")),
        "{}",
        replies[0]
    );
    assert_eq!(rig.exceptions(), "");
    Ok(())
}

#[test]
fn a_ready_answer_lands_the_run_and_replies_on_the_want() -> R {
    answered("READY")
}

#[test]
fn a_not_ready_answer_lands_the_run_and_replies_on_the_want() -> R {
    answered("NOT READY")
}

#[test]
fn a_needs_further_review_answer_lands_the_run_and_replies_on_the_want() -> R {
    answered("NEEDS FURTHER REVIEW")
}

#[test]
fn two_lost_jobs_fail_the_step_land_the_run_and_alarm_once() -> R {
    let (mut rig, want) = research_rig()?;
    for attempt in 1..=2 {
        rig.tick_until("research job", |rig| rig.open_job("research").is_some())?;
        let job = rig
            .open_job("research")
            .ok_or_else(|| VivariumError::Message(format!("no research job a{attempt}")))?;
        rig.finish(&job.id, JobStatus::Lost, "")?;
    }
    rig.tick_until("run done", landed)?;
    rig.tick_until("want reply", |rig| replied(rig, &want))?;
    rig.tick()?;
    assert_eq!(rig.port.need_state(&rig.need)?, NeedStatus::Done);
    let replies = research_replies(&rig, &want)?;
    assert_eq!(replies.len(), 1, "{replies:?}");
    assert!(replies[0].contains("failed"), "{}", replies[0]);
    assert!(
        replies[0].contains("expected a researcher envelope"),
        "{}",
        replies[0]
    );
    let alarms = rig
        .exceptions()
        .lines()
        .filter(|line| line.contains(RESEARCH_FAILED))
        .count();
    assert_eq!(alarms, 1, "{}", rig.exceptions());
    Ok(())
}
