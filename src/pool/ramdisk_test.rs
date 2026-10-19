use super::*;

#[test]
fn volume_name_joins_parts_under_the_prefix() {
    assert_eq!(
        volume_name(&["here", "slot3", "job1"]),
        "vivi-here-slot3-job1"
    );
}

#[test]
fn volume_name_sanitizes_slashes_and_whitespace() {
    assert_eq!(
        volume_name(&["here/fetch", "slot 3", "job\t1"]),
        "vivi-here-fetch-slot-3-job-1"
    );
}

#[test]
fn sectors_counts_512_byte_sectors_per_gib() {
    assert_eq!(sectors(0), 0);
    assert_eq!(sectors(1), 1024 * 1024 * 1024 / 512);
    assert_eq!(sectors(8), 8 * 1024 * 1024 * 1024 / 512);
    assert_eq!(sectors(32), 32 * 1024 * 1024 * 1024 / 512);
}

#[test]
fn admits_rejects_when_held_margin_and_want_exceed_free() {
    // 16 GiB free, 8 held + 8 wanted + 6 margin = 22 GiB needed: too big.
    assert!(!admits(16 * GIB, 8, 8, 6));
}

#[test]
fn admits_accepts_when_the_request_fits() {
    // 30 GiB free covers the same 22 GiB request.
    assert!(admits(30 * GIB, 8, 8, 6));
    // The exact boundary is admitted: need == free.
    assert!(admits(22 * GIB, 8, 8, 6));
    // A zero-size job with no held disks fits even at the margin boundary.
    assert!(admits(0, 0, 0, 0));
}

#[test]
fn enospc_matches_both_kernel_and_errno_forms() {
    assert!(enospc("hdiutil: attach failed - No space left on device"));
    assert!(enospc("build failed: os error 28"));
    assert!(!enospc("disk attached and erased"));
    assert!(!enospc("os error 13: permission denied"));
    assert!(!enospc(""));
}

#[test]
fn parse_mounts_keeps_only_vivi_volumes_and_returns_raw_devices() {
    let text = "\
/dev/disk5s1 on /Volumes/vivi-here-slot3-job1 (apfs, local, nodev, nosuid, journaled)
/dev/disk1s1s1 on / (apfs, local, journaled)
map auto_home on /System/Volumes/Data/home (autofs, automounted, nobrowse)
/dev/disk3s1 on /Volumes/TimeMachine (apfs, local)
";
    assert_eq!(
        parse_mounts(text),
        vec![(
            "/dev/disk5s1".to_string(),
            "/Volumes/vivi-here-slot3-job1".to_string()
        )]
    );
}

#[test]
fn parse_mounts_ignores_blank_and_malformed_lines() {
    assert!(parse_mounts("").is_empty());
    assert!(parse_mounts("a line with no mount marker\n").is_empty());
    assert!(parse_mounts("/dev/disk9s1 on /Volumes/vivi-x\n").is_empty());
}

#[test]
fn parse_df_gib_keeps_vivi_rows_with_1g_block_sizes() {
    let text = "\
Filesystem     1G-blocks Used Available Capacity iused ifree %iused  Mounted on
/dev/disk5s1          30    1        29     4%       2  9999    0%   /Volumes/vivi-here-slot3-job1
/dev/disk1s1s1       466  200       266    43%    1234  5678    0%   /
/dev/disk7s1           8    1         7     4%       2  9999    0%   /Volumes/vivi-here-slot4-job2
";
    assert_eq!(
        parse_df_gib(text),
        vec![
            ("/Volumes/vivi-here-slot3-job1".to_string(), 30),
            ("/Volumes/vivi-here-slot4-job2".to_string(), 8),
        ]
    );
    assert!(parse_df_gib("").is_empty());
}
