//! `vivi want show`: the thread plus the want's computed state.

use std::path::Path;

use vivi::VivariumError;
use vivi::mailspace::{Mailspace, WantStateReport};

/// Show a want. `--latest` keeps the plain thread output; text adds the
/// state block after the thread; `--json` wraps the thread and the state.
pub(crate) fn show_want(
    handle: &str,
    json: bool,
    latest: bool,
    project: Option<&Path>,
) -> Result<(), VivariumError> {
    let mailspace = Mailspace::discover(project)?;
    if latest {
        return vivi::mailspace::print_thread(&mailspace, handle, false, 50, 50, json, true);
    }
    if json {
        return print_json(&mailspace, handle);
    }
    vivi::mailspace::print_thread_outline(&mailspace.thread_outline(handle)?);
    vivi::mailspace::print_thread(&mailspace, handle, false, 50, 50, false, false)?;
    print_state(&mailspace.want_state(handle)?);
    Ok(())
}

fn print_json(mailspace: &Mailspace, handle: &str) -> Result<(), VivariumError> {
    let thread = mailspace.thread(handle, false, 50, 50)?;
    let report = mailspace.want_state(handle)?;
    let value = serde_json::json!({ "thread": thread, "want_state": report });
    let rendered = serde_json::to_string_pretty(&value)
        .map_err(|e| VivariumError::Other(format!("failed to encode want JSON: {e}")))?;
    println!("{rendered}");
    Ok(())
}

fn print_state(report: &WantStateReport) {
    println!("  state    {}", report.summary());
    for need in &report.serving {
        let verdict = need
            .verdict
            .as_deref()
            .map(|v| format!(" ({v})"))
            .unwrap_or_default();
        println!(
            "  serves   {}  {}{}  {}",
            need.handle,
            need.state().as_str(),
            verdict,
            need.subject
        );
    }
}
