//! Tests for [`crate::pool::apfs`]: listing, admission, create and delete
//! against canned `diskutil` output.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Mutex;

use super::*;

const GIB: u64 = 1024 * 1024 * 1024;

/// Replies keyed by the exact command line, each served once in order; the
/// plutil step returns the staged file as-is, so canned "plists" are JSON.
struct Fake {
    calls: Mutex<Vec<String>>,
    replies: Mutex<Vec<(String, VecDeque<Result<String, String>>)>>,
}

impl Fake {
    fn new(replies: &[(&str, Result<&str, &str>)]) -> Self {
        let mut keyed: Vec<(String, VecDeque<Result<String, String>>)> = Vec::new();
        for (line, reply) in replies {
            let reply = reply.map(str::to_string).map_err(str::to_string);
            match keyed.iter_mut().find(|(key, _)| key == line) {
                Some((_, queue)) => queue.push_back(reply),
                None => keyed.push(((*line).to_string(), VecDeque::from([reply]))),
            }
        }
        Self {
            calls: Mutex::new(Vec::new()),
            replies: Mutex::new(keyed),
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("calls").clone()
    }
}

impl Runner for Fake {
    fn run(&self, argv: &[&str], _timeout_secs: u64) -> Result<String, String> {
        if argv.first() == Some(&"/usr/bin/plutil") {
            let path = argv.last().expect("plutil path");
            return std::fs::read_to_string(path).map_err(|e| e.to_string());
        }
        let line = argv.join(" ");
        self.calls.lock().expect("calls").push(line.clone());
        let mut replies = self.replies.lock().expect("replies");
        let queue = replies.iter_mut().find(|(key, _)| *key == line);
        match queue.and_then(|(_, queue)| queue.pop_front()) {
            Some(reply) => reply,
            None => Err(format!("fake: unexpected `{line}`")),
        }
    }
}

const LIST: &str = "diskutil apfs list -plist";

/// A listing of container disk3 with `free` bytes and the given volumes as
/// `(device, name, quota, in_use)`.
fn listing(free: u64, volumes: &[(&str, &str, u64, u64)]) -> String {
    let volumes: Vec<_> = volumes
        .iter()
        .map(|(device, name, quota, in_use)| {
            serde_json::json!({
                "DeviceIdentifier": device,
                "Name": name,
                "CapacityQuota": quota,
                "CapacityInUse": in_use,
            })
        })
        .collect();
    serde_json::json!({
        "Containers": [{
            "ContainerReference": "disk3",
            "CapacityFree": free,
            "Volumes": volumes,
        }],
    })
    .to_string()
}

fn job_disk(device: &str, name: &str) -> Disk {
    Disk {
        device: device.to_string(),
        backing_device: None,
        volume: name.to_string(),
        size_gib: 12,
        mount: Path::new("/tmp/slot/targets/k").to_path_buf(),
    }
}

#[test]
fn the_create_command_sets_a_quota_and_never_a_reserve() {
    let argv = create_command("disk3", "vivi-p-slot-1-j", 12);
    assert_eq!(
        argv.join(" "),
        "diskutil apfs addVolume disk3 APFS vivi-p-slot-1-j -quota 12g -nomount"
    );
    assert!(!argv.iter().any(|arg| arg.contains("reserve")), "{argv:?}");
}

#[test]
fn created_device_reads_the_addvolume_report() {
    let out = "Will export new APFS Volume\nDisk from APFS operation: disk3s7\nFinished APFS operation on disk3\n";
    assert_eq!(created_device(out).as_deref(), Some("disk3s7"));
    assert_eq!(created_device("Finished APFS operation on disk3"), None);
    assert_eq!(created_device("Disk from APFS operation: ; rm -rf /"), None);
}

#[test]
fn only_named_quota_volumes_are_ours() {
    let (containers, volumes) = parse_list(
        &serde_json::from_str(&listing(
            100 * GIB,
            &[
                ("disk3s5", "Data", 0, 50 * GIB),
                ("disk3s7", "vivi-p-slot-1-j", 12 * GIB, GIB),
                ("disk3s8", "vivi-but-no-quota", 0, 0),
                ("bogus", "vivi-bad-device", GIB, 0),
            ],
        ))
        .expect("json"),
    );
    assert_eq!(containers[0].reference, "disk3");
    assert_eq!(containers[0].free_bytes, 100 * GIB);
    let ours: Vec<_> = volumes.iter().filter(|v| v.is_ours()).collect();
    assert_eq!(ours.len(), 1);
    assert_eq!(ours[0].device, "disk3s7");
    assert!(volumes.iter().all(|v| v.device != "bogus"));
}

#[test]
fn admission_counts_what_other_vivi_volumes_may_still_grow_into() {
    assert!(admits(30 * GIB, 0, 12, 6));
    assert!(!admits(30 * GIB, 13 * GIB, 12, 6));
    assert!(admits(30 * GIB, 12 * GIB, 12, 6));
    let fake = Fake::new(&[(
        LIST,
        Ok(&listing(
            29 * GIB,
            &[("disk3s7", "vivi-p-slot-1-a", 12 * GIB, 0)],
        )),
    )]);
    let said = admit(&fake, "disk3", 12, 6).expect_err("12 promised + 12 + 6 > 29");
    assert!(said.starts_with("apfs-wait:"), "{said}");
    assert!(said.contains("disk3"), "{said}");
    let fake = Fake::new(&[(LIST, Ok(&listing(100 * GIB, &[])))]);
    assert!(admit(&fake, "disk9", 1, 0).is_err(), "unknown container");
}

#[test]
fn create_makes_an_unmounted_quota_volume_then_mounts_it_noowners() {
    let at = tempfile::tempdir().expect("tempdir");
    let mount = at.path().join("targets").join("k");
    let mount_line = format!(
        "diskutil mount -mountOptions noowners -mountPoint {} disk3s7",
        mount.display()
    );
    let fake = Fake::new(&[
        (
            "diskutil apfs addVolume disk3 APFS vivi-p-slot-1-j -quota 12g -nomount",
            Ok("Disk from APFS operation: disk3s7\n"),
        ),
        (&mount_line, Ok("Volume vivi-p-slot-1-j on disk3s7 mounted")),
    ]);
    let disk = create(&fake, "disk3", "vivi-p-slot-1-j", 12, &mount).expect("create");
    assert_eq!(disk.device, "/dev/disk3s7");
    assert_eq!(disk.volume, "vivi-p-slot-1-j");
    assert_eq!(disk.mount, mount);
    assert!(mount.is_dir());
    assert_eq!(fake.calls().len(), 2);
}

#[test]
fn create_deletes_a_volume_it_cannot_mount() {
    let at = tempfile::tempdir().expect("tempdir");
    let mount = at.path().join("m");
    let mount_line = format!(
        "diskutil mount -mountOptions noowners -mountPoint {} disk3s7",
        mount.display()
    );
    let fake = Fake::new(&[
        (
            "diskutil apfs addVolume disk3 APFS vivi-x -quota 2g -nomount",
            Ok("Disk from APFS operation: disk3s7"),
        ),
        (&mount_line, Err("busy")),
        ("diskutil apfs deleteVolume disk3s7", Ok("Finished")),
    ]);
    let said = create(&fake, "disk3", "vivi-x", 2, &mount).expect_err("mount fails");
    assert!(said.contains("cannot mount"), "{said}");
    assert_eq!(
        fake.calls().last().map(String::as_str),
        Some("diskutil apfs deleteVolume disk3s7")
    );
}

#[test]
fn create_refuses_a_name_that_is_not_vivis() {
    let fake = Fake::new(&[]);
    let said = create(&fake, "disk3", "Data", 2, Path::new("/tmp/x")).expect_err("refused");
    assert!(said.contains("non-Vivi"), "{said}");
    assert!(fake.calls().is_empty());
}

#[test]
fn delete_proves_ownership_deletes_and_verifies() {
    let present = listing(50 * GIB, &[("disk3s7", "vivi-p-slot-1-j", 12 * GIB, GIB)]);
    let gone = listing(50 * GIB, &[]);
    let fake = Fake::new(&[
        (LIST, Ok(&present)),
        ("diskutil apfs deleteVolume disk3s7", Ok("Finished")),
        (LIST, Ok(&gone)),
    ]);
    delete(&fake, &job_disk("/dev/disk3s7", "vivi-p-slot-1-j")).expect("deleted");
    assert_eq!(fake.calls().len(), 3);
}

#[test]
fn delete_refuses_a_device_that_now_holds_another_volume() {
    for (name, quota) in [
        ("Data", 0),
        ("vivi-someone-else", 12 * GIB),
        ("vivi-p-slot-1-j", 0),
    ] {
        let fake = Fake::new(&[(LIST, Ok(&listing(GIB, &[("disk3s7", name, quota, 0)])))]);
        let said =
            delete(&fake, &job_disk("/dev/disk3s7", "vivi-p-slot-1-j")).expect_err("not ours");
        assert!(said.contains("refusing"), "{name}: {said}");
        assert_eq!(
            fake.calls(),
            vec![LIST.to_string()],
            "{name}: no delete ran"
        );
    }
}

#[test]
fn delete_of_a_volume_already_gone_is_success() {
    let fake = Fake::new(&[(LIST, Ok(&listing(GIB, &[])))]);
    delete(&fake, &job_disk("/dev/disk3s7", "vivi-p-slot-1-j")).expect("gone is fine");
}

#[test]
fn delete_forces_the_unmount_once_and_reports_a_survivor() {
    let present = listing(GIB, &[("disk3s7", "vivi-j", GIB, 0)]);
    let fake = Fake::new(&[
        (LIST, Ok(&present)),
        ("diskutil apfs deleteVolume disk3s7", Err("resource busy")),
        ("diskutil unmount force disk3s7", Ok("Unmounted")),
        ("diskutil apfs deleteVolume disk3s7", Ok("Finished")),
        (LIST, Ok(&present)),
    ]);
    let said = delete(&fake, &job_disk("disk3s7", "vivi-j")).expect_err("survived");
    assert!(said.contains("still present"), "{said}");
    assert!(
        fake.calls()
            .contains(&"diskutil unmount force disk3s7".to_string())
    );
}

#[test]
fn ours_filters_by_prefix_and_quota() {
    let fake = Fake::new(&[(
        LIST,
        Ok(&listing(
            GIB,
            &[
                ("disk3s7", "vivi-space-p-slot-1-j", GIB, 0),
                ("disk3s8", "vivi-other-p-slot-1-j", GIB, 0),
                ("disk3s9", "vivi-space-unquoted", 0, 0),
            ],
        )),
    )]);
    let found = ours(&fake, "vivi-space-").expect("list");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].device, "disk3s7");
}

