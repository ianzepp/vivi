//! Tests for the local provider's per-job APFS volume, against a fake
//! `diskutil` that keeps a live volume table.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::pool::config::PoolConfig;
use crate::pool::local::LocalProvider;
use crate::pool::local_slots as slots;
use crate::pool::provider::{CreateSpec, PoolProvider};
use crate::pool::ramdisk::Runner;
use crate::pool::slot::{PoolId, slot_metadata};

/// A canonical pool root under a fresh temp dir (macOS temp paths are links).
fn root() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap().join("pool");
    (dir, root)
}

const GIB: u64 = 1024 * 1024 * 1024;

/// A fake `diskutil` over container disk3: `addVolume` adds a volume,
/// `deleteVolume` removes it, and `apfs list` reports what is there.
struct ApfsRunner {
    calls: Mutex<Vec<String>>,
    volumes: Mutex<Vec<(String, String, u64)>>,
    free: u64,
    next: Mutex<u32>,
}

impl ApfsRunner {
    fn new(free: u64) -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
            volumes: Mutex::new(Vec::new()),
            free,
            next: Mutex::new(7),
        })
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    fn volumes(&self) -> Vec<String> {
        self.volumes
            .lock()
            .unwrap()
            .iter()
            .map(|v| v.1.clone())
            .collect()
    }

    fn listing(&self) -> String {
        let volumes: Vec<_> = self
            .volumes
            .lock()
            .unwrap()
            .iter()
            .map(|(device, name, quota)| {
                serde_json::json!({"DeviceIdentifier": device, "Name": name,
                    "CapacityQuota": quota, "CapacityInUse": 0})
            })
            .collect();
        serde_json::json!({"Containers": [{"ContainerReference": "disk3",
            "CapacityFree": self.free, "Volumes": volumes}]})
        .to_string()
    }

    fn add(&self, argv: &[&str]) -> Result<String, String> {
        let gib: u64 = argv[7].trim_end_matches('g').parse().map_err(|_| "quota")?;
        let mut next = self.next.lock().unwrap();
        let device = format!("disk3s{next}");
        *next += 1;
        self.volumes
            .lock()
            .unwrap()
            .push((device.clone(), argv[5].to_string(), gib * GIB));
        Ok(format!("Disk from APFS operation: {device}\n"))
    }
}

