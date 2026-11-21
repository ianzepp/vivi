use super::*;
use clap::CommandFactory;

/// clap checks a command tree only when it is built in a debug binary, and
/// then only for the subcommand being parsed: a clash in one subcommand
/// panics that subcommand alone. Build the whole tree here so the gate sees it.
#[test]
fn the_whole_command_tree_passes_claps_own_checks() {
    Cli::command().debug_assert();
}
