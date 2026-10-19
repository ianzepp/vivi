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
    plutil_index: Mutex<usize>,
    plist_request: Mutex<Option<String>>,
    plist_device: Mutex<Option<String>>,
    listed_volume: Mutex<Option<String>>,
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
            plutil_index: Mutex::new(0),
            plist_request: Mutex::new(None),
            plist_device: Mutex::new(None),
            listed_volume: Mutex::new(None),
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

fn disk_info_json(device: &str, container: &str) -> String {
    serde_json::json!({
        "DeviceIdentifier": device,
        "APFSContainerReference": container,
    })
    .to_string()
}

fn apfs_list_json(name: &str, backing: &str, logical: &str) -> String {
    serde_json::json!({
        "Containers": [{
            "ContainerReference": "disk9",
            "PhysicalStores": [{"DeviceIdentifier": backing}],
            "Volumes": [{
                "Name": name,
                "DeviceIdentifier": logical,
            }],
        }],
    })
    .to_string()
}

fn volume_info_json(device: &str, container: &str, mount_point: &str) -> String {
    serde_json::json!({
        "DeviceIdentifier": device,
        "APFSContainerReference": container,
        "MountPoint": mount_point,
    })
    .to_string()
}

impl Runner for FakeRunner {
    fn run(&self, argv: &[&str], _timeout_secs: u64) -> Result<String, String> {
        let line = argv.join(" ");
        self.calls
            .lock()
            .expect("call log")
            .push(if argv.first() == Some(&"/usr/bin/plutil") {
                vec![format!("{} <plist>", argv[..5].join(" "))]
            } else {
                argv.iter().map(|arg| (*arg).to_string()).collect()
            });
        if argv.len() == 4 && argv[0] == "diskutil" && argv[1] == "info" {
            *self.plist_request.lock().expect("plist request") = Some(argv[3].to_string());
            *self.plist_device.lock().expect("plist device") = Some(argv[3].to_string());
        } else if argv.len() == 5 && argv[0] == "diskutil" && argv[1] == "apfs" && argv[2] == "list"
        {
            *self.plist_request.lock().expect("plist request") =
                Some(format!("container:{}", argv[4]));
        } else if argv.first() == Some(&"mount") {
            if let Some((_, Ok(output))) = self.replies.iter().find(|(key, _)| key == "mount") {
                let volume = parse_mount_rows(output).iter().find_map(|(_, mount)| {
                    let name = last_component(mount);
                    name.starts_with(VOLUME_PREFIX).then(|| name.to_string())
                });
                *self.listed_volume.lock().expect("listed volume") = volume;
            }
        }
        let reply = if argv.first() == Some(&"/usr/bin/plutil") {
            let mut index = self.plutil_index.lock().expect("plutil index");
            let reply = self
                .replies
                .iter()
                .filter(|(key, _)| key == "plutil")
                .nth(*index);
            *index += 1;
            reply
        } else {
            self.replies.iter().find(|(key, _)| *key == line)
        };
        match reply {
            Some((_, reply)) => reply.clone(),
            None if argv.first() == Some(&"/usr/bin/plutil") => self.dynamic_plist(),
            None if argv.len() == 4 && argv[0] == "diskutil" && argv[1] == "info" => {
                Ok("plist".to_string())
            }
            None if argv.len() == 5
                && argv[0] == "diskutil"
                && argv[1] == "apfs"
                && argv[2] == "list" =>
            {
                Ok("plist".to_string())
            }
            None => Err(format!("fake: unexpected `{line}`")),
        }
    }
}

impl FakeRunner {
    fn dynamic_plist(&self) -> Result<String, String> {
        let request = self
            .plist_request
            .lock()
            .expect("plist request")
            .clone()
            .unwrap_or_default();
        if request.starts_with("/dev/") {
            let device = self
                .plist_device
                .lock()
                .expect("plist device")
                .clone()
                .unwrap_or_default();
            return Ok(serde_json::json!({
                "DeviceIdentifier": device,
                "APFSContainerReference": whole_disk(&device).trim_start_matches("/dev/"),
            })
            .to_string());
        }
        let Some(container) = request.strip_prefix("container:") else {
            return Err(format!("fake: unexpected plist request `{request}`"));
        };
        let device = self
            .plist_device
            .lock()
            .expect("plist device")
            .clone()
            .unwrap_or_else(|| format!("/dev/{container}s1"));
        let volume = self
            .listed_volume
            .lock()
            .expect("listed volume")
            .clone()
            .unwrap_or_else(|| "vivi-x".to_string());
        Ok(serde_json::json!({
            "Containers": [{
                "ContainerReference": container,
                "PhysicalStores": [{"DeviceIdentifier": whole_disk(&device)}],
                "Volumes": [{"Name": volume, "DeviceIdentifier": device}],
            }],
        })
        .to_string())
    }
}

