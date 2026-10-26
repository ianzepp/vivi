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
        "CREDENTIALS_DIRECTORY",
    ] {
        assert!(help.contains(name), "help is missing {name}");
    }
}
