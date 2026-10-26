//! Bounded runtime teardown for the server: the runtime that drives the
//! serving future is consumed explicitly when the future completes, so
//! unfinished blocking work cannot extend process lifetime.

use std::future::Future;
use std::time::Duration;

use crate::VivariumError;

/// Build the multi-thread runtime, drive `future` to completion on it,
/// and return its output. The runtime is then consumed with
/// [`shutdown_timeout`](tokio::runtime::Runtime::shutdown_timeout) at
/// zero: unfinished blocking work must not extend process lifetime.
/// Runtime-construction failures surface as [`VivariumError`].
pub(super) fn run<F>(future: F) -> Result<F::Output, VivariumError>
where
    F: Future,
{
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let output = runtime.block_on(future);
    runtime.shutdown_timeout(Duration::ZERO);
    Ok(output)
}

#[cfg(test)]
#[path = "shutdown_test.rs"]
mod tests;
