//! Tests for the pump-start RAM disk sweep in [`crate::pump::ramsweep`].
//!
//! Wired from `pump.rs` as the module `ramsweep_test`, so these tests read as
//! `pump::ramsweep_test::*`. The git side runs through
//! [`crate::pump::host::FakeHost`]; the disk side through a `Runner` that
//! serves a `mount` listing and honours `diskutil eject`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::pool::ramdisk::Runner;
use crate::pump::config::PumpConfig;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::ramsweep::{sweep_ramdisks, sweep_ramdisks_with};

/// One attached fake RAM disk: whole-disk device and mount point.
type Attached = Vec<(String, String)>;

/// A `Runner` serving `mount` / `df -g` from an attach list that `eject`
/// empties. `keep_attached` makes an eject that changes nothing, which is how
/// a failed verification is staged.
struct FakeRunner {
    attached: Mutex<Attached>,
    keep_attached: bool,
    calls: Mutex<Vec<String>>,
    plist_request: Mutex<Option<String>>,
    plist_device: Mutex<Option<String>>,
    listed_volume: Mutex<Option<String>>,
}

impl FakeRunner {
    fn new(attached: &[(&str, &str)]) -> Self {
        Self {
            attached: Mutex::new(
                attached
                    .iter()
                    .map(|(device, mount)| ((*device).to_string(), (*mount).to_string()))
                    .collect(),
            ),
            keep_attached: false,
            calls: Mutex::new(Vec::new()),
            plist_request: Mutex::new(None),
            plist_device: Mutex::new(None),
            listed_volume: Mutex::new(None),
        }
    }

    fn keep_attached(self) -> Self {
        Self {
            keep_attached: true,
            ..self
        }
    }

    /// The device of every `diskutil eject` call, in order.
    fn ejections(&self) -> Vec<String> {
        self.calls
            .lock()
            .expect("call log")
            .iter()
            .filter_map(|line| line.strip_prefix("diskutil eject ").map(String::from))
            .collect()
    }

    fn attached(&self) -> Attached {
        self.attached.lock().expect("attach list").clone()
    }

    fn eject(&self, device: &str) {
        if self.keep_attached {
            return;
        }
        self.attached
            .lock()
            .expect("attach list")
            .retain(|(whole, _)| whole != device && !whole.starts_with(&format!("{device}s")));
    }
}

impl Runner for FakeRunner {
    fn run(&self, argv: &[&str], _timeout_secs: u64) -> Result<String, String> {
        let line = argv.join(" ");
        self.calls.lock().expect("call log").push(line.clone());
        match line.as_str() {
            "mount" => {
                let attached = self.attached();
                let volume = attached.iter().find_map(|(_, mount)| {
                    let path = PathBuf::from(mount);
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .filter(|name| name.starts_with("vivi-"))
                        .map(ToString::to_string)
                        .or_else(|| {
                            let slot = crate::pool::ramdisk::pool_slot_for_mount(&path)?;
                            std::fs::read_to_string(slot.join("ramdisk"))
                                .ok()?
                                .lines()
                                .find_map(|line| line.strip_prefix("volume="))
                                .map(ToString::to_string)
                        })
                });
                *self.listed_volume.lock().expect("listed volume") = volume;
                Ok(attached
                    .iter()
                    .map(|(device, mount)| format!("{device} on {mount} (apfs, local, nodev)"))
                    .collect::<Vec<_>>()
                    .join("\n"))
            }
            "df -g" => Ok(self
                .attached()
                .iter()
                .map(|(_, mount)| format!("map ram 8 8 0 0 100% {mount}"))
                .collect::<Vec<_>>()
                .join("\n")),
            _ if argv.len() == 4 && argv[0] == "diskutil" && argv[1] == "info" => {
                *self.plist_request.lock().expect("plist request") = Some(argv[3].to_string());
                *self.plist_device.lock().expect("plist device") = Some(argv[3].to_string());
                Ok("plist".to_string())
            }
            _ if argv.len() == 5
                && argv[0] == "diskutil"
                && argv[1] == "apfs"
                && argv[2] == "list" =>
            {
                *self.plist_request.lock().expect("plist request") =
                    Some(format!("container:{}", argv[4]));
                Ok("plist".to_string())
            }
            _ if argv.first() == Some(&"/usr/bin/plutil") => {
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
                    Ok(serde_json::json!({
                        "DeviceIdentifier": device,
                        "APFSContainerReference": crate::pool::ramdisk::whole_disk(&device).trim_start_matches("/dev/"),
                    }).to_string())
                } else {
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
                            "PhysicalStores": [{"DeviceIdentifier": crate::pool::ramdisk::whole_disk(&device)}],
                            "Volumes": [{"Name": volume, "DeviceIdentifier": device}],
                        }],
                    }).to_string())
                }
            }
            _ => match line.strip_prefix("diskutil eject ") {
                Some(device) => {
                    self.eject(device);
                    Ok(String::new())
                }
                None => Err(format!("fake: unexpected `{line}`")),
            },
        }
    }
}