/// A mount point under the system temp directory, removed by its owner.
fn mount_point(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("vivi-ramdisk-{tag}-{}", std::process::id()))
}

fn mounted_owned_device(
    runner: &dyn Runner,
    disk: &Disk,
    expected_mount: &Path,
) -> Result<(String, String), String> {
    let listing = runner.run(&["mount"], LIST_SECS)?;
    let expected_mount = expected_mount
        .canonicalize()
        .map_err(|error| format!("cannot resolve owned mountpoint: {error}"))?;
    for (device, mount) in parse_mount_rows(&listing) {
        if !same_device_id(&device, &disk.device) {
            continue;
        }
        let actual_mount = Path::new(&mount)
            .canonicalize()
            .map_err(|error| format!("cannot resolve attached mountpoint: {error}"))?;
        if actual_mount == expected_mount {
            let backing = backing_device_for_volume(runner, &device, &disk.volume)?;
            if disk
                .backing_device
                .as_deref()
                .is_some_and(|expected| !same_device_id(expected, &backing))
            {
                return Err("mounted logical volume has a different backing disk".to_string());
            }
            return Ok((device, mount));
        }
    }
    Err("owned RAM disk is not mounted at its exact temp path".to_string())
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
    let mount_line = format!("diskutil mount -mountPoint {} /dev/disk9s1", at.display());
    let erased = format!("diskutil eraseVolume APFS vivi-here-slot3-job1 /dev/disk8");
    let disk_info = disk_info_json("disk8", "disk9");
    let apfs_list = apfs_list_json("vivi-here-slot3-job1", "disk8", "disk9s1");
    let volume_info = volume_info_json("disk9s1", "disk9", "/Volumes/vivi-here-slot3-job1");
    let runner = FakeRunner::new(&[
        ("hdiutil attach -nomount ram://16777216", Ok("/dev/disk8\n")),
        (erased.as_str(), Ok("Started erase on disk8")),
        (
            "diskutil info -plist /dev/disk8",
            Ok("<?xml version=\"1.0\"?><plist><dict/></plist>"),
        ),
        ("plutil", Ok(disk_info.as_str())),
        (
            "diskutil apfs list -plist disk9",
            Ok("<?xml version=\"1.0\"?><plist><dict/></plist>"),
        ),
        ("plutil", Ok(apfs_list.as_str())),
        (
            "diskutil info -plist /dev/disk9s1",
            Ok("<?xml version=\"1.0\"?><plist><dict/></plist>"),
        ),
        ("plutil", Ok(volume_info.as_str())),
        (
            "diskutil unmount /dev/disk9s1",
            Ok("Volume vivi-here-slot3-job1 on disk9s1 unmounted"),
        ),
        (
            mount_line.as_str(),
            Ok("/dev/disk9s1 on /Volumes/vivi-here-slot3-job1"),
        ),
    ]);

    let disk = mount(&runner, "vivi-here-slot3-job1", 8, &at).expect("mount");

    assert_eq!(
        disk,
        Disk {
            device: "/dev/disk9s1".to_string(),
            backing_device: Some("/dev/disk8".to_string()),
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
            "diskutil info -plist /dev/disk8".to_string(),
            "/usr/bin/plutil -convert json -o - <plist>".to_string(),
            "diskutil apfs list -plist disk9".to_string(),
            "/usr/bin/plutil -convert json -o - <plist>".to_string(),
            "diskutil info -plist /dev/disk9s1".to_string(),
            "/usr/bin/plutil -convert json -o - <plist>".to_string(),
            "diskutil unmount /dev/disk9s1".to_string(),
            mount_line,
        ]
    );
    let _ = std::fs::remove_dir_all(&at);
}