impl Runner for ApfsRunner {
    fn run(&self, argv: &[&str], _timeout_secs: u64) -> Result<String, String> {
        if argv.first() == Some(&"/usr/bin/plutil") {
            return std::fs::read_to_string(argv[argv.len() - 1]).map_err(|e| e.to_string());
        }
        let line = argv.join(" ");
        self.calls.lock().unwrap().push(line.clone());
        match argv {
            ["mount"] | ["df", "-g"] => Ok(String::new()),
            ["diskutil", "apfs", "list", "-plist"] => Ok(self.listing()),
            ["df", "-P", _] => {
                Ok("Filesystem\n/dev/disk3s5 1 1 1 1% /System/Volumes/Data\n".into())
            }
            ["diskutil", "info", "-plist", "disk3s5"] => {
                Ok(r#"{"APFSContainerReference":"disk3"}"#.into())
            }
            [
                "diskutil",
                "apfs",
                "addVolume",
                "disk3",
                "APFS",
                _,
                "-quota",
                _,
                "-nomount",
            ] => self.add(argv),
            [
                "diskutil",
                "mount",
                "-mountOptions",
                "noowners",
                "-mountPoint",
                _,
                _,
            ] => Ok("mounted".into()),
            ["diskutil", "apfs", "deleteVolume", device] => {
                self.volumes.lock().unwrap().retain(|v| v.0 != *device);
                Ok("Finished".into())
            }
            _ => Err(format!("fake: unexpected `{line}`")),
        }
    }
}

fn apfs_provider(
    root: &Path,
    source: &Path,
    extra: &str,
    runner: Arc<ApfsRunner>,
) -> LocalProvider {
    let config: PoolConfig = toml::from_str(&format!(
        "provider = \"local\"\nslots = 2\n[local]\nroot = {:?}\nworktree_of = {:?}\n\
         [workspace]\nenabled = true\nkind = \"apfs\"\nsize_gib = 12\nmargin_gib = 6\n{extra}",
        root.to_string_lossy(),
        source.to_string_lossy()
    ))
    .unwrap();
    let context = crate::pool::runpod::HostContext {
        dir: root.parent().unwrap().join(".vivi"),
        mailspace: "ms".into(),
        pool: "pool".into(),
        pools: vec!["pool".into()],
    };
    LocalProvider::with_runner(&config, &context, runner).with_available_memory(u64::MAX)
}

fn idle_slot(root: &Path) -> String {
    let id = String::from("ms/pool/slot-1");
    let spec = CreateSpec {
        name: "ms-pool-slot-1".into(),
        image: String::new(),
        metadata: slot_metadata(&PoolId::new("ms", "", "pool"), 1, "main"),
        env: BTreeMap::new(),
        once_env: BTreeMap::new(),
        passthrough_env: Vec::new(),
        files: vec![("/work/boot.sh".into(), "exit 0\n".into())],
        command: vec!["bash".into(), "/work/boot.sh".into()],
        build_dir: None,
        tmpfs: String::new(),
    };
    slots::create(root, &id, &spec).unwrap();
    id
}

fn target(source: &Path) -> crate::pool::dispatch::target::JobTarget {
    let source = source.to_string_lossy().into_owned();
    crate::pool::dispatch::target::JobTarget {
        id: String::from("default"),
        source: source.clone(),
        cache_key: "a".repeat(64),
        local_source: Some(source),
        publish_remote: None,
        seed: Vec::new(),
        requires: Vec::new(),
        build_dir: None,
    }
}

#[test]
fn an_apfs_job_gets_a_quota_volume_recorded_in_the_apfs_marker() {
    let (_tmp, root) = root();
    let id = idle_slot(&root);
    let fixture = crate::pool::local_fixture::fixture();
    let start = crate::pool::local_fixture::git(&fixture.repo, &["rev-parse", "HEAD"]);
    let runner = ApfsRunner::new(100 * GIB);
    let provider = apfs_provider(
        &root,
        &fixture.repo,
        "container = \"disk3\"\n",
        runner.clone(),
    );

    provider
        .prepare_target_checkout(&id, &target(&fixture.repo), &start, "job-123456")
        .unwrap();

    let slot = root.join(&id);
    let mount = slot.join("targets").join("a".repeat(64));
    let marker = std::fs::read_to_string(slot.join("apfs")).unwrap();
    assert!(
        marker.contains("volume=vivi-ms-pool-slot-1-job-123456\n"),
        "{marker}"
    );
    assert!(marker.contains("volume_device=/dev/disk3s7\n"), "{marker}");
    assert!(marker.contains("size_gib=12\n"), "{marker}");
    assert!(marker.contains("container=disk3\n"), "{marker}");
    assert!(
        marker.contains(&format!("mount={}\n", mount.display())),
        "{marker}"
    );
    assert!(
        !slot.join("ramdisk").exists(),
        "the RAM marker is never written"
    );
    assert!(mount.join("repo/.git").is_file());
    assert_eq!(runner.volumes(), vec!["vivi-ms-pool-slot-1-job-123456"]);
    let calls = runner.calls();
    assert!(
        calls.iter().any(|c| c.ends_with("-quota 12g -nomount")),
        "{calls:?}"
    );
    assert!(
        !calls
            .iter()
            .any(|c| c.contains("reserve") || c.contains("hdiutil") || c.contains("eject"))
    );
}

#[test]
fn unclaim_deletes_the_volume_detaches_the_worktree_and_drops_the_marker() {
    let (_tmp, root) = root();
    let id = idle_slot(&root);
    let fixture = crate::pool::local_fixture::fixture();
    let start = crate::pool::local_fixture::git(&fixture.repo, &["rev-parse", "HEAD"]);
    let runner = ApfsRunner::new(100 * GIB);
    let provider = apfs_provider(&root, &fixture.repo, "", runner.clone());
    provider
        .prepare_target_checkout(&id, &target(&fixture.repo), &start, "job-1")
        .unwrap();
    let repo = crate::pool::local_git::Repo::new(&fixture.repo);
    let registered = repo.worktrees().unwrap().len();

    provider.unclaim(&id).unwrap();

    assert!(!root.join(&id).join("apfs").exists());
    assert!(runner.volumes().is_empty());
    assert_eq!(repo.worktrees().unwrap().len(), registered - 1);
    let calls = runner.calls();
    assert!(
        calls
            .iter()
            .any(|c| c == "diskutil apfs deleteVolume disk3s7"),
        "{calls:?}"
    );
    assert!(
        calls.iter().any(|c| c.starts_with("diskutil info -plist")),
        "container found from the slot"
    );
}

#[test]
fn an_apfs_job_waits_when_the_container_cannot_hold_its_quota() {
    let (_tmp, root) = root();
    let id = idle_slot(&root);
    let fixture = crate::pool::local_fixture::fixture();
    let start = crate::pool::local_fixture::git(&fixture.repo, &["rev-parse", "HEAD"]);
    let runner = ApfsRunner::new(10 * GIB);
    let provider = apfs_provider(
        &root,
        &fixture.repo,
        "container = \"disk3\"\n",
        runner.clone(),
    );

    let said = provider
        .prepare_target_checkout(&id, &target(&fixture.repo), &start, "job-1")
        .unwrap_err()
        .to_string();

    assert!(said.contains("apfs-wait"), "{said}");
    assert!(runner.volumes().is_empty());
    assert!(!runner.calls().iter().any(|c| c.contains("addVolume")));
    assert!(!root.join(&id).join("apfs").exists());
}

#[test]
fn a_malformed_apfs_marker_is_kept_and_nothing_is_deleted() {
    let (_tmp, root) = root();
    let id = idle_slot(&root);
    let slot = root.join(&id);
    let marker = "volume_device=/dev/disk3s5\nvolume=Data\nsize_gib=1\njob=j\nmount=/\n";
    std::fs::write(slot.join("apfs"), marker).unwrap();
    let runner = ApfsRunner::new(GIB);

    let said = super::teardown(&slot, &id, runner.as_ref())
        .unwrap_err()
        .to_string();

    assert!(said.contains("malformed APFS volume marker"), "{said}");
    assert!(slot.join("apfs").exists());
    assert!(runner.calls().is_empty());
}
