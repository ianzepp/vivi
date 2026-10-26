#![cfg(unix)]
//! Real-process shutdown coverage: a fully isolated `vivi server run`
//! child (see `server_shutdown/support.rs`), readiness over a real socket,
//! and Unix signals sent with the `kill` utility.

#[path = "server_shutdown/support.rs"]
mod support;

use support::Server;

/// A separate idle child per shutdown signal exits normally, promptly, on
/// the first signal.
#[test]
fn idle_server_exits_promptly_on_shutdown_signal() {
    for signal in ["TERM", "INT"] {
        let mut server = Server::spawn_ready();
        let (status, elapsed) = server.signal_and_wait(signal);
        assert_eq!(
            status.code(),
            Some(0),
            "SIG{signal}: expected a normal exit ({status}):\n{}",
            server.output()
        );
        assert!(
            elapsed <= support::PROMPT_EXIT_CEILING,
            "SIG{signal}: exit took {elapsed:?}, over the {:?} ceiling",
            support::PROMPT_EXIT_CEILING
        );
    }
}