#[test]
fn mount_refuses_a_container_not_backed_by_its_owned_physical_disk() {
    let at = mount_point("wrong-apfs-store");
    let disk_info = disk_info_json("disk8", "disk9");
    let apfs_list = apfs_list_json("vivi-x", "disk10", "disk9s1");
    let runner = FakeRunner::new(&[
        ("hdiutil attach -nomount ram://2097152", Ok("/dev/disk8\n")),
        (
            "diskutil eraseVolume APFS vivi-x /dev/disk8",
            Ok("Started erase on disk8"),
        ),
        (
            "diskutil info -plist /dev/disk8",
            Ok("<?xml version=\"1.0\"?><plist><dict/></plist>"),
        ),
        ("plutil", Ok(disk_info.as_str())),
        (
            "diskutil apfs list -plist disk9",
            Ok("<?xml version=\"1.0\"?><plist><dict/></plist>"),
        ),
        ("plutil", Ok(apfs_list.as_str())),
        ("diskutil eject /dev/disk8", Ok("disk8 ejected")),
    ]);

    let error = mount(&runner, "vivi-x", 1, &at).expect_err("foreign APFS store");

    assert!(
        error.contains("not backed by attached disk `disk8`"),
        "{error}"
    );
    assert!(
        runner
            .calls()
            .contains(&"diskutil eject /dev/disk8".to_string())
    );
    assert!(
        runner
            .calls()
            .iter()
            .all(|call| !call.starts_with("diskutil mount ")
                && !call.starts_with("diskutil unmount "))
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
        backing_device: Some("/dev/disk7".to_string()),
        volume: "vivi-x".to_string(),
        size_gib: 8,
        mount: PathBuf::from("/Volumes/vivi-x"),
    };

    assert_eq!(teardown(&runner, &disk), Ok(()));
    assert_eq!(
        runner.calls(),
        vec![
            "diskutil info -plist /dev/disk7s1".to_string(),
            "/usr/bin/plutil -convert json -o - <plist>".to_string(),
            "diskutil apfs list -plist disk7".to_string(),
            "/usr/bin/plutil -convert json -o - <plist>".to_string(),
            "diskutil eject /dev/disk7".to_string(),
            "mount".to_string(),
        ]
    );
}

#[test]
fn teardown_reports_a_disk_that_survived_the_eject() {
    let disk = Disk {
        device: "/dev/disk7s1".to_string(),
        backing_device: Some("/dev/disk7".to_string()),
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
        teardown(&by_volume, &disk),
        Ok(()),
        "a same-named volume on another logical device is not the owned disk"
    );
}

