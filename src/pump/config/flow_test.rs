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
