//! Shared provider-rate-limit burst reporting.

use crate::pump::host::Ctx;
use crate::pump::state::State;

/// Suppress duplicate provider-rate-limit notices across board and split
/// planner exits in the same burst.
pub(crate) const RATE_LIMIT_EXCEPTION_WINDOW: u64 = 600;

/// Record one exception for a provider-rate-limit burst shared by split and
/// board planners. Individual units keep their retry details in the log.
pub(crate) fn rate_limit_burst(ctx: &Ctx, st: &mut State, uids: &[String]) {
    if uids.is_empty() {
        return;
    }
    let now = ctx.now();
    if st
        .rate_limited_at
        .is_some_and(|last| now.saturating_sub(last) < RATE_LIMIT_EXCEPTION_WINDOW)
    {
        return;
    }
    st.rate_limited_at = Some(now);
    ctx.exception(
        "provider-rate-limit",
        uids.first().map(String::as_str).unwrap_or("?"),
        &format!(
            "{} planner(s) hit the provider rate limit (429); retried with backoff, retry budgets untouched",
            uids.len()
        ),
    );
}
