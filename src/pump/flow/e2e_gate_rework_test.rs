//! End to end gate: the rework and verify pass 2 edges.
//!
//! Region: `rework1`, `build2`, `audit2`, `test2`, `join2`, `judge2`, the raise
//! check and `raise2`. Edges covered: rework1 `sliced`, `escalate`, `failed`;
//! build2 `built`, `failed`, `unchanged`; join2 `verified`, `judge`, `block`,
//! `failed`, `raised`; judge2 and raise2 outcomes; a full rework pass to done.