/// A scratch directory under the system temp dir, removed on drop.
struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!("vivi-ramsweep-{tag}-{}", std::process::id()));
        drop(std::fs::remove_dir_all(&path));
        std::fs::create_dir_all(&path).expect("scratch dir");
        Self { path }
    }

    /// A fake mount point holding a worktree directory per `worktrees` entry,
    /// each carrying the `.git` marker a real worktree has.
    fn mount_point(&self, volume: &str, worktrees: &[&str]) -> String {
        let mount = self.path.join(volume);
        for name in worktrees {
            std::fs::create_dir_all(mount.join(name)).expect("worktree dir");
            std::fs::write(mount.join(name).join(".git"), "gitdir: /elsewhere\n")
                .expect("git marker");
        }
        mount.to_string_lossy().into_owned()
    }

    /// A local pool RAM disk mounted at `<slot>/targets/<key>`, with the
    /// ownership marker written by LocalProvider.
    fn pool_mount_point(&self, volume: &str, job: &str) -> String {
        let mount = self.path.join("slot-0").join("targets").join("cache-key");
        std::fs::create_dir_all(&mount).expect("pool mount directory");
        std::fs::write(
            mount.parent().unwrap().parent().unwrap().join("ramdisk"),
            format!(
                "device=/dev/disk7\nvolume_device=/dev/disk7s1\nvolume={volume}\nsize_gib=8\njob={job}\nmount={}\nsource=/repo\n",
                mount.display()
            ),
        )
        .expect("pool marker");
        mount.to_string_lossy().into_owned()
    }

    /// The repository the fake `rev-parse` names as every worktree's owner.
    fn owner(&self) -> PathBuf {
        let owner = self.path.join("owner");
        std::fs::create_dir_all(owner.join(".git")).expect("owner repo");
        owner
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        drop(std::fs::remove_dir_all(&self.path));
    }
}

/// A context scoped to `mailspace`, with the git replies the sweep needs: every
/// worktree's common dir is the scratch repository's `.git`.
fn ctx_with(mailspace: &str, scratch: &Scratch) -> (Ctx, Arc<FakeHost>) {
    let (ctx, host) = test_ctx(&scratch.path.join("project"), PumpConfig::default());
    host.reply(
        "rev-parse --git-common-dir",
        0,
        &format!("{}\n", scratch.owner().join(".git").display()),
    );
    host.reply("worktree remove --force", 0, "");
    (ctx.with_mailspace(mailspace.to_string()), host)
}

/// Every `worktree remove` command line the fake host ran, in order.
fn removals(host: &FakeHost) -> Vec<String> {
    host.commands()
        .into_iter()
        .filter(|line| line.contains("worktree remove"))
        .collect()
}

/// Whether any command line the fake host ran contains `fragment`.
fn ran(host: &FakeHost, fragment: &str) -> bool {
    host.commands().iter().any(|line| line.contains(fragment))
}

#[test]
fn sweep_detaches_only_this_mailspace_disk() {
    let scratch = Scratch::new("scope");
    let here = scratch.mount_point("vivi-here-slot1-job1", &["repo"]);
    let foreign = scratch.mount_point("vivi-other-slot2-job2", &["repo"]);
    let runner = FakeRunner::new(&[
        ("/dev/disk7s1", here.as_str()),
        ("/dev/disk9s1", foreign.as_str()),
    ]);
    let (ctx, host) = ctx_with("here", &scratch);

    let lines = sweep_ramdisks(&ctx, &runner);

    assert_eq!(runner.ejections(), vec!["/dev/disk7".to_string()]);
    assert_eq!(
        lines,
        vec!["ramdisk sweep: detached vivi-here-slot1-job1".to_string()]
    );
    assert!(!ran(&host, &foreign), "the foreign disk is never touched");
}

#[test]
fn sweep_removes_the_exact_worktree_paths_and_never_prunes() {
    let scratch = Scratch::new("paths");
    let mount = scratch.mount_point("vivi-here-slot1-job1", &["repo", "w"]);
    let owner = scratch.owner();
    let runner = FakeRunner::new(&[("/dev/disk7s1", mount.as_str())]);
    let (ctx, host) = ctx_with("here", &scratch);

    let lines = sweep_ramdisks(&ctx, &runner);

    assert_eq!(
        removals(&host),
        vec![
            format!(
                "git -C {} worktree remove --force {mount}/repo",
                owner.display()
            ),
            format!(
                "git -C {} worktree remove --force {mount}/w",
                owner.display()
            ),
        ]
    );
    assert!(!ran(&host, "worktree prune"), "no repository-wide prune");
    assert!(runner.attached().is_empty(), "the disk is ejected");
    assert_eq!(
        lines,
        vec!["ramdisk sweep: detached vivi-here-slot1-job1".to_string()]
    );
}

