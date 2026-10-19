//! The provider through its trait, against directories and processes on this
//! machine.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::*;
use crate::pool::slot::{META_STATE, PoolId, STATE_READY, slot_metadata, slot_state};

const RAMDISK_SLOT_ID: &str = "ms/pool/slot-0";

fn provider(root: &std::path::Path) -> LocalProvider {
    provider_for(
        root.parent().unwrap(),
        root.to_str().unwrap(),
        "ms",
        "lanes-a",
    )
}

fn provider_for(
    project: &std::path::Path,
    root: &str,
    mailspace: &str,
    pool: &str,
) -> LocalProvider {
    let config: PoolConfig = toml::from_str(&format!(
        "provider = \"local\"\nslots = 2\n[local]\nroot = \"{root}\"\n"
    ))
    .unwrap();
    LocalProvider::new(
        &config,
        &super::super::runpod::HostContext {
            dir: project.join(".vivi"),
            mailspace: mailspace.into(),
            pool: pool.into(),
            pools: vec![pool.into()],
        },
    )
}

fn local_host_context(
    project: &std::path::Path,
    mailspace: &str,
    pool: &str,
) -> super::super::runpod::HostContext {
    super::super::runpod::HostContext {
        dir: project.join(".vivi"),
        mailspace: mailspace.into(),
        pool: pool.into(),
        pools: vec![pool.into()],
    }
}

fn spec(pool: &str, index: u32, boot: &str) -> CreateSpec {
    spec_for("ms", pool, index, boot)
}

fn spec_for(mailspace: &str, pool: &str, index: u32, boot: &str) -> CreateSpec {
    CreateSpec {
        name: format!("{mailspace}-{pool}-slot-{index}"),
        image: String::new(),
        metadata: slot_metadata(&PoolId::new(mailspace, "", pool), index, "main"),
        env: BTreeMap::new(),
        once_env: BTreeMap::new(),
        passthrough_env: vec!["IGNORED".into()],
        files: vec![("/work/boot.sh".into(), boot.into())],
        command: vec!["bash".into(), "/work/boot.sh".into()],
        build_dir: None,
        tmpfs: String::new(),
    }
}

fn until(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn root() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap().join("lanes");
    (dir, root)
}

#[test]
fn refuse_foreign_worktree_names_both_repositories_and_the_fix() {
    let configured = tempfile::tempdir().unwrap();
    let recorded = tempfile::tempdir().unwrap();
    let slot = tempfile::tempdir().unwrap();
    let marker = slot.path().join(slots::WORKTREE_OF_FILE);
    std::fs::write(&marker, format!("{}\n", recorded.path().display())).unwrap();
    let error = refuse_foreign_worktree("e2e/wt/slot-1", slot.path(), configured.path())
        .unwrap_err()
        .to_string();
    assert!(error.contains("worktree of"), "{error}");
    assert!(
        error.contains(&configured.path().display().to_string()),
        "{error}"
    );
    assert!(
        error.contains(&recorded.path().display().to_string()),
        "{error}"
    );
    assert!(error.contains("vivi pool destroy e2e/wt/slot-1"), "{error}");

    std::fs::write(&marker, format!("{}\n", configured.path().display())).unwrap();
    assert!(refuse_foreign_worktree("e2e/wt/slot-1", slot.path(), configured.path()).is_ok());
    std::fs::remove_file(marker).unwrap();
    assert!(refuse_foreign_worktree("e2e/wt/slot-1", slot.path(), configured.path()).is_ok());
}

fn plant_slot(root: &std::path::Path, id: &str, mailspace: &str, pool: &str) {
    let dir = root.join(id);
    std::fs::create_dir_all(dir.join("marks")).unwrap();
    std::fs::write(dir.join("marks/vivi_pool"), format!("{pool}\n")).unwrap();
    std::fs::write(
        dir.join("marks/vivi_pool_mailspace"),
        format!("{mailspace}\n"),
    )
    .unwrap();
    std::fs::write(dir.join("marks/vivi_pool_state"), "ready\n").unwrap();
}

#[test]
fn the_provider_says_what_a_directory_on_this_machine_is() {
    let (_tmp, root) = root();
    let p = provider(&root);
    assert!(!p.supplies_build_mount());
    assert!(p.build_persists());
    assert!(p.runs_unprivileged());
    assert_eq!(p.ensure_home().unwrap(), None);
    assert!(p.live_test_notice().contains(&root.display().to_string()));
    assert!(p.destroy_command("ms/p/slot-1").contains("rm -rf"));
}

