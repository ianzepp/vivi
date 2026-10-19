use super::*;
use std::path::PathBuf;
use std::sync::Mutex;

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

/// A `Runner` that records every argv it was given and answers from a canned
/// table keyed by the exact joined command line. An unlisted command fails,
/// so a test notices an argv it did not expect.
struct FakeRunner {
    calls: Mutex<Vec<Vec<String>>>,
    replies: Vec<(String, Result<String, String>)>,
}

impl FakeRunner {
    fn new(replies: &[(&str, Result<&str, &str>)]) -> Self {
        let replies = replies
            .iter()
            .map(|(line, reply)| {
                let reply = match *reply {
                    Ok(out) => Ok(out.to_string()),
                    Err(err) => Err(err.to_string()),
                };
                ((*line).to_string(), reply)
            })
            .collect();
        Self {
            calls: Mutex::new(Vec::new()),
            replies,
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls
            .lock()
            .expect("call log")
            .iter()
            .map(|argv| argv.join(" "))
            .collect()
    }
}

impl Runner for FakeRunner {
    fn run(&self, argv: &[&str], _timeout_secs: u64) -> Result<String, String> {
        let line = argv.join(" ");
        self.calls
            .lock()
            .expect("call log")
            .push(argv.iter().map(|arg| (*arg).to_string()).collect());
        match self.replies.iter().find(|(key, _)| *key == line) {
            Some((_, reply)) => reply.clone(),
            None => Err(format!("fake: unexpected `{line}`")),
        }
    }
}

/// A mount point under the system temp directory, removed by its owner.
fn mount_point(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("vivi-ramdisk-{tag}-{}", std::process::id()))
}

#[test]
fn whole_disk_strips_every_slice_suffix() {
    assert_eq!(whole_disk("/dev/disk7s1"), "/dev/disk7");
    assert_eq!(whole_disk("/dev/disk7"), "/dev/disk7");
    assert_eq!(whole_disk("/dev/disk5s1s1"), "/dev/disk5");
    assert_eq!(whole_disk("disk7s1"), "disk7");
    assert_eq!(whole_disk("/dev/disks1"), "/dev/disks1");
    assert_eq!(whole_disk(""), "");
}

#[test]
fn mount_runs_attach_erase_unmount_mount_in_order() {
    let at = mount_point("mount");
    let mount_line = format!("diskutil mount -mountPoint {} /dev/disk7s1", at.display());
    let erased = format!("diskutil eraseVolume APFS vivi-here-slot3-job1 /dev/disk7");
    let runner = FakeRunner::new(&[
        ("hdiutil attach -nomount ram://16777216", Ok("/dev/disk7\n")),
        (erased.as_str(), Ok("Started erase on disk7")),
        (
            "diskutil unmount /dev/disk7s1",
            Ok("Volume vivi-here-slot3-job1 on disk7s1 unmounted"),
        ),
        (
            mount_line.as_str(),
            Ok("/dev/disk7s1 on /Volumes/vivi-here-slot3-job1"),
        ),
    ]);

    let disk = mount(&runner, "vivi-here-slot3-job1", 8, &at).expect("mount");

    assert_eq!(
        disk,
        Disk {
            device: "/dev/disk7".to_string(),
            volume: "vivi-here-slot3-job1".to_string(),
            size_gib: 8,
            mount: at.clone(),
        }
    );
    assert!(at.is_dir(), "mount point was created");
    assert_eq!(
        runner.calls(),
        vec![
            "hdiutil attach -nomount ram://16777216".to_string(),
            erased,
            "diskutil unmount /dev/disk7s1".to_string(),
            mount_line,
        ]
    );
    let _ = std::fs::remove_dir_all(&at);
}

#[test]
fn mount_ejects_the_whole_disk_when_a_step_fails() {
    let at = mount_point("mount-fail");
    let runner = FakeRunner::new(&[
        ("hdiutil attach -nomount ram://2097152", Ok("/dev/disk9\n")),
        (
            "diskutil eraseVolume APFS vivi-x /dev/disk9",
            Err("eraseVolume: Volume name is not valid"),
        ),
        ("diskutil eject /dev/disk9", Ok("disk9 ejected")),
    ]);

    let error = mount(&runner, "vivi-x", 1, &at).expect_err("erase fails");

    assert!(error.contains("not valid"), "{error}");
    assert_eq!(
        runner.calls(),
        vec![
            "hdiutil attach -nomount ram://2097152".to_string(),
            "diskutil eraseVolume APFS vivi-x /dev/disk9".to_string(),
            "diskutil eject /dev/disk9".to_string(),
        ]
    );
    let _ = std::fs::remove_dir_all(&at);
}

#[test]
fn mount_refuses_a_device_that_attach_did_not_name() {
    let at = mount_point("mount-nodevice");
    let runner = FakeRunner::new(&[(
        "hdiutil attach -nomount ram://8388608",
        Ok("hdiutil: attach failed - no such file\n"),
    )]);

    let error = mount(&runner, "vivi-x", 4, &at).expect_err("no device");

    assert!(error.contains("returned no device"), "{error}");
    assert_eq!(
        runner.calls(),
        vec!["hdiutil attach -nomount ram://8388608".to_string()]
    );
    let _ = std::fs::remove_dir_all(&at);
}