#[test]
fn sweep_skips_a_mount_without_worktrees() {
    let scratch = Scratch::new("empty");
    let mount = scratch.mount_point("vivi-here-slot1-job1", &[]);
    let runner = FakeRunner::new(&[("/dev/disk7s1", mount.as_str())]);
    let (ctx, host) = ctx_with("here", &scratch);

    let lines = sweep_ramdisks(&ctx, &runner);

    assert!(removals(&host).is_empty());
    assert!(!ran(&host, "rev-parse"), "no git call for a bare mount");
    assert_eq!(
        lines,
        vec!["ramdisk sweep: detached vivi-here-slot1-job1".to_string()]
    );
}

#[test]
fn sweep_preserves_a_live_dispatched_pool_job_across_pump_restart() {
    let scratch = Scratch::new("live-pool-job");
    let mount = scratch.pool_mount_point("vivi-here-slot-0-build", "build");
    let runner = FakeRunner::new(&[("/dev/disk7s1", mount.as_str())]);
    let (ctx, host) = ctx_with("here", &scratch);
    let lines = sweep_ramdisks_with(&ctx, &runner, |_| true);

    assert!(runner.ejections().is_empty(), "live job disk stays mounted");
    assert!(
        removals(&host).is_empty(),
        "live job worktree stays registered"
    );
    assert_eq!(
        lines,
        vec!["ramdisk sweep: preserving vivi-here-slot-0-build for live dispatched job"]
    );
}

#[test]
fn sweep_preserves_a_pool_mount_when_its_marker_names_another_device() {
    let scratch = Scratch::new("pool-device-mismatch");
    let mount = scratch.pool_mount_point("vivi-here-slot-0-build", "build");
    let marker = PathBuf::from(&mount)
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("ramdisk");
    let text = std::fs::read_to_string(&marker).unwrap();
    std::fs::write(
        &marker,
        text.replace("device=/dev/disk7", "device=/dev/disk8"),
    )
    .unwrap();
    let runner = FakeRunner::new(&[("/dev/disk7s1", mount.as_str())]);
    let (ctx, host) = ctx_with("here", &scratch);

    let lines = sweep_ramdisks_with(&ctx, &runner, |_| false);

    assert!(runner.ejections().is_empty());
    assert!(removals(&host).is_empty());
    assert_eq!(
        lines,
        vec![
            "WARNING ramdisk sweep: preserving vivi-here-slot-0-build because its pool ownership marker does not match the attached device"
        ]
    );
}

#[test]
fn sweep_warns_when_the_disk_survives_teardown() {
    let scratch = Scratch::new("survivor");
    let mount = scratch.mount_point("vivi-here-slot1-job1", &["repo"]);
    let runner = FakeRunner::new(&[("/dev/disk7s1", mount.as_str())]).keep_attached();
    let (ctx, host) = ctx_with("here", &scratch);

    let lines = sweep_ramdisks(&ctx, &runner);

    assert!(removals(&host).len() == 1, "the worktree is still detached");
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("WARNING"), "loud: {}", lines[0]);
    assert!(lines[0].contains("vivi-here-slot1-job1"), "{}", lines[0]);
    assert!(lines[0].contains("still attached"), "{}", lines[0]);
}

#[test]
fn sweep_without_a_mailspace_sweeps_every_vivi_disk_and_says_so() {
    let scratch = Scratch::new("unscoped");
    let one = scratch.mount_point("vivi-a-slot1-job1", &[]);
    let two = scratch.mount_point("vivi-b-slot2-job2", &[]);
    let runner = FakeRunner::new(&[
        ("/dev/disk7s1", one.as_str()),
        ("/dev/disk9s1", two.as_str()),
    ]);
    let (ctx, _host) = ctx_with("", &scratch);

    let lines = sweep_ramdisks(&ctx, &runner);

    assert_eq!(runner.ejections().len(), 2);
    assert_eq!(
        lines.len(),
        4,
        "a warning plus a detach line per disk: {lines:?}"
    );
    for volume in ["vivi-a-slot1-job1", "vivi-b-slot2-job2"] {
        assert!(
            lines
                .iter()
                .any(|line| line.contains("WARNING") && line.contains(volume)),
            "scoping-off warning for {volume}: {lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line == &format!("ramdisk sweep: detached {volume}")),
            "detach line for {volume}: {lines:?}"
        );
    }
}
