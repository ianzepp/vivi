//! Delivery of decision notices to the Mind from the tick.
//!
//! The [`DecisionNotices`] phase inventories the actionable waits and posts
//! one ordinary mail reply per wait on the waiting step's own thread: sent
//! as the pump's posting identity, addressed explicitly to the decision
//! seat, with no operator copy. The notice carries the request identity, the
//! handles, the newest issue, the reason, the evidence and the control
//! choices, so the seat can judge without reading the pump store.
//!
//! Mail is the durable delivery receipt. Before sending, the phase looks for
//! an earlier pump-authored letter on the same thread that is actually
//! addressed to the seat and carries the same request identity — author and
//! addressed recipient, never body text alone. A failed send is recorded
//! nowhere, so the next tick retries it. A missing or invalid seat raises
//! [`DECISION_UNROUTABLE`] once and leaves the work waiting: there is no
//! fallback recipient. Nothing else moves — no task is minted or bound, no
//! work is closed, no state changes.

use crate::error::VivariumError;
use crate::mailspace::{Mailspace, MailspaceThreadMessage};
use crate::pump::flow::decision::{Wait, inventory, recipient};
use crate::pump::flow::handlers::{FlowMail, Registry, TickPhase};
use crate::pump::flow::tick::raise_once;
use crate::pump::host::Ctx;
use crate::pump::jobs::JobStore;
use crate::pump::record::open_mailspace;
use crate::pump::store::PumpStore;

/// Exception kind: the decision seat is missing or invalid, so no notice can
/// be routed. The waits keep waiting; nothing falls back to the operator.
pub const DECISION_UNROUTABLE: &str = "flow-decision-unroutable";

/// The body line that carries a notice's stable request identity.
pub(super) const REQUEST_MARK: &str = "decision request: ";

/// The tick phase: deliver one decision notice per actionable wait.
struct DecisionNotices;

impl<S: JobStore, M: FlowMail> TickPhase<S, M> for DecisionNotices {
    fn name(&self) -> &'static str {
        "decision"
    }

    fn run(
        &self,
        ctx: &Ctx,
        _st: &mut S,
        store: &PumpStore,
        _mail: &mut M,
    ) -> Result<(), VivariumError> {
        deliver(ctx, store)
    }
}

/// Registry hook: the decision phase, after the recovery and build-front
/// phases, so a notice describes a wait the tick could not clear.
pub(crate) fn register<S: JobStore, M: FlowMail>(registry: &mut Registry<S, M>) {
    registry.phase(Box::new(DecisionNotices));
}

/// Deliver a notice for every actionable wait over the project's mailspace.
///
/// # Errors
/// Returns the first failed send after every wait has had its attempt; the
/// tick's boundary records it and the next tick retries.
pub(crate) fn deliver(ctx: &Ctx, store: &PumpStore) -> Result<(), VivariumError> {
    let Some(mailspace) = open_mailspace(ctx) else {
        return Ok(());
    };
    deliver_to(&mailspace, ctx, store)
}

/// Deliver over an explicit mailspace.
///
/// # Errors
/// As [`deliver`]; a missing or invalid seat raises the unroutable
/// diagnostic and returns `Ok`, leaving the work waiting.
pub(crate) fn deliver_to(
    mailspace: &Mailspace,
    ctx: &Ctx,
    store: &PumpStore,
) -> Result<(), VivariumError> {
    let seat = match recipient(mailspace, &ctx.cfg) {
        Ok(seat) => seat,
        Err(error) => {
            raise_unroutable(ctx, &error.to_string());
            return Ok(());
        }
    };
    let from = ctx.cfg.reply_from.trim();
    let from_address = mailspace.address_for(from);
    let seat_address = mailspace.address_for(&seat);
    let mut failure = None;
    for wait in inventory(store)? {
        if let Err(error) = deliver_one(mailspace, ctx, from, &from_address, &seat_address, &wait) {
            ctx.log(&format!("decision: {} undelivered: {error}", wait.id));
            failure.get_or_insert(error);
        }
    }
    match failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// Raise the unroutable-seat diagnostic once; the waits keep waiting.
fn raise_unroutable(ctx: &Ctx, why: &str) {
    let configured = ctx.cfg.decision_to.trim();
    let unit = if configured.is_empty() {
        "decision"
    } else {
        configured
    };
    raise_once(
        ctx,
        DECISION_UNROUTABLE,
        unit,
        &format!("decision seat unroutable: {why}; no notice was sent and the waits keep waiting"),
    );
}

/// Deliver one wait's notice: skip a delivered request, else reply on the
/// step's thread to the seat and no one else.
fn deliver_one(
    mailspace: &Mailspace,
    ctx: &Ctx,
    from: &str,
    from_address: &str,
    seat_address: &str,
    wait: &Wait,
) -> Result<(), VivariumError> {
    let delivered = mailspace
        .thread(&wait.task, false, usize::MAX, usize::MAX)
        .is_ok_and(|thread| delivered(&thread, from_address, seat_address, &wait.id));
    if delivered {
        return Ok(());
    }
    mailspace.reply(
        &wait.task,
        from,
        vec![seat_address.to_owned()],
        Vec::new(),
        None,
        notice_body(wait),
    )?;
    ctx.log(&format!(
        "decision: delivered {} to the seat on task {}",
        wait.id, wait.task
    ));
    Ok(())
}

/// Whether `thread` already holds the receipt of `request`: one message the
/// pump authored, actually addressed to `seat`, carrying the request
/// identity. Body text alone proves nothing — a quoted line or a letter
/// aimed elsewhere is not a delivery.
fn delivered(
    thread: &[MailspaceThreadMessage],
    from_address: &str,
    seat_address: &str,
    request: &str,
) -> bool {
    thread.iter().any(|message| {
        message.from.trim().eq_ignore_ascii_case(from_address)
            && message
                .to
                .split(", ")
                .any(|who| who.trim().eq_ignore_ascii_case(seat_address))
            && message.body.contains(&format!("{REQUEST_MARK}{request}"))
    })
}

/// The notice body: request identity, handles, issue, reason, evidence and
/// control choices.
fn notice_body(wait: &Wait) -> String {
    let attempt = wait.job.as_ref().map_or(0, |row| row.attempt);
    let mut body = format!(
        "{REQUEST_MARK}{id}\n\nThe {kind} wait holds: {need} r{run} at `{node}` \
         (flow {flow}, task {task}, attempt a{attempt}).\n",
        id = wait.id,
        kind = wait.kind.name(),
        need = wait.need,
        run = wait.run,
        node = wait.node,
        flow = wait.flow,
        task = wait.task,
        attempt = attempt,
    );
    body.push_str(&match &wait.issue {
        Some(issue) => format!("issue: {issue}\n"),
        None => String::from("issue: none raised\n"),
    });
    body.push_str(&format!("reason: {}\n", wait.reason));
    body.push_str("\nevidence:\n");
    for line in &wait.evidence {
        body.push_str(&format!("- {line}\n"));
    }
    body.push_str("\ncontrol choices:\n");
    for choice in &wait.controls {
        body.push_str(&format!("- {choice}\n"));
    }
    body
}
