//! Tests for the pump-start APFS volume sweep in [`crate::pump::apfssweep`].
//! The git side runs through [`crate::pump::host::FakeHost`]; the disk side
//! through a `Runner` that keeps a live APFS volume table.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::{sweep_apfs, sweep_apfs_with};
use crate::pool::ramdisk::Runner;
use crate::pump::config::PumpConfig;
use crate::pump::host::{Ctx, FakeHost, test_ctx};

/// Volumes as `(device, name, quota bytes)`, and `(device, mount)` rows.
struct Disks {
    volumes: Mutex<Vec<(String, String, u64)>>,
    mounts: Vec<(String, String)>,
    calls: Mutex<Vec<String>>,
}

impl Disks {
    fn new(volumes: &[(&str, &str, u64)], mounts: &[(&str, &str)]) -> Self {
        Self {
            volumes: Mutex::new(
                volumes
                    .iter()
                    .map(|(d, n, q)| ((*d).to_string(), (*n).to_string(), *q))
                    .collect(),
            ),
            mounts: mounts
                .iter()
                .map(|(d, m)| ((*d).to_string(), (*m).to_string()))
                .collect(),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn deletes(&self) -> Vec<String> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter_map(|c| {
                c.strip_prefix("diskutil apfs deleteVolume ")
                    .map(String::from)
            })
            .collect()
    }
}

impl Runner for Disks {
    fn run(&self, argv: &[&str], _timeout_secs: u64) -> Result<String, String> {
        if argv.first() == Some(&"/usr/bin/plutil") {
            return std::fs::read_to_string(argv[argv.len() - 1]).map_err(|e| e.to_string());
        }
        let line = argv.join(" ");
        self.calls.lock().unwrap().push(line.clone());
        match argv {
            ["diskutil", "apfs", "list", "-plist"] => {
                let volumes: Vec<_> = self
                    .volumes
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|(d, n, q)| {
                        serde_json::json!({"DeviceIdentifier": d, "Name": n,
                            "CapacityQuota": q, "CapacityInUse": 0})
                    })
                    .collect();
                Ok(
                    serde_json::json!({"Containers": [{"ContainerReference": "disk3",
                    "CapacityFree": 1_u64 << 40, "Volumes": volumes}]})
                    .to_string(),
                )
            }
            ["mount"] => Ok(self
                .mounts
                .iter()
                .map(|(d, m)| format!("{d} on {m} (apfs, local, nodev)"))
                .collect::<Vec<_>>()
                .join("\n")),
            ["diskutil", "apfs", "deleteVolume", device] => {
                self.volumes.lock().unwrap().retain(|v| v.0 != *device);
                Ok("Finished".into())
            }
            _ => Err(format!("fake: unexpected `{line}`")),
        }
    }
}

const QUOTA: u64 = 12 << 30;

fn scratch(tag: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::Builder::new()
        .prefix(&format!("vivi-apfssweep-{tag}-"))
        .tempdir()
        .unwrap();
    let path = dir.path().canonicalize().unwrap();
    (dir, path)
}

fn ctx_with(mailspace: &str, scratch: &Path) -> (Ctx, Arc<FakeHost>) {
    let (ctx, host) = test_ctx(&scratch.join("project"), PumpConfig::default());
    let owner = scratch.join("owner");
    std::fs::create_dir_all(owner.join(".git")).unwrap();
    host.reply(
        "rev-parse --git-common-dir",
        0,
        &format!("{}\n", owner.join(".git").display()),
    );
    host.reply("worktree remove --force", 0, "");
    (ctx.with_mailspace(mailspace.to_string()), host)
}

/// A slot target mount holding a `repo` worktree, with the APFS marker the
/// local provider writes for `volume`.
fn pool_mount(scratch: &Path, volume: &str) -> (PathBuf, PathBuf) {
    let slot = scratch.join("slot-1");
    let mount = slot.join("targets").join("key");
    std::fs::create_dir_all(mount.join("repo")).unwrap();
    std::fs::write(mount.join("repo/.git"), "gitdir: /elsewhere\n").unwrap();
    std::fs::write(
        slot.join("apfs"),
        format!(
            "volume_device=/dev/disk3s7\nvolume={volume}\nsize_gib=12\njob=j\nmount={}\nsource=/repo\ncontainer=disk3\n",
            mount.display()
        ),
    )
    .unwrap();
    (slot, mount)
}

#[test]
fn an_orphaned_pool_volume_loses_its_worktree_volume_and_marker() {
    let (_dir, scratch) = scratch("orphan");
    let (slot, mount) = pool_mount(&scratch, "vivi-here-pool-slot-1-j");
    let disks = Disks::new(
        &[("disk3s7", "vivi-here-pool-slot-1-j", QUOTA)],
        &[("/dev/disk3s7", &mount.to_string_lossy())],
    );
    let (ctx, host) = ctx_with("here", &scratch);

    let lines = sweep_apfs_with(&ctx, &disks, |_| false);

    assert_eq!(lines, vec!["apfs sweep: deleted vivi-here-pool-slot-1-j"]);
    assert_eq!(disks.deletes(), vec!["disk3s7"]);
    assert!(
        !slot.join("apfs").exists(),
        "the marker goes with the volume"
    );
    assert!(
        host.commands()
            .iter()
            .any(|c| c.contains("worktree remove --force"))
    );
}

#[test]
fn a_live_slot_keeps_its_volume() {
    let (_dir, scratch) = scratch("live");
    let (slot, mount) = pool_mount(&scratch, "vivi-here-pool-slot-1-j");
    let disks = Disks::new(
        &[("disk3s7", "vivi-here-pool-slot-1-j", QUOTA)],
        &[("/dev/disk3s7", &mount.to_string_lossy())],
    );
    let (ctx, _host) = ctx_with("here", &scratch);

    let lines = sweep_apfs_with(&ctx, &disks, |candidate| candidate == slot);

    assert!(disks.deletes().is_empty(), "{lines:?}");
    assert!(lines[0].contains("preserving"), "{lines:?}");
    assert!(slot.join("apfs").exists());
}

#[test]
fn a_volume_remounted_after_reboot_is_deleted_by_name_scope_and_quota() {
    let (_dir, scratch) = scratch("reboot");
    let disks = Disks::new(
        &[
            ("disk3s7", "vivi-here-pool-slot-1-j", QUOTA),
            ("disk3s8", "vivi-there-pool-slot-1-j", QUOTA),
            ("disk3s9", "vivi-here-unquoted", 0),
            ("disk3s5", "Data", 0),
        ],
        &[("/dev/disk3s7", "/Volumes/vivi-here-pool-slot-1-j")],
    );
    let (ctx, _host) = ctx_with("here", &scratch);

    let lines = sweep_apfs(&ctx, &disks);

    assert_eq!(disks.deletes(), vec!["disk3s7"], "{lines:?}");
}

#[test]
fn nothing_is_said_when_there_is_nothing_to_sweep_or_no_apfs() {
    let (_dir, scratch) = scratch("quiet");
    let (ctx, _host) = ctx_with("here", &scratch);
    let none = Disks::new(&[("disk3s5", "Data", 0)], &[]);
    assert!(sweep_apfs(&ctx, &none).is_empty());

    struct NoApfs;
    impl Runner for NoApfs {
        fn run(&self, _argv: &[&str], _timeout_secs: u64) -> Result<String, String> {
            Err("not macOS".into())
        }
    }
    assert!(sweep_apfs(&ctx, &NoApfs).is_empty());
}
