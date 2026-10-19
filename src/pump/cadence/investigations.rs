//! Head-ceo investigation needs run through read-only auditor jobs.

use std::collections::BTreeMap;

use crate::error::VivariumError;
use crate::mailspace::{Mailspace, RoleView};
use crate::pump::host::Ctx;
use crate::pump::jobs::{self, JobSpec};
use crate::pump::state::State;

use super::{CadenceState, backoff_secs};

/// Default investigation rounds per want. The head-ceo charter may override it.
pub(super) const MAX_INVESTIGATION_ROUNDS: u32 = 2;

/// Pool retries for one investigation round before escalating.
const MAX_INVESTIGATION_ATTEMPTS: u32 = 2;

/// A marked head-ceo investigation dispatched to the auditor role.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub struct InvestigationRun {
    #[serde(default)]
    pub need: String,
    #[serde(default)]
    pub want: String,
    #[serde(default)]
    pub auditor: String,
    #[serde(default)]
    pub job: String,
    #[serde(default)]
    pub task: String,
    #[serde(default)]
    pub round: u32,
    #[serde(default)]
    pub attempt: u32,
    #[serde(default)]
    pub started_at: u64,
    #[serde(default)]
    pub ended_at: Option<u64>,
    #[serde(default)]
    pub outcome: Option<String>,
    #[serde(default)]
    pub retry_at: Option<u64>,
}

/// Record completed auditor jobs and close each source need after its report lands.
pub(super) fn complete(
    ctx: &Ctx,
    st: &State,
    mailspace: &Mailspace,
    cadence: &mut CadenceState,
    now: u64,
) {
    let needs: Vec<String> = cadence.investigations.keys().cloned().collect();
    for need in needs {
        complete_one(ctx, st, mailspace, cadence, &need, now);
    }
}

fn complete_one(
    ctx: &Ctx,
    st: &State,
    mailspace: &Mailspace,
    cadence: &mut CadenceState,
    need: &str,
    now: u64,
) {
    let Some(run) = cadence.investigations.get(need).cloned() else {
        return;
    };
    if run.ended_at.is_some() || run.job.is_empty() {
        return;
    }
    if let Some(task) = st.jobs.get(&run.job).and_then(|job| job.task.clone())
        && let Some(current) = cadence.investigations.get_mut(need)
    {
        current.task = task;
    }
    let Some(outcome) = jobs::completion(st, &run.job) else {
        return;
    };
    let state = outcome.state.clone();
    let agent_success = outcome.agent_success();
    if state == "finished" && agent_success == Some(true) {
        if report_landed(mailspace, &run) {
            match close_investigation_need(mailspace, &run.need, &run.want, &run.auditor) {
                Ok(()) => finish_run(cadence, need, now, "report-landed", None),
                Err(error) => {
                    let retry_at = now.saturating_add(backoff_secs(run.attempt));
                    finish_run(cadence, need, now, "close-failed", Some(retry_at));
                    ctx.exception(
                        "investigation-close-failed",
                        need,
                        &format!("report landed, but source need could not close: {error}"),
                    );
                }
            }
            return;
        }
    }
    let label = failure_label(&state, agent_success);
    let retry_at = now.saturating_add(backoff_secs(run.attempt));
    finish_run(cadence, need, now, label, Some(retry_at));
    ctx.exception(
        "investigation-report-missing",
        need,
        &format!("auditor job ended as {label}; the source need remains open"),
    );
}

fn finish_run(
    cadence: &mut CadenceState,
    need: &str,
    now: u64,
    outcome: &str,
    retry_at: Option<u64>,
) {
    if let Some(run) = cadence.investigations.get_mut(need) {
        run.ended_at = Some(now);
        run.outcome = Some(outcome.to_string());
        run.retry_at = retry_at;
    }
}

fn failure_label(state: &str, agent_success: Option<bool>) -> &'static str {
    match state {
        "timeout" => "timeout",
        "lost" => "lost",
        _ if agent_success == Some(false) => "failed",
        _ => "missing-report",
    }
}

