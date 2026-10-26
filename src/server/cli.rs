//! The command line of the `vivi-server` binary.
//!
//! The server is configured by environment variables alone, so the command
//! line may only ask about the binary itself. Any other argument is an
//! error: a mistyped flag must never boot a second server.

/// What the command line asks the binary to do.
#[derive(Debug, PartialEq, Eq)]
pub enum Invocation {
    /// No arguments: read the environment and serve.
    Serve,
    /// `-h` / `--help`: print the usage text and exit.
    Help,
    /// `-V` / `--version`: print the version and exit.
    Version,
}

/// Read the arguments after the program name. `--help` wins over
/// `--version` when both appear.
///
/// # Errors
/// Returns the first argument that is neither a help nor a version flag.
pub fn parse<I, S>(args: I) -> Result<Invocation, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut found = Invocation::Serve;
    for arg in args {
        match arg.as_ref() {
            "-h" | "--help" => found = Invocation::Help,
            "-V" | "--version" if found != Invocation::Help => found = Invocation::Version,
            "-V" | "--version" => {}
            other => return Err(format!("unrecognized argument: {other}")),
        }
    }
    Ok(found)
}

/// The `--version` line.
#[must_use]
pub fn version_line() -> String {
    format!("vivi-server {}", env!("CARGO_PKG_VERSION"))
}

/// The `--help` text: what the binary is, its flags, and every environment
/// variable it reads.
#[must_use]
pub fn help_text() -> String {
    format!(
        "{version}
Public server hosting vivi project mailspaces and lightweight chat rooms.

Usage: vivi server run [OPTIONS]   (or: vivi-server [OPTIONS])

Running with no arguments starts the server. It is configured only by the
environment variables below; there are no other options.

Options:
  -h, --help     Print this help and exit
  -V, --version  Print the version and exit

Environment:
  PORT                      Listen port (default 8080)
  BIND                      Listen address (default 127.0.0.1)
  DATA_DIR                  Data directory: required, an absolute path
  STORE_ENCRYPTION_KEY      Standard base64 of exactly 32 bytes. Must not change
                            for an existing data directory.
  MODERATION_TOKEN          Enables the /v1/_health endpoint
  HOST_OPERATOR_TOKEN       Built-in host principal on POST /v1/mcp
  HOSTED_DEFAULT_IDENTITY   Identity an API key without its own acts as on a
                            hosted project (default operator)
  SEND_RATE_PER_MINUTE      Writes per minute per identity (default 30; 0 is
                            unlimited)
  HOST_PROJECTS_ROOT        Where host tools put project roots
  HOST_LANES_ROOT           Where host tools put lane repositories
  HOST_GIT_URL_PREFIX       Git URL prefix for lanes
  HOST_DEFAULT_POOL         Pool a new project is given
  RUST_LOG                  Log filter (default vivi::server=info,tower_http=info)
  CREDENTIALS_DIRECTORY     Set by systemd. STORE_ENCRYPTION_KEY, MODERATION_TOKEN and
                            HOST_OPERATOR_TOKEN are read from a file of that name here
                            (LoadCredential=NAME:/path) before the environment, which
                            keeps them out of the process environment.
",
        version = version_line()
    )
}

#[cfg(test)]
#[path = "cli_test.rs"]
mod tests;
