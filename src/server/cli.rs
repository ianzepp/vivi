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

Usage: vivi-server [OPTIONS]

Running with no arguments starts the server. It is configured only by the
environment variables below; there are no other options.

Options:
  -h, --help     Print this help and exit
  -V, --version  Print the version and exit

Environment:
  PORT                      Listen port (default 8080)
  BIND                      Listen address (default 0.0.0.0)
  DATA_DIR                  Data directory (default ./data)
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
  RUST_LOG                  Log filter (default vivi_server=info,tower_http=info)
",
        version = version_line()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_arguments_serve() {
        assert_eq!(parse(Vec::<&str>::new()), Ok(Invocation::Serve));
    }

    #[test]
    fn help_and_version_flags_are_recognised() {
        assert_eq!(parse(["--help"]), Ok(Invocation::Help));
        assert_eq!(parse(["-h"]), Ok(Invocation::Help));
        assert_eq!(parse(["--version"]), Ok(Invocation::Version));
        assert_eq!(parse(["-V"]), Ok(Invocation::Version));
    }

    #[test]
    fn help_wins_over_version_in_either_order() {
        assert_eq!(parse(["--version", "--help"]), Ok(Invocation::Help));
        assert_eq!(parse(["--help", "--version"]), Ok(Invocation::Help));
    }

    #[test]
    fn any_other_argument_is_an_error_not_a_boot() {
        assert_eq!(
            parse(["--halp"]),
            Err("unrecognized argument: --halp".to_owned())
        );
        assert!(parse(["serve"]).is_err());
        assert!(parse(["--version", "extra"]).is_err());
    }

    #[test]
    fn version_line_names_the_crate_version() {
        assert_eq!(
            version_line(),
            format!("vivi-server {}", env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn help_text_lists_every_variable_the_server_reads() {
        let help = help_text();
        for name in [
            "PORT",
            "BIND",
            "DATA_DIR",
            "STORE_ENCRYPTION_KEY",
            "MODERATION_TOKEN",
            "HOST_OPERATOR_TOKEN",
            "HOSTED_DEFAULT_IDENTITY",
            "SEND_RATE_PER_MINUTE",
            "HOST_PROJECTS_ROOT",
            "HOST_LANES_ROOT",
            "HOST_GIT_URL_PREFIX",
            "HOST_DEFAULT_POOL",
            "RUST_LOG",
        ] {
            assert!(help.contains(name), "help is missing {name}");
        }
    }
}
