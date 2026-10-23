//! End to end gate: the fanout and verify pass 1 edges.
//!
//! Region: `build1`, `audit1`, `test1`, `join1`, `judge1`, the raise check and
//! `raise1`. Edges covered: build1 `failed` and `unchanged`; join1 `judge`,
//! `raised`, `block`, `failed`; judge1 `residual`, `block`, `failed`; the raise
//! check `none` and `raised`; raise1 `pass`, `fail`, `failed`.
