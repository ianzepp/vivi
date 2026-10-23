//! Starting a flow run lives in [`crate::pump::flow::advance::start`]: a run
//! start is a flow-node transition, and `advance` owns those. This module
//! only keeps the paths older callers import from.

pub(crate) use crate::pump::flow::advance::start::SENTINEL_NODE;
#[cfg(test)]
pub(crate) use crate::pump::flow::advance::start::start;

#[cfg(test)]
#[path = "run_test.rs"]
mod tests;
