//! Tests for the pump reclaim worker.

use tempfile::tempdir;

use crate::pump::config::PumpConfig;
use crate::pump::host::test_ctx;
use crate::pump::reclaim::Reclaimer;
use crate::pump::state::Unit;

/// A second request while a reclaim is in flight defers exactly once, and
/// folding the landed reclaim clears `busy` so a later request goes through.
#[test]
fn a_busy_reclaimer_defers_a_second_request_until_folded() {
    let dir = tempdir().expect("tempdir");
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    let reclaimer = Reclaimer::detached();
    let deferred = || {
        host.logs()
            .iter()
            .filter(|line| {
                line.contains("work-lane clean deferred: reclaim still running")
            })
            .count()
    };

    reclaimer.request(&ctx, &Unit::default());
    assert_eq!(deferred(), 0, "first request must not defer");
    reclaimer.request(&ctx, &Unit::default());
    assert_eq!(
        deferred(),
        1,
        "second request while busy must defer exactly once"
    );

    reclaimer.serve(&ctx);
    reclaimer.request(&ctx, &Unit::default());
    assert_eq!(
        deferred(),
        1,
        "no new deferral: fold must have cleared busy"
    );
}
