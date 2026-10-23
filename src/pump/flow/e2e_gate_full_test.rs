//! End to end gate: one build need from intake to done, over a real mailspace.
//!
//! Region: the whole build flow, `plan` -> `admit1` -> `slice` -> the slice
//! hands and their slice merges -> `build1` -> `audit1` and `test1` -> `join1`
//! -> `merge` -> done. Edges covered: `planned`, `admitted`, `sliced`, `built`,
//! `verified`, `merged`, with the need and its pump record closed by the pump.
