//! Researcher step handler.
//!
//! The research step of the build and research flows hands the researcher the
//! need thread, plus the want a `for: <want>` line of the need body names. The
//! job is read-only; the researcher answers with its envelope.

use crate::error::VivariumError;
use crate::mailspace::shipped::RESEARCHER_ROLE;
use crate::pump::flow::def::{FlowKind, NodeClass};
use crate::pump::flow::handlers::{FlowMail, Registry};
use crate::pump::flow::index::StepRow;
use crate::pump::flow::outcome::{self, StartAction, StartCx, StepHandler};
use crate::pump::host::{Ctx, shell_quote};
use crate::pump::jobs::JobStore;

/// Largest share of the brief one thread section may take.
const SECTION_CAP: usize = 24 * 1024;
/// Seconds one `vivi` read may take.
const READ_TIMEOUT: u64 = 60;

/// The want a need body names with a `for:` line: the first such line,
/// trimmed, without surrounding `<>` or backticks.
pub(crate) fn for_want(body: &str) -> Option<String> {
    body.lines().find_map(|line| {
        let rest = line.trim().strip_prefix("for:")?;
        let want = rest
            .trim()
            .trim_matches(|c| matches!(c, '<' | '>' | '`'))
            .trim();
        (!want.is_empty()).then(|| want.to_owned())
    })
}

/// Starts the research step as a read-only researcher job.
struct ResearcherStart;

impl StepHandler for ResearcherStart {
    fn start(&self, cx: &StartCx<'_>, step: &StepRow) -> Result<StartAction, VivariumError> {
        let role = cx.ctx.routing.lookup(RESEARCHER_ROLE).ok_or_else(|| {
            VivariumError::Config(format!(
                "no `{RESEARCHER_ROLE}` role to run the research step"
            ))
        })?;
        let need = &cx.run.need;
        let body = cx.mail.task_body(need)?;
        let want = for_want(&body);
        let thread = section(
            cx.ctx,
            &format!("need show {} --newest 40", shell_quote(need)),
        );
        let want_thread = want
            .as_deref()
            .map(|want| section(cx.ctx, &format!("want show {}", shell_quote(want))));
        let brief = render(
            need,
            want.as_deref(),
            &body,
            &thread,
            want_thread.as_deref(),
        );
        let mut spec = outcome::role_job(cx.ctx, cx.run, step, "researcher", &role.name, &brief);
        spec.read_only = true;
        Ok(StartAction::Job(Box::new(spec)))
    }
}

/// The brief text of a research request.
fn render(
    need: &str,
    want: Option<&str>,
    body: &str,
    thread: &str,
    want_thread: Option<&str>,
) -> String {
    let mut parts = vec![format!("Research request for need {need}.")];
    if let Some(want) = want {
        parts.push(format!("for: {want}"));
    }
    parts.push(format!("NEED BODY:\n{}", body.trim()));
    parts.push(format!("NEED THREAD:\n{thread}"));
    if let Some(text) = want_thread {
        parts.push(format!("WANT THREAD:\n{text}"));
    }
    parts.push(String::from(
        "Answer with the researcher envelope: READY, NOT READY or NEEDS FURTHER REVIEW.",
    ));
    parts.join("\n\n")
}

/// The output of one `vivi` read, newest text kept under the cap; one notice
/// line when the read fails.
fn section(ctx: &Ctx, args: &str) -> String {
    let out = ctx.vivi(args, READ_TIMEOUT);
    if out.code != 0 {
        return format!("(`vivi {args}` could not be read for this brief; work from the rest.)");
    }
    tail_of(out.text.trim(), SECTION_CAP)
}

/// The last `max` bytes of `text`, cut forward to a char boundary, with a
/// note when something was cut.
fn tail_of(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!(
        "[older text cut: newest {} of {} bytes]\n{}",
        text.len() - start,
        text.len(),
        &text[start..]
    )
}

/// Registers this module's handlers.
pub(crate) fn register<S: JobStore, M: FlowMail>(registry: &mut Registry<S, M>) {
    registry.handler(
        FlowKind::Build,
        NodeClass::Researcher,
        Box::new(ResearcherStart),
    );
    registry.handler(
        FlowKind::Research,
        NodeClass::Researcher,
        Box::new(ResearcherStart),
    );
}

#[cfg(test)]
#[path = "research_test.rs"]
mod tests;
