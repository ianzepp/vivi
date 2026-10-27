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

Running with no arguments starts the server. Its home is $VIVI_HOME/server
($VIVI_HOME defaults to ~/.vivi). It reads one file, config.toml there; no
other environment variable is read.

Options:
  -h, --help     Print this help and exit
  -V, --version  Print the version and exit

config.toml (every key optional; relative paths are under the server home):
  bind                      Listen address (default 127.0.0.1)
  port                      Listen port (default 8080)
  send_rate_per_minute      Writes per minute per identity (default 30; 0 is
                            unlimited)
  hosted_default_identity   Identity an API key without its own acts as on a
                            hosted project (default operator)
  log                       Log filter (default vivi::server=info,tower_http=info)
  data_dir                  Data directory (default data)
  projects_root             Where host tools put project roots (default projects)
  lanes_root                Where host tools put lane repositories (default lanes)
  git_url_prefix            Git URL prefix for lanes (default file://<lanes_root>/)
  default_pool              Pool a new project is given

Secrets, one file each in creds/ under the server home:
  STORE_ENCRYPTION_KEY      Standard base64 of exactly 32 bytes. Must not change
                            for an existing data directory.
  MODERATION_TOKEN          Enables the /v1/_health endpoint
  HOST_OPERATOR_TOKEN       Built-in host principal on POST /v1/mcp
",
        version = version_line()
    )
}

#[cfg(test)]
#[path = "cli_test.rs"]
mod tests;
