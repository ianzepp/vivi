//! Tests for the `[pump.ramdisk]` table of [`crate::pump::config`].
//!
//! Wired from `config.rs` as the module `ramdisk`, so these tests read as
//! `pump::config::ramdisk::*`.

use crate::mailspace::MailspaceConfig;
use crate::pump::config::PumpConfig;
use std::path::Path;

/// The Cargo environment pairs a lane exports by default.
const CARGO_ENV: [(&str, &str); 5] = [
    ("CARGO_INCREMENTAL", "0"),
    ("CARGO_PROFILE_DEV_DEBUG", "0"),
    ("CARGO_PROFILE_DEV_STRIP", "symbols"),
    ("CARGO_PROFILE_TEST_DEBUG", "0"),
    ("CARGO_PROFILE_TEST_STRIP", "symbols"),
];

/// Parse a mailspace file's `[pump]` table.
fn pump(text: &str) -> PumpConfig {
    let config: MailspaceConfig = toml::from_str(text).expect("mailspace file text should parse");
    config.pump.expect("pump table should be present")
}

/// A `[pump]` file that validates, with `ramdisk` text appended.
fn valid_with(ramdisk: &str) -> PumpConfig {
    pump(&format!(
        "name = \"t\"\n\
         [pump]\n\
         committer_name = \"Pump Tester\"\n\
         committer_email = \"pump@example.com\"\n\
         branch = \"next\"\n\
         promote = false\n\
         [pump.board]\n\
         mark = \"\"\n\
         [pump.dispatch]\n\
         lanes = [{{ model = \"m\", cap = 1 }}]\n\
         [pump.gate]\n\
         command = \"./gate\"\n\
         [pump.ramdisk]\n{ramdisk}"
    ))
}

#[test]
fn ramdisk_defaults_leave_lanes_on_disk_and_carry_the_cargo_pairs() {
    let cfg = pump("name = \"t\"\n[pump]\nbranch = \"next\"\n");
    assert_eq!(cfg.ramdisk.lane_gib, None, "lanes are off by default");
    assert_eq!(cfg.ramdisk.margin_gib, 6);
    let env: Vec<(&str, &str)> = cfg
        .ramdisk
        .env
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    assert_eq!(env, CARGO_ENV);
}

#[test]
fn ramdisk_parses_lane_gib_and_margin_and_keeps_the_default_env() {
    let cfg = valid_with("lane_gib = 8\nmargin_gib = 10\n");
    assert_eq!(cfg.ramdisk.lane_gib, Some(8));
    assert_eq!(cfg.ramdisk.margin_gib, 10);
    assert_eq!(cfg.ramdisk.env.len(), CARGO_ENV.len());
    assert!(cfg.validate(Path::new("."), &[]).is_ok(), "{:?}", cfg.validate(Path::new("."), &[]));
}

#[test]
fn ramdisk_keeps_an_explicit_env_and_drops_the_default_pairs() {
    let cfg = valid_with("lane_gib = 8\nenv = { CARGO_INCREMENTAL = \"1\" }\n");
    assert_eq!(cfg.ramdisk.env.len(), 1);
    assert_eq!(
        cfg.ramdisk.env.get("CARGO_INCREMENTAL").map(String::as_str),
        Some("1")
    );
    assert!(
        cfg.validate(Path::new("."), &[]).is_ok(),
        "{:?}",
        cfg.validate(Path::new("."), &[])
    );
}

#[test]
fn ramdisk_validate_accepts_lane_gib_at_both_bounds() {
    for gib in [1, 1024] {
        let cfg = valid_with(&format!("lane_gib = {gib}\n"));
        assert!(
            cfg.validate(Path::new("."), &[]).is_ok(),
            "lane_gib = {gib} must validate: {:?}", cfg.validate(Path::new("."), &[])
        );
    }
}

#[test]
fn ramdisk_validate_rejects_lane_gib_zero_and_over_1024() {
    for gib in [0, 1025] {
        let cfg = valid_with(&format!("lane_gib = {gib}\n"));
        let said = cfg
            .validate(Path::new("."), &[])
            .expect_err("an out-of-range lane_gib must fail validation")
            .to_string();
        assert!(said.contains("[pump.ramdisk] lane_gib"), "{said}");
        assert!(said.contains(&gib.to_string()), "{said}");
    }
}
