//! Pool-dispatched merge jobs and per-need integration.
//!
//! Live execution is a durable Merge-role job. The pump records intent,
//! dispatches, and applies receipts. It does not merge, check, or push.

mod apply;
mod flaky;
mod need_branch_init;
mod need_cap;
mod need_dispatch;
mod need_receipts;
mod pool_dispatch;
mod select;

pub(crate) use need_dispatch::observed_ref_sha;
pub(crate) use need_dispatch::refresh_freezes as refresh_need_freezes;
pub(crate) use pool_dispatch::{
    apply_dispatched, dispatch_pool, target_write_blocked, timeout_status_line,
};