#[test]
fn project_relative_root_lays_out_a_two_part_slot_id() {
    let project = tempfile::tempdir().unwrap();
    let provider = provider_for(project.path(), "pool", "space", "here");
    let id = provider
        .create(&spec_for("space", "here", 1, "exit 0\n"))
        .unwrap();
    assert_eq!(id, "here/slot-1");
    assert!(project.path().join("pool/here/slot-1").is_dir());
    assert_eq!(provider.list().unwrap()[0].id, id);
}

#[test]
fn absolute_root_keeps_the_mailspace_id_segment() {
    let project = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let provider = provider_for(
        project.path(),
        root.path().to_str().unwrap(),
        "space",
        "here",
    );
    let id = provider
        .create(&spec_for("space", "here", 1, "exit 0\n"))
        .unwrap();
    assert_eq!(id, "space/here/slot-1");
    assert!(root.path().join(&id).is_dir());
    assert_eq!(provider.list().unwrap()[0].id, id);
}

#[test]
fn listing_never_leaves_the_selected_pool_directory() {
    let project = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let provider = provider_for(
        project.path(),
        root.path().to_str().unwrap(),
        "space-id",
        "here",
    );
    provider
        .create(&spec_for("space-id", "here", 1, "exit 0\n"))
        .unwrap();
    plant_slot(
        root.path(),
        "space-id/here-fetch/slot-1",
        "space-id",
        "here-fetch",
    );
    plant_slot(root.path(), "other-id/here/slot-1", "other-id", "here");
    assert_eq!(
        provider
            .list()
            .unwrap()
            .iter()
            .map(|machine| machine.id.as_str())
            .collect::<Vec<_>>(),
        ["space-id/here/slot-1"]
    );
}

#[test]
fn a_created_slot_runs_its_boot_to_its_end_and_reads_stopped_with_an_exit_code() {
    let (_tmp, root) = root();
    let p = provider(&root);
    let id = p
        .create(&spec("lanes-a", 1, "echo booted > booted.txt\nexit 3\n"))
        .unwrap();
    assert_eq!(id, "ms/lanes-a/slot-1");
    until("the boot to end", || {
        p.machine(&id).unwrap().state == "stopped"
    });
    let record = p.machine(&id).unwrap();
    assert_eq!(record.exit_code, Some(3));
    assert_eq!(record.region, "local");
    let read = p
        .read_stopped(&id, &["/work/booted.txt", "/work/absent"])
        .unwrap()
        .unwrap();
    assert_eq!(read["/work/booted.txt"], b"booted\n");
    assert_eq!(read.len(), 1);
    assert_eq!(p.list().unwrap().len(), 1);
}

#[test]
fn start_and_stop_are_one_running_process_tree_and_nothing_else() {
    let (_tmp, root) = root();
    let p = provider(&root);
    let id = p.create(&spec("lanes-a", 1, "sleep 60\n")).unwrap();
    assert_eq!(p.machine(&id).unwrap().state, "started");
    p.stop(&id).unwrap();
    assert_eq!(p.machine(&id).unwrap().state, "stopped");
    assert_eq!(
        p.machine(&id).unwrap().exit_code,
        None,
        "a stop leaves no exit code"
    );
    p.start(&id).unwrap();
    assert_eq!(p.machine(&id).unwrap().state, "started");
    p.destroy(&id).unwrap();
    assert_eq!(p.machine(&id).unwrap().state, "destroyed");
    assert!(!root.exists());
}

#[test]
fn exec_runs_in_the_slot_with_work_set_and_returns_the_scripts_own_exit_code() {
    let (_tmp, root) = root();
    let p = provider(&root);
    let id = p.create(&spec("lanes-a", 1, "exit 0\n")).unwrap();
    let out = p
        .exec(&id, "pwd -P; echo \"$WORK\"; echo oops >&2; exit 4", 5)
        .unwrap();
    let dir = root.join(&id);
    assert_eq!(out.stdout, format!("{0}\n{0}\n", dir.display()));
    assert_eq!(out.stderr.trim(), "oops");
    assert_eq!(out.exit_code, 4);
}

