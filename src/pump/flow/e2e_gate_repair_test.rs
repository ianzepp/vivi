//! End to end gate: the repair flow edges not driven elsewhere.
//!
//! Region: `fix1`, `rung1`, `merge1`, `fix2`, `rung2`, `merge2` and the
//! unrepaired wait. Edges covered: merge2 `merged`, `failed`, `conflict`;
//! rung2 lost once, `fail`, `failed`; merge1 and rung1 failures into `fix2`.
