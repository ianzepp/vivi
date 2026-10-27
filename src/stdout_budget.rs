//! Shared stdout size budget for agent-facing CLI output.

use serde::Serialize;

use vivi_error::VivariumError;

/// Refuse unconfirmed stdout larger than this many bytes.
pub const MAX_STDOUT_BYTES: usize = 64 * 1024;

/// Error when `rendered_len` exceeds [`MAX_STDOUT_BYTES`] without confirmation.
///
/// # Errors
/// Returns [`VivariumError::Message`] describing the limit and recovery flags.
pub fn enforce_stdout_bytes(
    label: &str,
    rendered_len: usize,
    confirm_large: bool,
    mermaid_hint: Option<&str>,
) -> Result<(), VivariumError> {
    if confirm_large || rendered_len <= MAX_STDOUT_BYTES {
        return Ok(());
    }
    let mermaid = match mermaid_hint {
        None => String::new(),
        Some("") => " For graph topology, prefer Mermaid instead of JSON: \
             `vivi graph show <code>` (add --include-state for readiness classes). \
             For status loops use `vivi graph ready`."
            .to_string(),
        Some(code) => format!(
            " For graph topology, prefer Mermaid instead of JSON: \
             `vivi graph show {code}` (add --include-state for readiness classes). \
             For status loops use `vivi graph ready {code}`."
        ),
    };
    Err(VivariumError::Message(format!(
        "{label} is {rendered_len} bytes; refusing large stdout. \
         Pass --confirm-large if you really want the full result on stdout.{mermaid}"
    )))
}

/// Pretty-print JSON to stdout, enforcing the large-output budget.
///
/// Always includes a Mermaid topology hint in the large-output refusal (graph
/// control-plane JSON should not replace `graph show`).
///
/// # Errors
/// Returns encode errors or the large-stdout refusal.
pub fn print_pretty_json<T: Serialize + ?Sized>(
    label: &str,
    value: &T,
    confirm_large: bool,
    mermaid_hint: Option<&str>,
) -> Result<(), VivariumError> {
    let rendered = serde_json::to_string_pretty(value)
        .map_err(|e| VivariumError::Other(format!("failed to encode JSON: {e}")))?;
    // Graph JSON printers always want a Mermaid redirect in the refusal.
    let hint = mermaid_hint.or(Some(""));
    enforce_stdout_bytes(label, rendered.len(), confirm_large, hint)?;
    println!("{rendered}");
    Ok(())
}
