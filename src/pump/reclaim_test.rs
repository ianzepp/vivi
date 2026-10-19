//! Tests for the pump reclaim worker.

use tempfile::tempdir;

use crate::pump::config::PumpConfig;
use crate::pump::host::test_ctx;
use crate::pump::reclaim::Reclaimer;
use crate::pump::state::Unit;

/// A busy request defers, then can proceed after the worker completion folds.
#[test]
fn a_busy_reclaimer_defers_a_second_request_until_folded() {
    let dir = tempdir().expect("tempdir");
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    let reclaimer = Reclaimer::detached();
    let u1 = Unit::default();
    let u2 = Unit::default();
    let deferred_count = || {
        host.logs()
            .iter()
            .filter(|line| *line == "work-lane clean deferred: reclaim still running")
            .count()
    };

    reclaimer.request(&ctx, &u1);
    reclaimer.request(&ctx, &u2);
    assert_eq!(deferred_count(), 1);

    reclaimer.serve(&ctx);
    reclaimer.request(&ctx, &u2);
    assert_eq!(deferred_count(), 1, "fold should clear busy after completion");
}
