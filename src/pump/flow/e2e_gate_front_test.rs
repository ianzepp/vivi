//! End to end gate: the research and plan front of the build flow.
//!
//! Region: `research` and `plan`. Edges covered: research `READY`, `NOT READY`,
//! `NEEDS FURTHER REVIEW` and `failed`; plan `delivered`, `operator`, `hold`,
//! `design` and `failed`, including the park node and its restart at `plan`.