#[test]
fn container_of_resolves_the_device_of_a_path_then_its_container() {
    let df = "Filesystem 512-blocks Used Available Capacity Mounted on\n/dev/disk3s5 1 1 1 1% /System/Volumes/Data\n";
    let fake = Fake::new(&[
        ("df -P /Users/me/vivi-pool", Ok(df)),
        (
            "diskutil info -plist disk3s5",
            Ok(r#"{"APFSContainerReference":"disk3","DeviceIdentifier":"disk3s5"}"#),
        ),
    ]);
    assert_eq!(
        container_of(&fake, Path::new("/Users/me/vivi-pool")).as_deref(),
        Ok("disk3")
    );
    let fake = Fake::new(&[
        (
            "df -P /mnt/x",
            Ok("Filesystem\n/dev/disk2s1 1 1 1 1% /mnt\n"),
        ),
        (
            "diskutil info -plist disk2s1",
            Ok(r#"{"DeviceIdentifier":"disk2s1"}"#),
        ),
    ]);
    assert!(
        container_of(&fake, Path::new("/mnt/x")).is_err(),
        "not APFS"
    );
    let fake = Fake::new(&[(
        "df -P /net/x",
        Ok("Filesystem\nmap auto_home 0 0 0 100% /net\n"),
    )]);
    assert!(
        container_of(&fake, Path::new("/net/x")).is_err(),
        "no disk device"
    );
}

#[test]
#[ignore = "manual 1 GiB macOS APFS quota volume create/fill/delete smoke"]
fn real_runner_makes_fills_and_deletes_a_one_gib_quota_volume() {
    if !cfg!(target_os = "macos") {
        return;
    }
    use std::io::Write;
    let runner = crate::pool::ramdisk::RealRunner;
    let dir = tempfile::tempdir().expect("tempdir");
    let base = dir.path().canonicalize().expect("canonical");
    let container = container_of(&runner, &base).expect("container of the temp dir");
    let mount = base.join("targets").join("k");
    let name = format!("vivi-smoke-{}", std::process::id());
    let disk = create(&runner, &container, &name, 1, &mount).expect("create");
    let listed = ours(&runner, &name).expect("list");
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(
        listed[0].quota_bytes,
        1_000_001_536.max(listed[0].quota_bytes)
    );
    let mut file = std::fs::File::create(mount.join("fill")).expect("writable as this user");
    let chunk = vec![7_u8; 1 << 20];
    let mut written = 0_u64;
    let refused = loop {
        if let Err(error) = file.write_all(&chunk) {
            break error;
        }
        written += chunk.len() as u64;
        assert!(written < 2 * GIB, "the quota never stopped the writes");
    };
    assert!(written < GIB, "stopped at {written} bytes: {refused}");
    drop(file);
    delete(&runner, &disk).expect("delete");
    assert!(
        ours(&runner, &name).expect("list").is_empty(),
        "volume gone"
    );
}
