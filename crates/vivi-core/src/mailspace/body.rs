use std::fs;
use std::path::Path;

use vivi_error::VivariumError;

/// Read a body argument from a string, `-` for stdin, or `@path` for a file.
///
/// # Errors
/// Returns an error if the file cannot be read.
pub fn read_body_arg(value: &str) -> Result<String, VivariumError> {
    if value == "-" {
        return read_stdin_body();
    }
    if let Some(path) = value.strip_prefix('@') {
        read_body_file(Path::new(path))
    } else {
        Ok(value.to_string())
    }
}

/// Read a message body from an inline string or a file path.
///
/// # Errors
/// Returns an error if the file cannot be read or if neither `body` nor
/// `body_file` is provided.
pub fn read_body_input(
    body: Option<&str>,
    body_file: Option<&Path>,
) -> Result<String, VivariumError> {
    if let Some(path) = body_file {
        return read_body_file(path);
    }
    let Some(body) = body else {
        return Err(VivariumError::Message(
            "message body is required; pass --body or --body-file".into(),
        ));
    };
    read_body_arg(body)
}

fn read_body_file(path: &Path) -> Result<String, VivariumError> {
    fs::read_to_string(path).map_err(|e| {
        VivariumError::Message(format!("failed to read body file {}: {e}", path.display()))
    })
}

fn read_stdin_body() -> Result<String, VivariumError> {
    let mut body = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut body)?;
    Ok(body)
}