/// Enqueue every marked investigation need once, deduplicated by want.
pub(super) fn prepare(
    ctx: &Ctx,
    st: &mut State,
    mailspace: &Mailspace,
    cadence: &mut CadenceState,
    now: u64,
) {
    let needs = match mailspace.list_kind_all(Some("head-ceo"), "sent", "need") {
        Ok(needs) => needs,
        Err(error) => {
            ctx.log(&format!("investigation scan failed: {error}"));
            return;
        }
    };
    let mut marked = Vec::new();
    for need in needs {
        if let Some((true, body)) = need_body(mailspace, &need.handle, ctx) {
            marked.push((need, body));
        }
    }
    if marked.is_empty() {
        return;
    }
    let Some(auditor) = auditor_role(mailspace) else {
        ctx.log("investigation dispatch skipped: no configured auditor role");
        return;
    };
    marked.sort_by_key(|(need, _)| {
        !cadence
            .investigations
            .get(&need.handle)
            .is_some_and(|run| run.ended_at.is_none() || run.retry_at.is_some_and(|at| at > now))
    });
    let max_rounds = round_limit(mailspace);
    let mut active_wants = BTreeMap::new();
    for (need, body) in marked {
        process_need(
            ctx,
            st,
            mailspace,
            cadence,
            need,
            body,
            &auditor,
            max_rounds,
            now,
            &mut active_wants,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn process_need(
    ctx: &Ctx,
    st: &mut State,
    mailspace: &Mailspace,
    cadence: &mut CadenceState,
    need: crate::storage::StoredMessageView,
    body: String,
    auditor: &RoleView,
    max_rounds: u32,
    now: u64,
    active_wants: &mut BTreeMap<String, String>,
) {
    let Some(want) = valid_want(mailspace, ctx, cadence, &need, &body, auditor, now) else {
        return;
    };
    if let Some(active) = active_wants.get(&want) {
        if active != &need.handle {
            close_duplicate(mailspace, ctx, &need.handle, active, &want);
            cadence.investigations.insert(
                need.handle.clone(),
                receipt(&need.handle, &want, &auditor.name, 0, now, "duplicate"),
            );
        }
        return;
    }
    let active = if let Some(previous) = cadence.investigations.get(&need.handle).cloned() {
        process_existing(
            ctx, st, mailspace, cadence, &need, &body, &want, auditor, previous, now,
        )
    } else {
        process_new(
            ctx, st, mailspace, cadence, &need, &body, &want, auditor, max_rounds, now,
        )
    };
    if active {
        active_wants.insert(want, need.handle);
    }
}

fn valid_want(
    mailspace: &Mailspace,
    ctx: &Ctx,
    cadence: &mut CadenceState,
    need: &crate::storage::StoredMessageView,
    body: &str,
    auditor: &RoleView,
    now: u64,
) -> Option<String> {
    let Some(want) = target(body) else {
        ctx.exception(
            "investigation-marker-invalid",
            &need.handle,
            "marked investigation need is missing `for: <want-handle>`",
        );
        cadence.investigations.insert(
            need.handle.clone(),
            receipt(&need.handle, "", &auditor.name, 0, now, "invalid-marker"),
        );
        return None;
    };
    if !open_want_exists(mailspace, &want) {
        ctx.exception(
            "investigation-marker-invalid",
            &need.handle,
            &format!("`for: {want}` does not name an open want"),
        );
        cadence.investigations.insert(
            need.handle.clone(),
            receipt(&need.handle, &want, &auditor.name, 0, now, "invalid-target"),
        );
        return None;
    }
    Some(want)
}

#[allow(clippy::too_many_arguments)]
fn process_existing(
    ctx: &Ctx,
    st: &mut State,
    mailspace: &Mailspace,
    cadence: &mut CadenceState,
    need: &crate::storage::StoredMessageView,
    body: &str,
    want: &str,
    auditor: &RoleView,
    previous: InvestigationRun,
    now: u64,
) -> bool {
    if previous.outcome.as_deref() == Some("close-failed") {
        return retry_close(ctx, mailspace, cadence, &need.handle, want, &previous, now);
    }
    if matches!(
        previous.outcome.as_deref(),
        Some(
            "report-landed"
                | "round-cap"
                | "duplicate"
                | "attempt-cap"
                | "invalid-marker"
                | "invalid-target"
        )
    ) || previous.ended_at.is_none()
        || previous.retry_at.is_some_and(|retry_at| retry_at > now)
    {
        return true;
    }
    if previous.attempt >= MAX_INVESTIGATION_ATTEMPTS {
        escalate(
            mailspace,
            ctx,
            &need.handle,
            want,
            previous.round,
            "auditor retry cap reached",
        );
        finish_run(cadence, &need.handle, now, "attempt-cap", None);
        return true;
    }
    enqueue(
        ctx,
        st,
        cadence,
        &need.handle,
        &need.subject,
        body,
        want,
        auditor,
        previous.round,
        previous.attempt.saturating_add(1).max(1),
        now,
    )
}

fn retry_close(
    ctx: &Ctx,
    mailspace: &Mailspace,
    cadence: &mut CadenceState,
    need: &str,
    want: &str,
    previous: &InvestigationRun,
    now: u64,
) -> bool {
    if !report_landed(mailspace, previous) {
        return true;
    }
    match close_investigation_need(mailspace, need, want, &previous.auditor) {
        Ok(()) => finish_run(cadence, need, now, "report-landed", None),
        Err(error) => {
            let retry_at = now.saturating_add(backoff_secs(previous.attempt));
            finish_run(cadence, need, now, "close-failed", Some(retry_at));
            ctx.exception(
                "investigation-close-failed",
                need,
                &format!("report landed, but source need could not close: {error}"),
            );
        }
    }
    true
}

#[allow(clippy::too_many_arguments)]
fn process_new(
    ctx: &Ctx,
    st: &mut State,
    mailspace: &Mailspace,
    cadence: &mut CadenceState,
    need: &crate::storage::StoredMessageView,
    body: &str,
    want: &str,
    auditor: &RoleView,
    max_rounds: u32,
    now: u64,
) -> bool {
    let round = cadence
        .investigations
        .values()
        .filter(|run| run.want == want)
        .map(|run| run.round)
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    if round > max_rounds {
        escalate(
            mailspace,
            ctx,
            &need.handle,
            want,
            round,
            "investigation round cap reached",
        );
        cadence.investigations.insert(
            need.handle.clone(),
            receipt(&need.handle, want, &auditor.name, round, now, "round-cap"),
        );
        return true;
    }
    enqueue(
        ctx,
        st,
        cadence,
        &need.handle,
        &need.subject,
        body,
        want,
        auditor,
        round,
        1,
        now,
    )
}

#[allow(clippy::too_many_arguments)]
fn enqueue(
    ctx: &Ctx,
    st: &mut State,
    cadence: &mut CadenceState,
    need: &str,
    subject: &str,
    source_body: &str,
    want: &str,
    auditor: &RoleView,
    round: u32,
    attempt: u32,
    now: u64,
) -> bool {
    let job = format!("investigate-{need}-r{round}-a{attempt}");
    let spec = investigation_spec(
        ctx,
        need,
        subject,
        source_body,
        want,
        auditor,
        round,
        attempt,
        &job,
    );
    if let Err(error) = jobs::enqueue(st, spec) {
        ctx.log(&format!("investigation enqueue {need} failed: {error}"));
        return false;
    }
    let task = st
        .jobs
        .get(&job)
        .and_then(|record| record.task.clone())
        .unwrap_or_default();
    cadence.investigations.insert(
        need.to_string(),
        InvestigationRun {
            need: need.to_string(),
            want: want.to_string(),
            auditor: auditor.name.clone(),
            job,
            task,
            round,
            attempt,
            started_at: now,
            ended_at: None,
            outcome: None,
            retry_at: None,
        },
    );
    true
}

fn receipt(
    need: &str,
    want: &str,
    auditor: &str,
    round: u32,
    now: u64,
    outcome: &str,
) -> InvestigationRun {
    InvestigationRun {
        need: need.to_string(),
        want: want.to_string(),
        auditor: auditor.to_string(),
        job: String::new(),
        task: String::new(),
        round,
        attempt: 0,
        started_at: now,
        ended_at: Some(now),
        outcome: Some(outcome.to_string()),
        retry_at: None,
    }
}

fn investigation_spec(
    ctx: &Ctx,
    need: &str,
    subject: &str,
    source_body: &str,
    want: &str,
    auditor: &RoleView,
    round: u32,
    attempt: u32,
    job: &str,
) -> JobSpec {
    let mut spec = JobSpec::new(
        job,
        "investigation",
        auditor.name.clone(),
        format!("investigate {want}: {subject}"),
        investigation_text(ctx, need, want, source_body, &auditor.name),
    );
    spec.budget = ctx.cfg.dispatch.budget;
    spec.read_only = true;
    spec.payload = serde_json::json!({
        "investigation": true,
        "need": need,
        "want": want,
        "round": round,
        "attempt": attempt,
    });
    spec
}

fn investigation_text(
    ctx: &Ctx,
    need: &str,
    want: &str,
    source_body: &str,
    auditor: &str,
) -> String {
    format!(
        "Read-only investigation for need `{need}`, linked to want `{want}`.\n\
         Review the need and want, research or triage the gap, and do not edit files.\n\
         Reply with findings on want `{want}` and to head-ceo. The need will close\n\
         after the report lands. Use `vivi mail reply {want} --from {auditor} --to\n\
         head-ceo --body <report> --project {}`.\n\
         Original need:\n{source_body}",
        ctx.project.display()
    )
}

fn need_body(mailspace: &Mailspace, handle: &str, ctx: &Ctx) -> Option<(bool, String)> {
    let thread = match mailspace.thread(handle, false, 1, 0) {
        Ok(thread) => thread,
        Err(error) => {
            ctx.log(&format!("investigation need {handle} unreadable: {error}"));
            return None;
        }
    };
    let need = thread.first()?;
    let marked = need
        .marks
        .iter()
        .any(|mark| mark.eq_ignore_ascii_case("investigate"));
    Some((marked, need.body.clone()))
}

pub(super) fn target(body: &str) -> Option<String> {
    for line in body.lines().map(str::trim) {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if key.trim().eq_ignore_ascii_case("for") {
            let clean = value
                .trim()
                .trim_matches(|ch| matches!(ch, '<' | '>' | '`'));
            if !clean.is_empty() {
                return Some(clean.to_string());
            }
        }
    }
    None
}

fn open_want_exists(mailspace: &Mailspace, handle: &str) -> bool {
    let Ok(wants) = mailspace.list_kind_all(None, "wants", "want") else {
        return false;
    };
    wants.iter().any(|want| want.handle == handle)
}

fn auditor_role(mailspace: &Mailspace) -> Option<RoleView> {
    let identity = mailspace
        .config
        .identities
        .iter()
        .find(|role| {
            role.kind
                .as_deref()
                .is_some_and(|kind| kind.eq_ignore_ascii_case("auditor"))
        })
        .or_else(|| {
            mailspace
                .config
                .identities
                .iter()
                .find(|role| role.name.eq_ignore_ascii_case("auditor"))
        })?;
    match mailspace.role_view(&identity.name) {
        Ok(role)
            if role.has_charter
                && !super::DISABLED_STATUSES
                    .iter()
                    .any(|status| role.status.trim().eq_ignore_ascii_case(status)) =>
        {
            Some(role)
        }
        _ => None,
    }
}

pub(super) fn round_limit(mailspace: &Mailspace) -> u32 {
    let Ok(role) = mailspace.role_view("head-ceo") else {
        return MAX_INVESTIGATION_ROUNDS;
    };
    for line in role.charter.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if !key.trim().eq_ignore_ascii_case("investigation-rounds") {
            continue;
        }
        if let Ok(rounds) = value.trim().parse::<u32>() {
            return rounds;
        }
    }
    MAX_INVESTIGATION_ROUNDS
}

pub(super) fn report_landed(mailspace: &Mailspace, run: &InvestigationRun) -> bool {
    let Ok(thread) = mailspace.thread(&run.want, false, usize::MAX, usize::MAX) else {
        return false;
    };
    let Some(root) = thread.iter().find(|message| message.handle == run.want) else {
        return false;
    };
    let root_content_id = root.content_id.as_str();
    let address = mailspace.address_for(&run.auditor).to_ascii_lowercase();
    let ceo_address = mailspace.address_for("head-ceo").to_ascii_lowercase();
    // Threads are sorted by date and content ID; same-second replies can sort
    // before their root. Select direct children by the explicit parent link.
    thread
        .iter()
        .filter(|message| message.parent_content_id.as_deref() == Some(root_content_id))
        .any(|message| {
            let date = match chrono::DateTime::parse_from_rfc3339(&message.date) {
                Ok(date) => date,
                Err(_) => return false,
            };
            date.timestamp() >= i64::try_from(run.started_at).unwrap_or(i64::MAX)
                && message.from.to_ascii_lowercase().contains(&address)
                && message.to.to_ascii_lowercase().contains(&ceo_address)
                && !message.body.trim().is_empty()
        })
}

fn close_investigation_need(
    mailspace: &Mailspace,
    need: &str,
    want: &str,
    auditor: &str,
) -> Result<(), VivariumError> {
    let note = format!("Investigation report from {auditor} landed on want {want}.");
    mailspace.move_item_outcome("head-ceo", need, "done", Some(&note), "need done", None)?;
    Ok(())
}

fn close_duplicate(mailspace: &Mailspace, ctx: &Ctx, duplicate: &str, active: &str, want: &str) {
    let note = format!("duplicate investigation; need {active} is already active for {want}");
    if let Err(error) = mailspace.move_item_outcome(
        "head-ceo",
        duplicate,
        "done",
        Some(&note),
        "need done",
        None,
    ) {
        ctx.exception(
            "investigation-duplicate",
            duplicate,
            &format!("duplicate need could not be closed: {error}"),
        );
    }
}

fn escalate(mailspace: &Mailspace, ctx: &Ctx, need: &str, want: &str, round: u32, reason: &str) {
    let note = format!(
        "Head-ceo investigation limit reached after round {round}: {reason}. Operator review is needed."
    );
    let recipients = if mailspace.resolve_identity("operator").is_ok() {
        vec![String::from("operator")]
    } else {
        Vec::new()
    };
    if let Err(error) = mailspace.reply(
        want,
        "head-ceo",
        recipients,
        Vec::new(),
        Some(format!("Investigation limit reached for {want}")),
        note.clone(),
    ) {
        ctx.exception(
            "investigation-escalation",
            want,
            &format!("investigation limit reached; reply failed: {error}"),
        );
    }
    if let Err(error) =
        mailspace.move_item_outcome("head-ceo", need, "done", Some(&note), "need done", None)
    {
        ctx.exception(
            "investigation-cap",
            need,
            &format!("investigation limit reached; need could not be closed: {error}"),
        );
    }
}

/// Read the inspection state as `status.txt` lines.
pub(super) fn status_lines(cadence: &CadenceState) -> Vec<String> {
    cadence
        .investigations
        .values()
        .map(|run| {
            format!(
                "investigation {} for {} round {} attempt {} {}",
                run.need,
                run.want,
                run.round,
                run.attempt,
                run.outcome.as_deref().unwrap_or("in-flight")
            )
        })
        .collect()
}