#[test]
fn teardown_ejects_the_whole_disk_and_verifies_with_mount() {
    let runner = FakeRunner::new(&[
        ("diskutil eject /dev/disk7", Ok("disk7 ejected")),
        (
            "mount",
            Ok(
                "/dev/disk1s1s1 on / (apfs, local, journaled)\nmap auto_home on /System/Volumes/Data/home (autofs)\n",
            ),
        ),
    ]);
    let disk = Disk {
        device: "/dev/disk7s1".to_string(),
        volume: "vivi-x".to_string(),
        size_gib: 8,
        mount: PathBuf::from("/Volumes/vivi-x"),
    };

    assert_eq!(teardown(&runner, &disk), Ok(()));
    assert_eq!(
        runner.calls(),
        vec!["diskutil eject /dev/disk7".to_string(), "mount".to_string()]
    );
}

#[test]
fn teardown_reports_a_disk_that_survived_the_eject() {
    let disk = Disk {
        device: "/dev/disk7s1".to_string(),
        volume: "vivi-x".to_string(),
        size_gib: 8,
        mount: PathBuf::from("/Volumes/vivi-x"),
    };
    let by_device = FakeRunner::new(&[
        ("diskutil eject /dev/disk7", Ok("disk7 ejected")),
        (
            "mount",
            Ok("/dev/disk7s1 on /Volumes/vivi-x (apfs, local)\n"),
        ),
    ]);
    assert_eq!(
        teardown(&by_device, &disk).expect_err("still attached"),
        "vivi-x still attached after eject"
    );

    let by_volume = FakeRunner::new(&[
        ("diskutil eject /dev/disk7", Ok("disk7 ejected")),
        (
            "mount",
            Ok("/dev/disk12s1 on /Volumes/vivi-x (apfs, local)\n"),
        ),
    ]);
    assert_eq!(
        teardown(&by_volume, &disk).expect_err("volume still mounted"),
        "vivi-x still attached after eject"
    );
}

#[test]
fn teardown_surfaces_an_eject_error_when_the_disk_is_gone() {
    let runner = FakeRunner::new(&[
        ("diskutil eject /dev/disk7", Err("eject: Resource busy")),
        ("mount", Ok("")),
    ]);
    let disk = Disk {
        device: "/dev/disk7".to_string(),
        volume: "vivi-x".to_string(),
        size_gib: 8,
        mount: PathBuf::from("/Volumes/vivi-x"),
    };

    assert_eq!(
        teardown(&runner, &disk),
        Err("eject: Resource busy".to_string())
    );
}

#[test]
fn mounted_merges_mount_devices_with_df_sizes() {
    let runner = FakeRunner::new(&[
        (
            "mount",
            Ok(
                "/dev/disk5s1 on /Volumes/vivi-here-slot3-job1 (apfs, local)\n/dev/disk7s1 on /Volumes/vivi-here-slot4-job2 (apfs, local)\n/dev/disk9s1 on /Volumes/vivi-here-slot5-job3 (apfs, local)\n/dev/disk1s1s1 on / (apfs, local, journaled)\n",
            ),
        ),
        (
            "df -g",
            Ok(
                "Filesystem     1G-blocks Used Available Capacity Mounted on\n/dev/disk5s1          30    1        29     4%   /Volumes/vivi-here-slot3-job1\n/dev/disk7s1           8    1         7     4%   /Volumes/vivi-here-slot4-job2\n/dev/disk1s1s1       466  200       266    43%   /\n",
            ),
        ),
    ]);

    let disks = mounted(&runner);

    assert_eq!(
        disks,
        vec![
            Disk {
                device: "/dev/disk5s1".to_string(),
                volume: "vivi-here-slot3-job1".to_string(),
                size_gib: 30,
                mount: PathBuf::from("/Volumes/vivi-here-slot3-job1"),
            },
            Disk {
                device: "/dev/disk7s1".to_string(),
                volume: "vivi-here-slot4-job2".to_string(),
                size_gib: 8,
                mount: PathBuf::from("/Volumes/vivi-here-slot4-job2"),
            },
            Disk {
                device: "/dev/disk9s1".to_string(),
                volume: "vivi-here-slot5-job3".to_string(),
                size_gib: 0,
                mount: PathBuf::from("/Volumes/vivi-here-slot5-job3"),
            },
        ]
    );
    assert_eq!(
        runner.calls(),
        vec!["mount".to_string(), "df -g".to_string()]
    );
}

#[test]
fn mounted_reports_nothing_when_the_listings_fail() {
    let runner = FakeRunner::new(&[
        ("mount", Err("mount: command failed")),
        ("df -g", Err("df: command failed")),
    ]);

    assert!(mounted(&runner).is_empty());
}