#[test]
fn a_slot_is_claimed_once_sealed_ready_and_unclaimed_to_be_free_again() {
    let (_tmp, root) = root();
    let p = provider(&root);
    let id = p.create(&spec("lanes-a", 1, "exit 0\n")).unwrap();
    until("the boot to end", || {
        p.machine(&id).unwrap().state == "stopped"
    });
    assert!(
        p.claim(&id, "task-1", 1).unwrap().is_none(),
        "not ready yet"
    );
    p.seal(&id, &["GH_TOKEN"], META_STATE, STATE_READY).unwrap();
    assert_eq!(slot_state(&p.machine(&id).unwrap()), "free");
    let hold = p.claim(&id, "task-1", 7).unwrap().unwrap();
    assert!(p.claim(&id, "task-2", 8).unwrap().is_none());
    p.put_file(&id, &hold, "/work/job.tgz", b"x").unwrap();
    p.release(&id, &hold).unwrap();
    assert_eq!(slot_state(&p.machine(&id).unwrap()), "finished-uncollected");
    p.unclaim(&id).unwrap();
    assert_eq!(slot_state(&p.machine(&id).unwrap()), "free");
}

#[test]
fn a_slot_that_is_not_there_reads_as_gone_and_a_bad_id_never_reaches_the_disk() {
    let (_tmp, root) = root();
    let p = provider(&root);
    let gone = p.machine("ms/lanes-a/slot-9").unwrap();
    assert_eq!(gone.state, "destroyed");
    for bad in ["../x", "ms/lanes-a", "/etc", "ms/lanes-a/slot-1/../.."] {
        assert!(p.machine(bad).is_err(), "{bad}");
        assert!(p.start(bad).is_err(), "{bad}");
        assert!(p.destroy(bad).is_err(), "{bad}");
    }
    assert!(p.start("ms/lanes-a/slot-9").is_err());
}

#[test]
fn harness_names_come_from_the_environment_of_this_process_and_only_names_are_reported() {
    let (_tmp, root) = root();
    let p = provider(&root);
    let names = vec![
        "PATH".to_string(),
        "VIVI_TEST_SURELY_UNSET_NAME".to_string(),
    ];
    assert_eq!(
        p.harness_env_gaps(&names).unwrap(),
        vec!["VIVI_TEST_SURELY_UNSET_NAME"]
    );
    assert_eq!(
        p.env_gaps_at_dispatch(&names).unwrap(),
        vec!["VIVI_TEST_SURELY_UNSET_NAME"]
    );
    let hint = p.env_gap_hint(&names[1..]);
    assert!(
        hint.contains("VIVI_TEST_SURELY_UNSET_NAME") && hint.contains("no value is stored"),
        "{hint}"
    );
}

#[test]
fn creating_a_slot_whose_boot_cannot_start_removes_it_again() {
    let (_tmp, root) = root();
    let p = provider(&root);
    let mut bad = spec("lanes-a", 1, "exit 0\n");
    bad.metadata.remove("vivi_pool_slot");
    assert!(
        p.create(&bad)
            .unwrap_err()
            .to_string()
            .contains("lacks vivi_pool_slot")
    );
    assert!(p.list().unwrap().is_empty());
}

// ------------------------------------------------------ RAM disk lifecycle

struct RamdiskTestRunner {
    mount: PathBuf,
    attached: Mutex<bool>,
    keep_attached: bool,
    calls: Mutex<Vec<String>>,
    plist_request: Mutex<Option<String>>,
}

