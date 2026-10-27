//! The vivi home: `$VIVI_HOME`, else `~/.vivi`. Each part keeps its files in
//! a directory under it (`server/`, `pump/`, ...). `VIVI_HOME` is the only
//! environment variable that moves these files.

use std::path::PathBuf;

/// `$VIVI_HOME`, else `~/.vivi`.
///
/// # Errors
/// Fails when neither is known.
pub fn vivi_home() -> Result<PathBuf, String> {
    match std::env::var_os("VIVI_HOME").filter(|v| !v.is_empty()) {
        Some(home) => Ok(PathBuf::from(home)),
        None => dirs::home_dir()
            .map(|home| home.join(".vivi"))
            .ok_or_else(|| "cannot find the home directory; set VIVI_HOME".into()),
    }
}

/// `$VIVI_HOME/server`.
///
/// # Errors
/// Fails when the home is unknown.
pub fn server_home() -> Result<PathBuf, String> {
    Ok(vivi_home()?.join("server"))
}
