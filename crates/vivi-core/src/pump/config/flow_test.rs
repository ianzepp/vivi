//! Tests for [`crate::pump::config::FlowConfig`].

use crate::pump::config::PumpConfig;

fn pump(text: &str) -> Result<PumpConfig, toml::de::Error> {
    toml::from_str(text)
}

#[test]
fn max_live_runs_parses() {
    let cfg = pump("[flow]\nmax_live_runs = 3\n").expect("parses");
    assert_eq!(cfg.flow.max_live_runs, Some(3));
}

#[test]
fn no_table_means_no_cap() {
    let cfg = pump("").expect("parses");
    assert_eq!(cfg.flow.max_live_runs, None);
    assert!(cfg.flow.is_empty());
}

#[test]
fn unknown_key_is_refused() {
    assert!(pump("[flow]\nmax_live = 3\n").is_err());
}

#[test]
fn slice_minutes_default_is_ten() {
    let cfg = pump("").expect("parses");
    assert_eq!(cfg.slice.minutes(), 10);
    assert!(cfg.slice.is_empty());
}

#[test]
fn slice_minutes_parses() {
    let cfg = pump("[slice]\nminutes = 6\n").expect("parses");
    assert_eq!(cfg.slice.minutes(), 6);
    assert!(!cfg.slice.is_empty());
}

#[test]
fn slice_unknown_key_is_refused() {
    assert!(pump("[slice]\nminute = 6\n").is_err());
}

#[test]
fn stages_default_to_on() {
    let cfg = pump("").expect("parses");
    for stage in ["admit", "audit", "judge", "need_rung"] {
        assert!(cfg.flow.stages.runs(stage), "{stage}");
    }
    assert!(cfg.flow.is_empty());
}

#[test]
fn a_stage_switched_off_parses_and_leaves_the_others_on() {
    let cfg = pump("[flow.stages]\naudit = false\njudge = true\n").expect("parses");
    assert!(!cfg.flow.stages.runs("audit"));
    assert!(cfg.flow.stages.runs("judge"));
    assert!(cfg.flow.stages.runs("admit"));
    assert!(!cfg.flow.is_empty());
}

#[test]
fn an_unknown_stage_is_refused() {
    assert!(pump("[flow.stages]\nplanner = false\n").is_err());
}