impl RamdiskTestRunner {
    fn new(mount: &Path, keep_attached: bool) -> Self {
        Self {
            mount: mount.to_path_buf(),
            attached: Mutex::new(true),
            keep_attached,
            calls: Mutex::new(Vec::new()),
            plist_request: Mutex::new(None),
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl crate::pool::ramdisk::Runner for RamdiskTestRunner {
    fn run(&self, argv: &[&str], _timeout_secs: u64) -> Result<String, String> {
        let line = argv.join(" ");
        self.calls.lock().unwrap().push(line.clone());
        match line.as_str() {
            "mount" => {
                if *self.attached.lock().unwrap() {
                    Ok(format!(
                        "/dev/disk7s1 on {} (apfs, local)\n",
                        self.mount.display()
                    ))
                } else {
                    Ok(String::new())
                }
            }
            "df -g" => {
                if *self.attached.lock().unwrap() {
                    Ok(format!(
                        "Filesystem 1G-blocks Used Available Capacity Mounted on\n\
                         /dev/disk7s1 8 1 7 4% {}\n",
                        self.mount.display()
                    ))
                } else {
                    Ok(String::new())
                }
            }
            _ if argv.len() == 4 && argv[0] == "diskutil" && argv[1] == "info" => {
                *self.plist_request.lock().unwrap() = Some(argv[3].to_string());
                Ok("plist".to_string())
            }
            _ if argv.len() == 5
                && argv[0] == "diskutil"
                && argv[1] == "apfs"
                && argv[2] == "list" =>
            {
                *self.plist_request.lock().unwrap() = Some(format!("container:{}", argv[4]));
                Ok("plist".to_string())
            }
            _ if argv.first() == Some(&"/usr/bin/plutil") => {
                let request = self
                    .plist_request
                    .lock()
                    .unwrap()
                    .clone()
                    .unwrap_or_default();
                let json = if request == "/dev/disk7s1" {
                    serde_json::json!({
                        "DeviceIdentifier": "disk7s1",
                        "APFSContainerReference": "disk7",
                    })
                } else if request == "container:disk7" {
                    serde_json::json!({
                        "Containers": [{
                            "ContainerReference": "disk7",
                            "PhysicalStores": [{"DeviceIdentifier": "disk7"}],
                            "Volumes": [{
                                "Name": "vivi-ms-pool-slot-0-build",
                                "DeviceIdentifier": "disk7s1",
                            }],
                        }],
                    })
                } else {
                    return Err(format!("unexpected plist request: {request}"));
                };
                Ok(json.to_string())
            }
            "diskutil eject /dev/disk7" => {
                if !self.keep_attached {
                    *self.attached.lock().unwrap() = false;
                }
                Ok(String::from("disk7 ejected"))
            }
            _ => Err(format!("unexpected RAM disk command: {line}")),
        }
    }
}

type AttachedDisks = Vec<(String, String, u64)>;

struct LocalPoolRamRunner {
    attached: Mutex<AttachedDisks>,
    calls: Mutex<Vec<String>>,
    volume: Mutex<Option<String>>,
    plist_request: Mutex<Option<String>>,
}

impl LocalPoolRamRunner {
    fn new(attached: &[(&str, &str, u64)]) -> Self {
        Self {
            attached: Mutex::new(
                attached
                    .iter()
                    .map(|(device, mount, gib)| ((*device).to_string(), (*mount).to_string(), *gib))
                    .collect(),
            ),
            calls: Mutex::new(Vec::new()),
            volume: Mutex::new(None),
            plist_request: Mutex::new(None),
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    fn attached(&self) -> AttachedDisks {
        self.attached.lock().unwrap().clone()
    }
}

impl crate::pool::ramdisk::Runner for LocalPoolRamRunner {
    fn run(&self, argv: &[&str], _timeout_secs: u64) -> Result<String, String> {
        let line = argv.join(" ");
        self.calls.lock().unwrap().push(line.clone());
        match line.as_str() {
            "mount" => Ok(self
                .attached()
                .iter()
                .map(|(device, mount, _)| format!("{device} on {mount} (apfs, local)"))
                .collect::<Vec<_>>()
                .join("\n")),
            "df -g" => Ok(self
                .attached()
                .iter()
                .map(|(device, mount, gib)| format!("{device} {gib} 1 1 4% {mount}"))
                .collect::<Vec<_>>()
                .join("\n")),
            _ if line.starts_with("hdiutil attach -nomount ram://") => {
                Ok(String::from("/dev/disk13\n"))
            }
            _ if argv.len() == 5 && argv[0] == "diskutil" && argv[1] == "eraseVolume" => {
                *self.volume.lock().unwrap() = Some(argv[3].to_string());
                Ok(String::new())
            }
            _ if argv.len() == 4 && argv[0] == "diskutil" && argv[1] == "info" => {
                *self.plist_request.lock().unwrap() = Some(argv[3].to_string());
                Ok("plist".to_string())
            }
            _ if argv.len() == 5
                && argv[0] == "diskutil"
                && argv[1] == "apfs"
                && argv[2] == "list" =>
            {
                *self.plist_request.lock().unwrap() = Some(format!("container:{}", argv[4]));
                Ok("plist".to_string())
            }
            "diskutil unmount /dev/disk14s1" => Ok(String::new()),
            _ if argv.first() == Some(&"/usr/bin/plutil") => {
                let volume = self.volume.lock().unwrap().clone().unwrap_or_default();
                let request = self
                    .plist_request
                    .lock()
                    .unwrap()
                    .clone()
                    .unwrap_or_default();
                let json = if request == "/dev/disk13" {
                    serde_json::json!({
                        "DeviceIdentifier": "disk13",
                        "APFSContainerReference": "disk14",
                    })
                } else if request == "/dev/disk14s1" {
                    serde_json::json!({
                        "DeviceIdentifier": "disk14s1",
                        "APFSContainerReference": "disk14",
                        "MountPoint": format!("/Volumes/{volume}"),
                    })
                } else if request == "container:disk14" {
                    serde_json::json!({
                        "Containers": [{
                            "ContainerReference": "disk14",
                            "PhysicalStores": [{"DeviceIdentifier": "disk13"}],
                            "Volumes": [{
                                "Name": volume,
                                "DeviceIdentifier": "disk14s1",
                            }],
                        }],
                    })
                } else {
                    return Err(format!("unexpected plist request: {request}"));
                };
                Ok(json.to_string())
            }
            _ if argv.len() == 5 && argv[0] == "diskutil" && argv[1] == "mount" => {
                self.attached
                    .lock()
                    .unwrap()
                    .push((argv[4].to_string(), argv[3].to_string(), 8));
                Ok(String::new())
            }
            _ if argv.len() == 3 && argv[0] == "diskutil" && argv[1] == "eject" => {
                let device = argv[2];
                self.attached.lock().unwrap().retain(|(attached, _, _)| {
                    attached != device
                        && !attached.starts_with(&format!("{device}s"))
                        && !(device == "/dev/disk13" && attached == "/dev/disk14s1")
                });
                Ok(String::new())
            }
            _ => Err(format!("unexpected RAM disk command: {line}")),
        }
    }
}

fn local_ramdisk_provider(
    root: &Path,
    size_gib: u64,
    runner: Arc<LocalPoolRamRunner>,
    available: u64,
) -> LocalProvider {
    let config: PoolConfig = toml::from_str(&format!(
        "provider = \"local\"\nslots = 2\n[local]\nroot = \"{}\"\n\
         [ramdisk]\nenabled = true\nsize_gib = {size_gib}\nmargin_gib = 6\n",
        root.display()
    ))
    .unwrap();
    let context = local_host_context(root.parent().unwrap(), "ms", "pool");
    LocalProvider::with_runner(&config, &context, runner).with_available_memory(available)
}

fn local_worktree_ramdisk_provider(
    root: &Path,
    source: &Path,
    runner: Arc<LocalPoolRamRunner>,
) -> LocalProvider {
    let config: PoolConfig = toml::from_str(&format!(
        "provider = \"local\"\nslots = 2\n[local]\nroot = {:?}\nworktree_of = {:?}\n\
         [ramdisk]\nenabled = true\nsize_gib = 8\nmargin_gib = 6\n",
        root.to_string_lossy(),
        source.to_string_lossy()
    ))
    .unwrap();
    let context = local_host_context(root.parent().unwrap(), "ms", "pool");
    LocalProvider::with_runner(&config, &context, runner).with_available_memory(u64::MAX)
}

fn local_target(
    cache_key: char,
    source: Option<&Path>,
) -> super::super::dispatch::target::JobTarget {
    let source = source.map(|path| path.to_string_lossy().into_owned());
    super::super::dispatch::target::JobTarget {
        id: String::from("default"),
        source: source.clone().unwrap_or_default(),
        cache_key: cache_key.to_string().repeat(64),
        local_source: source,
        publish_remote: None,
        seed: Vec::new(),
        requires: Vec::new(),
        build_dir: None,
    }
}

fn create_idle_slot(root: &Path) -> String {
    let id = String::from("ms/pool/slot-1");
    slots::create(root, &id, &spec("pool", 1, "exit 0\n")).unwrap();
    id
}

#[test]
fn local_provider_mounts_the_target_worktree_on_a_fresh_ram_disk() {
    let (_tmp, root) = root();
    let id = create_idle_slot(&root);
    let fixture = crate::pool::local_fixture::fixture();
    let start = crate::pool::local_fixture::git(&fixture.repo, &["rev-parse", "HEAD"]);
    let runner = Arc::new(LocalPoolRamRunner::new(&[]));
    let provider = local_ramdisk_provider(&root, 8, runner.clone(), u64::MAX);
    let target = local_target('a', Some(&fixture.repo));

    provider
        .prepare_target_checkout(&id, &target, &start, "job-123456")
        .unwrap();

    let slot = root.join(&id);
    let mount = slot.join("targets").join(&target.cache_key);
    let marker = std::fs::read_to_string(slot.join("ramdisk")).unwrap();
    assert!(marker.contains("volume=vivi-ms-pool-slot-1-job-123456\n"));
    assert!(marker.contains("device=/dev/disk13\n"));
    assert!(marker.contains("volume_device=/dev/disk14s1\n"));
    assert!(marker.contains("size_gib=8\n"));
    assert!(marker.contains(&format!("mount={}\n", mount.display())));
    assert!(mount.join("repo/.git").is_file());
    assert_eq!(runner.attached().len(), 1);
    assert!(
        runner
            .calls()
            .iter()
            .any(|call| call.starts_with("hdiutil attach -nomount ram://"))
    );
}

#[test]
fn local_gate_pool_mounts_the_configured_full_size_without_a_local_source() {
    let (_tmp, root) = root();
    let id = create_idle_slot(&root);
    let slot = root.join(&id);
    let fixture = crate::pool::local_fixture::fixture();
    crate::pool::local_git::Repo::new(&fixture.repo)
        .add_worktree(&slot.join(slots::CHECKOUT), "main")
        .unwrap();
    let runner = Arc::new(LocalPoolRamRunner::new(&[]));
    let provider = local_ramdisk_provider(&root, 32, runner.clone(), u64::MAX);

    let workspace = provider
        .prepare_job(&id, None, "tip", "gate-123456")
        .unwrap()
        .unwrap();

    let mount = slot.join("targets").join(&workspace.cache_key);
    assert!(workspace.local_source.is_none());
    let marker = std::fs::read_to_string(slot.join("ramdisk")).unwrap();
    assert!(marker.contains("volume=vivi-ms-pool-slot-1-gate-123456\n"));
    assert!(marker.contains("size_gib=32\n"));
    assert!(marker.contains(&format!("mount={}\n", mount.display())));
    assert!(marker.contains("source=\n"));
    assert!(mount.is_dir());
    assert!(slot.join(slots::CHECKOUT).join(".git").is_file());
    assert_eq!(
        crate::pool::local_git::Repo::new(&fixture.repo)
            .worktrees()
            .unwrap()
            .len(),
        2,
        "the seeded checkout remains registered"
    );
    assert_eq!(runner.attached().len(), 1);
}

#[test]
fn a_project_relative_pool_two_part_slot_id_mounts_and_tears_down_ram() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path().join("poolroot");
    let id = "pool/slot-1";
    let config: PoolConfig = toml::from_str(
        "provider = \"local\"\nslots = 2\n[local]\nroot = \"poolroot\"\n\
         [ramdisk]\nenabled = true\nsize_gib = 1\nmargin_gib = 0\n",
    )
    .unwrap();
    let runner = Arc::new(LocalPoolRamRunner::new(&[]));
    let context = local_host_context(project.path(), "ms", "pool");
    let provider = LocalProvider::with_runner(&config, &context, runner.clone())
        .with_available_memory(u64::MAX);
    slots::create(&root, id, &spec("pool", 1, "exit 0\n")).unwrap();

    let workspace = provider
        .prepare_job(id, None, "tip", "two-part-job")
        .unwrap()
        .unwrap();
    let slot = root.join(id);
    let marker = std::fs::read_to_string(slot.join("ramdisk")).unwrap();
    assert!(marker.contains("volume=vivi-pool-slot-1-two-part-job\n"));
    assert!(slot.join("targets").join(workspace.cache_key).is_dir());
    assert_eq!(runner.attached().len(), 1);

    teardown_ramdisk(&slot, id, &*runner).unwrap();

    assert!(!slot.join("ramdisk").exists());
    assert!(runner.attached().is_empty());
}

#[test]
fn a_no_target_worktree_job_runs_under_ram_and_preserves_the_seeded_checkout() {
    let (_tmp, root) = root();
    let id = create_idle_slot(&root);
    let slot = root.join(&id);
    let fixture = crate::pool::local_fixture::fixture();
    let repo = crate::pool::local_git::Repo::new(&fixture.repo);
    repo.add_worktree(&slot.join(slots::CHECKOUT), "main")
        .unwrap();
    let start = crate::pool::local_fixture::git(&fixture.repo, &["rev-parse", "main"]);
    let runner = Arc::new(LocalPoolRamRunner::new(&[]));
    let provider = local_worktree_ramdisk_provider(&root, &fixture.repo, runner.clone());

    let workspace = provider
        .prepare_job(&id, None, &start, "job-ram-worktree")
        .unwrap()
        .unwrap();

    let mount = slot.join("targets").join(&workspace.cache_key);
    let job_worktree = mount.join(slots::CHECKOUT);
    assert_eq!(
        workspace.local_source.as_deref(),
        Some(fixture.repo.to_str().unwrap())
    );
    assert!(job_worktree.join(".git").is_file());
    assert!(slot.join(slots::CHECKOUT).join(".git").is_file());
    assert_eq!(repo.worktrees().unwrap().len(), 3);
    assert!(
        std::fs::read_to_string(slot.join("ramdisk"))
            .unwrap()
            .contains(&format!("mount={}\n", mount.display()))
    );

    teardown_ramdisk(&slot, &id, &*runner).unwrap();

    assert!(!job_worktree.exists());
    assert!(slot.join(slots::CHECKOUT).join(".git").is_file());
    assert_eq!(repo.worktrees().unwrap().len(), 2);
}

#[test]
fn local_provider_waits_for_memory_without_ssd_fallback() {
    let (_tmp, root) = root();
    let id = create_idle_slot(&root);
    let runner = Arc::new(LocalPoolRamRunner::new(&[
        ("/dev/disk3s1", "/Volumes/vivi-held-a", 32),
        ("/dev/disk5s1", "/Volumes/vivi-held-b", 32),
    ]));
    let available = 70 * 1024 * 1024 * 1024;
    let provider = local_ramdisk_provider(&root, 8, runner.clone(), available);
    let target = local_target('c', None);

    let error = provider
        .prepare_target_checkout(&id, &target, "tip", "job-wait")
        .unwrap_err();

    assert!(
        matches!(&error, VivariumError::Message(message) if message.starts_with("ramdisk-wait:")),
        "{error}"
    );
    assert!(!root.join(&id).join("ramdisk").exists());
    assert!(
        !runner
            .calls()
            .iter()
            .any(|call| call.starts_with("hdiutil attach"))
    );
}

#[test]
fn ramdisk_admission_lock_is_shared_across_project_roots() {
    let (_project_a, root_a) = root();
    let (_project_b, root_b) = root();
    let provider_a = provider(&root_a);
    let provider_b = provider(&root_b);
    assert_ne!(provider_a.root(), provider_b.root());

    let held = crate::pool::ramdisk::lock_admission().expect("host RAM admission lock");
    assert!(
        crate::pool::ramdisk::try_lock_admission()
            .expect("try host RAM admission lock")
            .is_none(),
        "a second project must contend on the same host lock"
    );
    drop(held);
    assert!(
        crate::pool::ramdisk::try_lock_admission()
            .expect("retry host RAM admission lock")
            .is_some(),
        "the lock is released when its owner drops the file"
    );
}

fn ramdisk_marker(slot: &Path, mount: &Path, source: &Path) {
    std::fs::create_dir_all(mount).unwrap();
    std::fs::write(
        slot.join("ramdisk"),
        format!(
            "device=/dev/disk7\nvolume_device=/dev/disk7s1\nvolume=vivi-ms-pool-slot-0-build\
             \nsize_gib=8\njob=build\nmount={}\nsource={}\n",
            mount.display(),
            source.display()
        ),
    )
    .unwrap();
}

#[test]
fn ramdisk_teardown_removes_its_worktree_ejects_and_clears_the_marker() {
    let fixture = crate::pool::local_fixture::fixture();
    let slot = tempfile::tempdir().unwrap();
    let mount = slot.path().join("targets").join("cache-key");
    let worktree = mount.join("repo");
    std::fs::create_dir_all(worktree.parent().unwrap()).unwrap();
    let repo = crate::pool::local_git::Repo::new(&fixture.repo);
    repo.add_worktree(&worktree, "main").unwrap();
    ramdisk_marker(slot.path(), &mount, &fixture.repo);
    let runner = RamdiskTestRunner::new(&mount, false);

    teardown_ramdisk(slot.path(), RAMDISK_SLOT_ID, &runner).unwrap();

    assert!(!slot.path().join("ramdisk").exists());
    assert!(!worktree.exists());
    assert_eq!(repo.worktrees().unwrap().len(), 1);
    assert_eq!(
        runner
            .calls()
            .iter()
            .filter(|call| *call == "diskutil eject /dev/disk7")
            .count(),
        1
    );
}

#[test]
fn ramdisk_teardown_failure_keeps_the_marker_for_retry() {
    let fixture = crate::pool::local_fixture::fixture();
    let slot = tempfile::tempdir().unwrap();
    let mount = slot.path().join("targets").join("cache-key");
    ramdisk_marker(slot.path(), &mount, &fixture.repo);
    let runner = RamdiskTestRunner::new(&mount, true);

    let error = teardown_ramdisk(slot.path(), RAMDISK_SLOT_ID, &runner).unwrap_err();

    assert!(error.to_string().contains("still attached"), "{error}");
    assert!(slot.path().join("ramdisk").is_file());
    assert!(
        runner
            .calls()
            .iter()
            .any(|call| call == "diskutil eject /dev/disk7")
    );
}

#[test]
fn a_marker_for_another_device_is_not_ejected_or_removed() {
    let fixture = crate::pool::local_fixture::fixture();
    let slot = tempfile::tempdir().unwrap();
    let mount = slot.path().join("targets").join("cache-key");
    ramdisk_marker(slot.path(), &mount, &fixture.repo);
    let marker = std::fs::read_to_string(slot.path().join("ramdisk")).unwrap();
    std::fs::write(
        slot.path().join("ramdisk"),
        marker.replace("device=/dev/disk7\n", "device=/dev/disk8\n"),
    )
    .unwrap();
    let runner = RamdiskTestRunner::new(&mount, false);

    assert!(teardown_ramdisk(slot.path(), RAMDISK_SLOT_ID, &runner).is_err());

    assert!(slot.path().join("ramdisk").is_file());
    assert!(
        !runner
            .calls()
            .iter()
            .any(|call| call.starts_with("diskutil eject "))
    );
}

#[test]
fn teardown_keeps_the_disk_when_the_recorded_repository_does_not_own_the_worktree() {
    let actual = crate::pool::local_fixture::fixture();
    let recorded = crate::pool::local_fixture::fixture();
    let slot = tempfile::tempdir().unwrap();
    let mount = slot.path().join("targets").join("cache-key");
    let worktree = mount.join("repo");
    std::fs::create_dir_all(worktree.parent().unwrap()).unwrap();
    let repo = crate::pool::local_git::Repo::new(&actual.repo);
    repo.add_worktree(&worktree, "main").unwrap();
    ramdisk_marker(slot.path(), &mount, &recorded.repo);
    let runner = RamdiskTestRunner::new(&mount, false);

    let error = teardown_ramdisk(slot.path(), RAMDISK_SLOT_ID, &runner).unwrap_err();

    assert!(error.to_string().contains("belongs to another repository"));
    assert!(slot.path().join("ramdisk").is_file());
    assert!(worktree.join(".git").is_file());
    assert_eq!(repo.worktrees().unwrap().len(), 2);
    assert!(
        !runner
            .calls()
            .iter()
            .any(|call| call.starts_with("diskutil eject "))
    );
}

#[test]
fn malformed_ramdisk_marker_causes_no_runner_or_worktree_operations() {
    let slot = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(slot.path().join("targets/cache-key")).unwrap();
    std::fs::write(slot.path().join("ramdisk"), "volume=vivi-unknown\n").unwrap();
    let runner = RamdiskTestRunner::new(&slot.path().join("targets/cache-key"), false);

    assert!(teardown_ramdisk(slot.path(), RAMDISK_SLOT_ID, &runner).is_err());

    assert!(runner.calls().is_empty());
    assert!(slot.path().join("ramdisk").is_file());
}

#[test]
fn root_seed_checkout_marker_is_rejected_without_removing_the_worktree() {
    let fixture = crate::pool::local_fixture::fixture();
    let slot = tempfile::tempdir().unwrap();
    let worktree = slot.path().join(slots::CHECKOUT);
    crate::pool::local_git::Repo::new(&fixture.repo)
        .add_worktree(&worktree, "main")
        .unwrap();
    ramdisk_marker(slot.path(), &worktree, &fixture.repo);
    let runner = RamdiskTestRunner::new(&worktree, false);

    assert!(teardown_ramdisk(slot.path(), RAMDISK_SLOT_ID, &runner).is_err());

    assert!(worktree.join(".git").is_file());
    assert_eq!(
        crate::pool::local_git::Repo::new(&fixture.repo)
            .worktrees()
            .unwrap()
            .len(),
        2,
        "the seeded slot checkout remains registered"
    );
    assert!(runner.calls().is_empty());
    assert!(slot.path().join("ramdisk").is_file());
}

#[test]
fn symlinked_ramdisk_marker_is_preserved_without_mount_operations() {
    use std::os::unix::fs::symlink;

    let slot = tempfile::tempdir().unwrap();
    let external = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(external.path(), "volume=vivi-unknown\n").unwrap();
    symlink(external.path(), slot.path().join("ramdisk")).unwrap();
    let runner = RamdiskTestRunner::new(&slot.path().join("targets/cache-key"), false);

    assert!(teardown_ramdisk(slot.path(), RAMDISK_SLOT_ID, &runner).is_err());

    assert!(
        slot.path()
            .join("ramdisk")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(runner.calls().is_empty());
}
