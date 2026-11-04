//! Deterministic reports over synced attempt and need-lifecycle records.

pub mod data;
pub mod format;
pub mod model;
pub mod views;

#[cfg(test)]
#[path = "report_test.rs"]
mod tests;
