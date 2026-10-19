//! Source contract for pump execution: durable work crosses the pool boundary,
//! and collectors apply the resulting receipts from the normal run loop.

const RUN: &str = include_str!("../src/pump/run.rs");
const COLLECT: &str = include_str!("../src/pump/collect.rs");
const UNIT_DISPATCH: &str = include_str!("../src/pump/dispatch.rs");
const PLANNER_JOBS: &str = include_str!("../src/pump/planner_jobs.rs");
const MERGE_POOL: &str = include_str!("../src/pump/merge/pool_dispatch.rs");
const MERGE_LANE: &str = include_str!("../src/pump/merge/lane.rs");

#[test]
fn production_run_uses_pool_dispatch_and_restarts_through_receipt_collectors() {
    assert!(RUN.contains("merge::dispatch_pool(ctx, st)"), "{RUN}");
    assert!(RUN.contains("jobs::dispatch_pending(ctx, st)"), "{RUN}");
    assert!(RUN.contains("collect::collect("), "{RUN}");
    assert!(
        COLLECT.contains("jobs::apply_reconcile(ctx, st"),
        "{COLLECT}"
    );
    assert!(
        COLLECT.contains("planner_jobs::apply_completions(ctx, st)"),
        "{COLLECT}"
    );
    assert!(RUN.contains("merge::apply_dispatched(ctx, st)"), "{RUN}");
    assert!(RUN.contains("gate::collect_completed(ctx, st)"), "{RUN}");
    assert!(RUN.contains("audit::collect_completed(ctx, st)"), "{RUN}");
    assert!(
        !RUN.contains("Merger::start"),
        "legacy merger started in run.rs"
    );
}

#[test]
fn hand_planner_and_merge_producers_dispatch_through_pool_jobs() {
    assert!(
        UNIT_DISPATCH.contains("pool dispatch --pool"),
        "{UNIT_DISPATCH}"
    );
    assert!(!UNIT_DISPATCH.contains("host.spawn"), "{UNIT_DISPATCH}");
    assert!(
        PLANNER_JOBS.contains("jobs::enqueue(st, spec)"),
        "{PLANNER_JOBS}"
    );
    assert!(!PLANNER_JOBS.contains(".spawn("), "{PLANNER_JOBS}");
    assert!(
        MERGE_POOL.contains("jobs::enqueue(st, spec)"),
        "{MERGE_POOL}"
    );
    assert!(!MERGE_POOL.contains("Merger::start"), "{MERGE_POOL}");
    assert!(!MERGE_POOL.contains(".spawn("), "{MERGE_POOL}");
    assert!(!MERGE_LANE.contains("Merger::start("), "{MERGE_LANE}");
}