#[test]
fn teardown_surfaces_an_eject_error_when_the_disk_is_gone() {
    let runner = FakeRunner::new(&[
        ("diskutil eject /dev/disk7", Err("eject: Resource busy")),
        ("mount", Ok("")),
    ]);
    let disk = Disk {
        device: "/dev/disk7s1".to_string(),
        backing_device: Some("/dev/disk7".to_string()),
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
                backing_device: None,
                volume: "vivi-here-slot3-job1".to_string(),
                size_gib: 30,
                mount: PathBuf::from("/Volumes/vivi-here-slot3-job1"),
            },
            Disk {
                device: "/dev/disk7s1".to_string(),
                backing_device: None,
                volume: "vivi-here-slot4-job2".to_string(),
                size_gib: 8,
                mount: PathBuf::from("/Volumes/vivi-here-slot4-job2"),
            },
            Disk {
                device: "/dev/disk9s1".to_string(),
                backing_device: None,
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
fn mounted_recognizes_local_pool_mounts_through_the_exact_slot_marker() {
    let slot = mount_point("pool-marker");
    let mount = slot.join("targets").join("cache-key");
    std::fs::create_dir_all(&mount).unwrap();
    std::fs::write(
        slot.join("ramdisk"),
        format!(
            "device=/dev/disk13\nvolume_device=/dev/disk13s1\nvolume=vivi-here-pool-slot-0-job\nsize_gib=8\njob=job\nmount={}\nsource=/repo\n",
            mount.display()
        ),
    )
    .unwrap();
    let mount_text = format!("/dev/disk13s1 on {} (apfs, local)\n", mount.display());
    let runner = FakeRunner::new(&[
        ("mount", Ok(mount_text.as_str())),
        (
            "df -g",
            Ok("Filesystem 1G-blocks Used Available Capacity Mounted on\n"),
        ),
    ]);

    let disks = mounted(&runner);

    assert_eq!(disks.len(), 1);
    assert_eq!(disks[0].volume, "vivi-here-pool-slot-0-job");
    assert_eq!(disks[0].size_gib, 8, "marker size covers absent df rows");
    assert_eq!(disks[0].mount, mount);
    assert_eq!(held_gib(&runner), 8);
    let _ = std::fs::remove_dir_all(&slot);
}

#[test]
fn mounted_reports_nothing_when_the_listings_fail() {
    let runner = FakeRunner::new(&[
        ("mount", Err("mount: command failed")),
        ("df -g", Err("df: command failed")),
    ]);

    assert!(mounted(&runner).is_empty());
}

#[test]
#[ignore = "manual 1 GiB macOS hdiutil mount/eject smoke"]
fn real_runner_mounts_and_releases_one_gib_at_an_owned_temp_path() {
    if !cfg!(target_os = "macos") {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let pid = std::process::id().to_string();
    let volume = volume_name(&["smoke", &pid]);
    let runner = RealRunner;

    let disk = mount(&runner, &volume, 1, temp.path()).unwrap();
    let device_proof = mounted_owned_device(&runner, &disk, temp.path());
    let write = std::fs::write(temp.path().join("smoke.txt"), b"RAM disk smoke\n");
    let detach = teardown(&runner, &disk);

    assert!(
        device_proof.is_ok(),
        "mount device ownership: {device_proof:?}"
    );
    let (logical, mount_path) = device_proof.expect("mount device ownership was checked");
    let backing = disk
        .backing_device
        .as_deref()
        .expect("real mount records its physical backing disk");
    eprintln!(
        "RAM disk smoke mounted physical {} as logical {} on {}",
        backing, logical, mount_path
    );
    assert!(
        write.is_ok(),
        "mounted RAM disk was not writable: {write:?}"
    );
    assert!(
        detach.is_ok(),
        "RAM disk did not detach cleanly: {detach:?}"
    );
    assert!(
        !mounted(&runner)
            .iter()
            .any(|attached| attached.volume == volume)
    );
    assert!(
        runner.run(&["mount"], LIST_SECS).is_ok_and(|text| {
            parse_mount_rows(&text)
                .iter()
                .all(|(device, _)| whole_disk(device) != whole_disk(&disk.device))
        }),
        "owned device remained after teardown"
    );
    eprintln!("RAM disk smoke teardown verified the owned mountpoint is gone");
}

/// A runner whose listings report no attached disks, so `held_gib` is zero.
fn empty_runner() -> FakeRunner {
    FakeRunner::new(&[("mount", Ok("")), ("df -g", Ok(""))])
}

/// A runner holding two RAM disks of `first` and `second` GiB.
fn two_disk_runner(first: u64, second: u64) -> FakeRunner {
    let mounts = "\
/dev/disk5s1 on /Volumes/vivi-here-slot3-job1 (apfs, local)
/dev/disk7s1 on /Volumes/vivi-here-slot4-job2 (apfs, local)
";
    let df = format!(
        "Filesystem     1G-blocks Used Available Capacity Mounted on\n\
         /dev/disk5s1          {first}    1         0     4%   /Volumes/vivi-here-slot3-job1\n\
         /dev/disk7s1          {second}    1         0     4%   /Volumes/vivi-here-slot4-job2\n"
    );
    FakeRunner::new(&[("mount", Ok(mounts)), ("df -g", Ok(df.as_str()))])
}

#[test]
fn held_gib_sums_the_mounted_disks() {
    assert_eq!(held_gib(&two_disk_runner(30, 8)), 38);
    assert_eq!(held_gib(&empty_runner()), 0);
    assert_eq!(held_gib(&two_disk_runner(0, 0)), 0);
}

#[test]
fn admit_with_accepts_a_job_that_fits() {
    // 30 GiB free, 8 + 4 + 6 = 18 GiB needed.
    assert_eq!(admit_with(&empty_runner(), 30 * GIB, 8, 6), Ok(()));
    // The exact boundary is admitted.
    assert_eq!(admit_with(&empty_runner(), 14 * GIB, 8, 6), Ok(()));
    // Held disks count against the job: 4 GiB held pushes the need to 18.
    assert_eq!(admit_with(&two_disk_runner(2, 2), 18 * GIB, 8, 6), Ok(()));
}

#[test]
fn admit_with_queues_the_job_when_held_disks_push_it_over() {
    // Without the held disks 16 GiB would fit 8 + 6; with 4 GiB held it does not.
    let runner = two_disk_runner(2, 2);
    let error = admit_with(&runner, 16 * GIB, 8, 6).expect_err("held disks overflow");
    assert!(error.starts_with("ramdisk-wait:"), "{error}");
    assert!(error.contains("16 GiB free"), "{error}");
    assert!(error.contains("4 GiB held"), "{error}");
}

#[test]
fn admission_waits_when_the_attached_disk_inventory_is_unavailable() {
    let runner = FakeRunner::new(&[("mount", Err("inventory unavailable"))]);

    let error = admit_with(&runner, 64 * GIB, 8, 6).expect_err("unknown held memory");

    assert!(error.starts_with("ramdisk-wait:"), "{error}");
    assert!(error.contains("cannot inventory held disks"), "{error}");
    assert!(error.contains("inventory unavailable"), "{error}");
    assert_eq!(runner.calls(), ["mount"]);
}

#[test]
fn admit_with_queues_the_job_when_free_memory_is_short() {
    let error = admit_with(&empty_runner(), 12 * GIB, 8, 6).expect_err("too little free");
    assert!(error.starts_with("ramdisk-wait:"), "{error}");
    assert!(error.contains("12 GiB free"), "{error}");
}
