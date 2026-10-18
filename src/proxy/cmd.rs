//! Run `key_cmd` / `auth_token_cmd` with `sh -c`. Never log stdout.

use std::process::Command;

use crate::VivariumError;

/// Execute a secret-producing shell command. Failures report stderr only.
///
/// # Errors
///
/// Returns a configuration error when the command cannot start, exits
/// nonzero, or prints only whitespace.
pub fn run_secret_cmd(label: &str, command: &str) -> Result<String, VivariumError> {
    let output = Command::new("sh")
        .arg("-c")
        .arg(command)
        .output()
        .map_err(|error| VivariumError::Config(format!("{label} failed to start: {error}")))?;
    if !output.status.success() {
        return Err(VivariumError::Config(format!(
            "{label} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let secret = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if secret.is_empty() {
        return Err(VivariumError::Config(format!(
            "{label} produced an empty secret"
        )));
    }
    Ok(secret)
}
