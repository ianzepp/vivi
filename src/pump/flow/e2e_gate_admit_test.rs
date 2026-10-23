//! End to end gate: the admit, plan-revision and slice edges.
//!
//! Region: `admit1`, `plan2`, `admit2` and `slice`. Edges covered: admit1
//! `revise`, `escalate`, `failed`; plan2 `planned`, `delivered`, `operator`,
//! `hold`, `design`, `failed`; admit2 `admitted`, `revise`, `escalate`,
//! `failed`; slice `escalate` and `failed`.
