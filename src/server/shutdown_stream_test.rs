//! Reserved for the independent stream-drain tests: per-connection drain
//! behavior over real sockets (an idle keep-alive connection, a slow or
//! stalled body, a half-closed socket) during the signal-relative drain
//! window. This slice adds no tests here; the child-module declaration
//! lives at the top of [`super`] (the shutdown test entry).
