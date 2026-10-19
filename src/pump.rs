//! `vivi pump`: the pump as a state machine; see skills/vivi/references/pump-contract.md.

pub mod alarm;
pub mod amend;
pub mod attempt_read;
pub mod attempt_record;
pub mod audit;
pub mod blockers;
pub mod board;
pub mod boardpass;
pub mod brief;
pub mod cadence;
pub mod closeout;
pub mod collect;
pub mod config;
pub mod control;
pub mod dispatch;
pub mod envfail;
pub mod gate;
pub mod gatefix;
pub mod host;
pub mod logview;
pub mod merge;
pub mod plan;
pub mod priority;
pub mod progress;
pub mod proof;
pub mod quarantine;
pub mod ramsweep;
pub mod ready;
pub mod reclaim;
pub mod remote;
pub mod replan;
pub mod report;
pub mod requeue;
pub mod run;
pub mod service;
pub mod state;
pub mod store;
pub mod transient;

#[cfg(test)]
#[path = "pump/logview_test.rs"]
mod logview_test;

#[cfg(test)]
#[path = "pump/config_test.rs"]
mod config_test;

#[cfg(test)]
#[path = "pump/config_ramdisk_test.rs"]
mod config_ramdisk_test;

#[cfg(test)]
#[path = "pump/control_test.rs"]
mod control_test;

#[cfg(test)]
#[path = "pump/state_test.rs"]
mod state_test;

#[cfg(test)]
#[path = "pump/store_test.rs"]
mod store_test;

#[cfg(test)]
#[path = "pump/transient_test.rs"]
mod transient_test;

#[cfg(test)]
#[path = "pump/report_test.rs"]
mod report_test;

#[cfg(test)]
#[path = "pump/replan_test.rs"]
mod replan_test;

#[cfg(test)]
#[path = "pump/amend_test.rs"]
mod amend_test;

#[cfg(test)]
#[path = "pump/plan_test.rs"]
mod plan_test;

#[cfg(test)]
#[path = "pump/priority_test.rs"]
mod priority_test;

#[cfg(test)]
#[path = "pump/ready_test.rs"]
mod ready_test;

#[cfg(test)]
#[path = "pump/brief_test.rs"]
mod brief_test;

#[cfg(test)]
#[path = "pump/host_test.rs"]
mod host_test;

#[cfg(test)]
#[path = "pump/collect_test.rs"]
mod collect_test;

#[cfg(test)]
#[path = "pump/dispatch_test.rs"]
mod dispatch_test;

#[cfg(test)]
#[path = "pump/merge_test.rs"]
mod merge_test;

#[cfg(test)]
#[path = "pump/merge_lane_test.rs"]
mod merge_lane_test;

#[cfg(test)]
#[path = "pump/merge_pull_test.rs"]
mod merge_pull_test;

#[cfg(test)]
#[path = "pump/merge_progress_test.rs"]
mod merge_progress_test;

#[cfg(test)]
#[path = "pump/remote_test.rs"]
mod remote_test;

#[cfg(test)]
#[path = "pump/gate_test.rs"]
mod gate_test;

#[cfg(test)]
#[path = "pump/gatefix_test.rs"]
mod gatefix_test;

#[cfg(test)]
#[path = "pump/gate_promote_test.rs"]
mod gate_promote_test;

#[cfg(test)]
#[path = "pump/board_test.rs"]
mod board_test;

#[cfg(test)]
#[path = "pump/boardpass_test.rs"]
mod boardpass_test;

#[cfg(test)]
#[path = "pump/run_test.rs"]
mod run_test;

#[cfg(test)]
#[path = "pump/lifecycle_test.rs"]
mod lifecycle_test;

#[cfg(test)]
#[path = "pump/blockers_test.rs"]
mod blockers_test;

#[cfg(test)]
#[path = "pump/alarm_test.rs"]
mod alarm_test;

#[cfg(test)]
#[path = "pump/quarantine_test.rs"]
mod quarantine_test;

#[cfg(test)]
#[path = "pump/requeue_test.rs"]
mod requeue_test;

#[cfg(test)]
#[path = "pump/attempt_record_test.rs"]
mod attempt_record_test;

#[cfg(test)]
#[path = "pump/attempt_read_test.rs"]
mod attempt_read_test;

#[cfg(test)]
#[path = "pump/envfail_test.rs"]
mod envfail_test;

#[cfg(test)]
#[path = "pump/proof_test.rs"]
mod proof_test;

#[cfg(test)]
#[path = "pump/audit_test.rs"]
mod audit_test;

#[cfg(test)]
#[path = "pump/ramsweep_test.rs"]
mod ramsweep_test;
