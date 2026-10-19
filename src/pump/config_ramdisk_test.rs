use super::PumpRamDisk;
use crate::pump::config::PumpConfig;
use std::path::Path;

#[test]
fn rejects_lane_sizes_outside_the_supported_range() {
    for gib in [0, 1025] {
        let cfg = PumpConfig {
            ramdisk: PumpRamDisk { lane_gib: Some(gib), ..PumpRamDisk::default() },
            ..PumpConfig::default()
        };
        let error = cfg.validate(Path::new("."), &[]).unwrap_err();
        assert!(error.to_string().contains("[pump.ramdisk] lane_gib"), "{error}");
    }
}

#[test]
fn accepts_lane_size_boundaries_and_disabled_lanes() {
    for gib in [None, Some(1), Some(1024)] {
        let cfg = PumpConfig {
            ramdisk: PumpRamDisk { lane_gib: gib, ..PumpRamDisk::default() },
            ..PumpConfig::default()
        };
        let error = cfg.validate(Path::new("."), &[]).unwrap_err();
        assert!(!error.to_string().contains("[pump.ramdisk]"), "{error}");
    }
}
