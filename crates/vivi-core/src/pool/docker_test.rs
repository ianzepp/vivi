use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::json;

use super::*;
use crate::pool::slot::{META_STATE, STATE_READY};

/// One container of the simulated daemon.
#[derive(Clone)]
struct Sim {
    id: String,
    name: String,
    image: String,
    labels: BTreeMap<String, String>,
    env: Vec<String>,
    status: String,
    exit: i64,
    files: BTreeMap<String, Vec<u8>>,
}

/// A daemon that keeps the one promise the claim leans on: a name is held by
/// at most one container, decided under one lock.
#[derive(Default)]
struct Daemon {
    containers: Mutex<Vec<Sim>>,
    calls: Mutex<Vec<Vec<String>>>,
    /// The manifest architecture inspect reports, when the daemon has one.
    manifest_arch: Option<&'static str>,
}

struct Shared(Arc<Daemon>);

impl Runner for Shared {
    fn run(
        &self,
        args: &[String],
        stdin: Option<&[u8]>,
        _timeout: Duration,
    ) -> Result<RunOutput, VivariumError> {
        self.0.calls.lock().unwrap().push(args.to_vec());
        Ok(self.0.handle(args, stdin))
    }
}

fn reply(code: i32, stdout: &str, stderr: &str) -> RunOutput {
    RunOutput {
        code,
        stdout: stdout.as_bytes().to_vec(),
        stderr: stderr.to_string(),
    }
}

impl Daemon {
    fn handle(&self, args: &[String], stdin: Option<&[u8]>) -> RunOutput {
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        match a[0] {
            "image" if a.contains(&"{{.Architecture}}") => reply(0, "", ""),
            "image" => reply(0, "sha256:abc", ""),
            "info" => reply(0, "aarch64", ""),
            "ps" => self.ps(),
            "inspect" => self.inspect(&a[3..]),
            "create" => self.create(&a),
            "cp" => self.cp(&a, stdin),
            "start" => self.set_status(a[1], "running", 0),
            "stop" => self.set_status(a[3], "exited", 0),
            "rm" => self.remove(a[3]),
            "exec" => reply(
                1,
                "",
                "Error response from daemon: container is not running",
            ),
            other => reply(2, "", &format!("sim: unknown verb {other}")),
        }
    }

    /// The 64-digit id a real daemon reports for a container.
    fn full(c: &Sim) -> String {
        c.id.repeat(6)[..64].to_string()
    }

    fn find<'a>(all: &'a mut [Sim], key: &str) -> Option<&'a mut Sim> {
        all.iter_mut()
            .find(|c| c.name == key || (!key.is_empty() && Self::full(c).starts_with(key)))
    }

    fn ps(&self) -> RunOutput {
        let all = self.containers.lock().unwrap();
        let ids: Vec<String> = all
            .iter()
            .filter(|c| c.labels.contains_key("vivi_pool"))
            .map(Self::full)
            .collect();
        reply(0, &ids.join("\n"), "")
    }

    fn inspect(&self, keys: &[&str]) -> RunOutput {
        let mut all = self.containers.lock().unwrap();
        let mut found = Vec::new();
        let mut missing = Vec::new();
        for key in keys {
            match Self::find(&mut all, key) {
                Some(c) => found.push(self.json_of(c)),
                None => missing.push(format!(
                    "Error response from daemon: No such container: {key}"
                )),
            }
        }
        let text = if found.is_empty() {
            String::new()
        } else {
            json!(found).to_string()
        };
        reply(i32::from(!missing.is_empty()), &text, &missing.join("\n"))
    }

    fn json_of(&self, c: &Sim) -> serde_json::Value {
        let mut v = json!({
            "Id": Self::full(c),
            "Name": format!("/{}", c.name),
            "Created": "2026-10-11T00:00:00Z",
            "State": {"Status": c.status, "ExitCode": c.exit},
            "Config": {"Image": c.image, "Labels": c.labels, "Env": c.env},
        });
        if let Some(arch) = self.manifest_arch {
            v["ImageManifestDescriptor"] = json!({"platform": {"architecture": arch}});
        }
        v
    }

    fn create(&self, a: &[&str]) -> RunOutput {
        let mut all = self.containers.lock().unwrap();
        let name = a[a.iter().position(|x| *x == "--name").unwrap() + 1];
        if all.iter().any(|c| c.name == name) {
            return reply(
                1,
                "",
                &format!(
                    "Error response from daemon: Conflict. The container name \"/{name}\" is already in use by container \"x\"."
                ),
            );
        }
        let (mut labels, mut env, mut image) = (BTreeMap::new(), Vec::new(), String::new());
        let mut i = 1;
        while i < a.len() {
            match a[i] {
                "--label" => {
                    let (k, v) = a[i + 1].split_once('=').unwrap();
                    labels.insert(k.to_string(), v.to_string());
                    i += 2;
                }
                "-e" => {
                    env.push(a[i + 1].to_string());
                    i += 2;
                }
                "--name" | "--restart" | "--memory" | "--cpus" | "--tmpfs" => i += 2,
                "--init" => i += 1,
                _ => {
                    image = a[i].to_string();
                    break;
                }
            }
        }
        let id = format!("{:012x}", all.len() + 0x00ab_c000);
        all.push(Sim {
            id: id.clone(),
            name: name.to_string(),
            image,
            labels,
            env,
            status: "created".into(),
            exit: 0,
            files: BTreeMap::new(),
        });
        reply(0, &id.repeat(6)[..64], "")
    }

    fn cp(&self, a: &[&str], stdin: Option<&[u8]>) -> RunOutput {
        let mut all = self.containers.lock().unwrap();
        if a[1] == "-" {
            let key = a[2].trim_end_matches(":/");
            let Some(c) = Self::find(&mut all, key) else {
                return reply(
                    1,
                    "",
                    &format!("Error response from daemon: No such container: {key}"),
                );
            };
            for f in tar::read(stdin.unwrap_or_default()).unwrap() {
                c.files.insert(format!("/{}", f.path), f.content);
            }
            return reply(0, "", "");
        }
        let (key, path) = a[1].split_once(':').unwrap();
        let Some(c) = Self::find(&mut all, key) else {
            return reply(
                1,
                "",
                &format!("Error response from daemon: No such container: {key}"),
            );
        };
        let base = path.rsplit('/').next().unwrap();
        let mut out: Vec<(String, Vec<u8>)> = Vec::new();
        for (p, bytes) in &c.files {
            if p == path {
                out.push((format!("/{base}"), bytes.clone()));
            } else if let Some(rest) = p.strip_prefix(&format!("{path}/")) {
                out.push((format!("/{base}/{rest}"), bytes.clone()));
            }
        }
        if out.is_empty() {
            return reply(
                1,
                "",
                &format!(
                    "Error response from daemon: Could not find the file {path} in container {key}"
                ),
            );
        }
        RunOutput {
            code: 0,
            stdout: tar::build(&out).unwrap(),
            stderr: String::new(),
        }
    }

    fn set_status(&self, key: &str, status: &str, exit: i64) -> RunOutput {
        let mut all = self.containers.lock().unwrap();
        match Self::find(&mut all, key) {
            Some(c) => {
                c.status = status.into();
                c.exit = exit;
                reply(0, key, "")
            }
            None => reply(
                1,
                "",
                &format!("Error response from daemon: No such container: {key}"),
            ),
        }
    }

    fn remove(&self, key: &str) -> RunOutput {
        let mut all = self.containers.lock().unwrap();
        let before = all.len();
        all.retain(|c| c.name != key && !Self::full(c).starts_with(key));
        if all.len() == before {
            return reply(
                1,
                "",
                &format!("Error response from daemon: No such container: {key}"),
            );
        }
        reply(0, key, "")
    }

    fn names(&self) -> Vec<String> {
        self.containers
            .lock()
            .unwrap()
            .iter()
            .map(|c| c.name.clone())
            .collect()
    }

    fn mutate(&self, key: &str, change: impl FnOnce(&mut Sim)) {
        let mut all = self.containers.lock().unwrap();
        change(Self::find(&mut all, key).unwrap());
    }

    fn verbs(&self) -> Vec<String> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .map(|c| c[0].clone())
            .collect()
    }
}

fn config() -> PoolConfig {
    toml::from_str(
        r#"
provider = "docker"
repo = "acme/widgets"
slots = 2
image = "ubuntu:24.04"
build_dir = "target"
[docker]
host = "ssh://ci@box"
memory = "4g"
cpus = 2
tmpfs = "2g"
[github]
app_id = 1
private_key = "k.pem"
"#,
    )
    .unwrap()
}

fn rig(manifest_arch: Option<&'static str>) -> (Arc<Daemon>, DockerProvider) {
    let daemon = Arc::new(Daemon {
        manifest_arch,
        ..Daemon::default()
    });
    let provider = DockerProvider::with_runner(&config(), Box::new(Shared(Arc::clone(&daemon))));
    (daemon, provider)
}

fn spec() -> CreateSpec {
    CreateSpec {
        name: "lane-slot-1".into(),
        image: "ubuntu:24.04".into(),
        metadata: BTreeMap::from([
            ("vivi_pool".into(), "lane".into()),
            ("vivi_pool_mailspace".into(), "ms".into()),
            ("vivi_pool_slot".into(), "1".into()),
            ("vivi_pool_base".into(), "abc".into()),
            ("vivi_pool_state".into(), "seeding".into()),
        ]),
        env: BTreeMap::from([
            ("BUILD_DIR".into(), "target".into()),
            ("BUILD_MOUNTED".into(), "1".into()),
        ]),
        once_env: BTreeMap::from([("GH_TOKEN".into(), "ghs_it's-a-secret".into())]),
        passthrough_env: vec!["VIVI_TEST_SURELY_UNSET_KEY".into()],
        files: vec![("/work/boot.sh".into(), "echo boot\n".into())],
        command: vec!["bash".into(), "/work/boot.sh".into()],
        build_dir: Some("target".into()),
        tmpfs: "2g".into(),
    }
}

/// A seeded slot at rest: created, delivered, run once, ended, marked ready.
fn seeded_slot(daemon: &Daemon, provider: &DockerProvider) -> String {
    let id = provider.create(&spec()).unwrap();
    daemon.mutate(&id, |c| {
        c.status = "exited".into();
    });
    provider
        .seal(&id, &["GH_TOKEN"], META_STATE, STATE_READY)
        .unwrap();
    id
}

#[test]
fn create_args_carry_the_slot_contract_and_no_secret() {
    let limits = Limits {
        memory: "4g".into(),
        cpus: Some(2.0),
    };
    let args = create_args(&spec(), &limits, &["HARNESS_KEY".to_string()]);
    let joined = args.join(" ");
    for needle in [
        "create --name lane-slot-1 --init --restart no --memory 4g",
        "--cpus 2",
        "--tmpfs /work/repo/target:rw,exec,size=2g",
        "--label vivi_pool=lane",
        "--label vivi_pool_role=slot",
        "-e BUILD_MOUNTED=1",
        " -e HARNESS_KEY ubuntu:24.04 bash /work/boot.sh",
    ] {
        assert!(joined.contains(needle), "missing {needle}: {joined}");
    }
    assert!(!joined.contains("privileged"));
    assert!(
        !joined.contains("ghs_") && !joined.contains("GH_TOKEN"),
        "{joined}"
    );
    assert!(
        !joined.contains("HARNESS_KEY="),
        "a harness key never carries a value"
    );
    let no_cpus = create_args(
        &spec(),
        &Limits {
            memory: "1g".into(),
            cpus: None,
        },
        &[],
    );
    assert!(!no_cpus.iter().any(|a| a == "--cpus"));
}

#[test]
fn create_delivers_files_the_one_time_secret_and_the_architecture_then_starts() {
    let (daemon, provider) = rig(Some("amd64"));
    let id = provider.create(&spec()).unwrap();
    assert_eq!(id.len(), 12);
    let all = daemon.containers.lock().unwrap();
    let files = &all[0].files;
    assert_eq!(files["/work/boot.sh"], b"echo boot\n");
    let once = String::from_utf8(files[ONCE_ENV_PATH].clone()).unwrap();
    assert_eq!(once, "export GH_TOKEN='ghs_it'\\''s-a-secret'\n");
    assert_eq!(files["/work/vivi-marks/vivi_pool_arch"], b"amd64\n");
    assert_eq!(all[0].status, "running");
    assert!(
        all[0].env.iter().all(|e| !e.contains("ghs_")),
        "the token never reaches the environment"
    );
    drop(all);
    let calls = daemon.calls.lock().unwrap();
    assert!(
        !calls.iter().flatten().any(|a| a.contains("ghs_")),
        "no argument holds the secret"
    );
}

#[test]
fn the_architecture_falls_back_to_the_image_then_to_the_daemon() {
    let (daemon, provider) = rig(None);
    provider.create(&spec()).unwrap();
    // The simulated image has no architecture to give, so the daemon's counts.
    let all = daemon.containers.lock().unwrap();
    assert_eq!(all[0].files["/work/vivi-marks/vivi_pool_arch"], b"arm64\n");
    assert_eq!(go_arch("x86_64"), "amd64");
    assert_eq!(go_arch("riscv64"), "riscv64");
}

#[test]
fn a_failed_delivery_removes_the_half_made_container() {
    let (daemon, provider) = rig(None);
    let mut bad = spec();
    bad.files = vec![("/".to_string() + &"x".repeat(400), String::new())];
    assert!(provider.create(&bad).is_err());
    assert!(daemon.names().is_empty(), "{:?}", daemon.names());
}

#[test]
fn list_and_machine_fold_marks_and_the_claim_into_one_record() {
    let (daemon, provider) = rig(Some("arm64"));
    let id = seeded_slot(&daemon, &provider);
    let listed = provider.list().unwrap();
    assert_eq!(listed.len(), 1);
    let rec = &listed[0];
    assert_eq!(
        (rec.id.as_str(), rec.name.as_str(), rec.state.as_str()),
        (id.as_str(), "lane-slot-1", "stopped")
    );
    assert_eq!(rec.region, "ssh://ci@box");
    assert_eq!(rec.metadata["vivi_pool_arch"], "arm64");
    assert_eq!(crate::pool::slot::slot_state(rec), "free");
    let slots =
        crate::pool::slot::pool_slots(&crate::pool::slot::PoolId::new("ms", "", "lane"), &listed);
    assert_eq!(slots[0].arch.as_deref(), Some("arm64"));
    assert_eq!(provider.machine(&id).unwrap(), *rec);
    assert_eq!(provider.machine("deadbeef0000").unwrap().state, "destroyed");
}

#[test]
fn foreign_containers_are_never_listed() {
    let (daemon, provider) = rig(None);
    daemon.containers.lock().unwrap().push(Sim {
        id: "f00000000001".into(),
        name: "someone-elses".into(),
        image: "postgres".into(),
        labels: BTreeMap::new(),
        env: Vec::new(),
        status: "running".into(),
        exit: 0,
        files: BTreeMap::new(),
    });
    assert!(provider.list().unwrap().is_empty());
    assert_eq!(provider.machine("f00000000001").unwrap().state, "destroyed");
}

#[test]
fn a_claim_marks_the_slot_and_a_second_claim_is_refused() {
    let (daemon, provider) = rig(None);
    let id = seeded_slot(&daemon, &provider);
    let hold = provider.claim(&id, "task1234", 1_700_000_000).unwrap();
    assert!(hold.is_some());
    assert!(
        provider
            .claim(&id, "rival567", 1_700_000_001)
            .unwrap()
            .is_none()
    );
    let rec = provider.machine(&id).unwrap();
    assert_eq!(rec.metadata["vivi_pool_task"], "task1234");
    assert_eq!(rec.metadata["vivi_pool_claimed"], "1700000000");
    assert_eq!(crate::pool::slot::slot_state(&rec), "finished-uncollected");
    assert_eq!(
        daemon.names(),
        ["lane-slot-1".to_string(), format!("vivi-claim-{id}")]
    );
    provider.release(&id, &hold.unwrap()).unwrap();
    assert!(
        provider
            .machine(&id)
            .unwrap()
            .metadata
            .contains_key("vivi_pool_task"),
        "release keeps the marks"
    );
    provider.unclaim(&id).unwrap();
    assert_eq!(
        crate::pool::slot::slot_state(&provider.machine(&id).unwrap()),
        "free"
    );
    provider.unclaim(&id).unwrap();
}

#[test]
fn a_slot_that_stopped_being_free_since_the_listing_is_not_claimed() {
    let (daemon, provider) = rig(None);
    let id = seeded_slot(&daemon, &provider);
    daemon.mutate(&id, |c| c.status = "running".into());
    assert!(provider.claim(&id, "task1234", 1).unwrap().is_none());
    assert_eq!(
        daemon.names(),
        ["lane-slot-1".to_string()],
        "the claim container was taken back"
    );
    assert!(provider.claim("deadbeef0000", "t", 1).is_err());
}

#[test]
fn many_claimants_at_once_yield_exactly_one_winner() {
    let (daemon, provider) = rig(None);
    let id = seeded_slot(&daemon, &provider);
    let provider = Arc::new(provider);
    let winners: Vec<bool> = std::thread::scope(|scope| {
        let tasks: Vec<_> = (0..12)
            .map(|n| {
                let (provider, id) = (Arc::clone(&provider), id.clone());
                scope.spawn(move || {
                    provider
                        .claim(&id, &format!("task{n:04}"), 1)
                        .unwrap()
                        .is_some()
                })
            })
            .collect();
        tasks.into_iter().map(|t| t.join().unwrap()).collect()
    });
    assert_eq!(winners.iter().filter(|w| **w).count(), 1, "{winners:?}");
    let claims = daemon
        .names()
        .into_iter()
        .filter(|n| n.starts_with("vivi-claim-"))
        .count();
    assert_eq!(claims, 1);
}

#[test]
fn a_job_archive_is_copied_into_the_stopped_container_and_seal_writes_the_mark() {
    let (daemon, provider) = rig(None);
    let id = seeded_slot(&daemon, &provider);
    let hold = Hold { nonce: "n".into() };
    provider
        .put_file(&id, &hold, "/work/job.tgz", b"archive")
        .unwrap();
    assert_eq!(
        daemon.containers.lock().unwrap()[0].files["/work/job.tgz"],
        b"archive"
    );
    assert_eq!(
        daemon.containers.lock().unwrap()[0].files["/work/vivi-marks/vivi_pool_state"],
        b"ready\n"
    );
    assert!(
        !daemon.verbs().contains(&"exec".to_string()),
        "delivery never needs a running slot"
    );
}

#[test]
fn a_stopped_slots_result_is_read_without_starting_it() {
    let (daemon, provider) = rig(None);
    let id = seeded_slot(&daemon, &provider);
    daemon.mutate(&id, |c| {
        c.files
            .insert("/work/job/result".into(), b"job=j1\npush=pushed\n".to_vec());
        c.files.insert("/work/job/run.log".into(), b"log".to_vec());
    });
    let starts = daemon.verbs().iter().filter(|v| *v == "start").count();
    let files = provider
        .read_stopped(
            &id,
            &[
                "/work/job/result",
                "/work/job/agent.out",
                "/work/job/run.log",
            ],
        )
        .unwrap()
        .unwrap();
    assert_eq!(files["/work/job/result"], b"job=j1\npush=pushed\n");
    assert_eq!(files["/work/job/run.log"], b"log");
    assert!(
        !files.contains_key("/work/job/agent.out"),
        "a missing file is just absent"
    );
    assert_eq!(
        daemon.verbs().iter().filter(|v| *v == "start").count(),
        starts
    );
    let got = crate::pool::collect::collected_from_files(&files).unwrap();
    assert_eq!(got.result.get("job"), "j1");
}

#[test]
fn exec_on_a_container_that_is_not_running_is_an_error_not_a_script_result() {
    let (daemon, provider) = rig(None);
    let id = seeded_slot(&daemon, &provider);
    let err = provider.exec(&id, "true", 5).unwrap_err().to_string();
    assert!(err.contains("not running"), "{err}");
}

#[test]
fn destroy_removes_the_slot_and_its_claim_and_tolerates_a_missing_claim() {
    let (daemon, provider) = rig(None);
    let id = seeded_slot(&daemon, &provider);
    provider.claim(&id, "task1234", 1).unwrap().unwrap();
    provider.destroy(&id).unwrap();
    assert!(daemon.names().is_empty());
    assert_eq!(
        provider.destroy_command("abc123abc123"),
        "DOCKER_HOST=ssh://ci@box docker rm -f -v abc123abc123 vivi-claim-abc123abc123"
    );
}

#[test]
fn the_pool_does_not_run_privileged_or_mount_for_itself() {
    let (_, provider) = rig(None);
    assert!(provider.supplies_build_mount());
    assert!(provider.live_test_notice().contains("containers"));
    assert!(provider.ensure_home().unwrap().is_none());
}

#[test]
fn harness_env_names_unset_in_this_process_are_reported_and_dispatch_asks_nothing() {
    let (_, provider) = rig(None);
    let names = vec!["VIVI_TEST_SURELY_UNSET_KEY".to_string(), "PATH".to_string()];
    assert_eq!(
        provider.harness_env_gaps(&names).unwrap(),
        ["VIVI_TEST_SURELY_UNSET_KEY"]
    );
    assert!(provider.env_gaps_at_dispatch(&names).unwrap().is_empty());
    assert_eq!(
        unset_names(&names, |n| n == "PATH"),
        ["VIVI_TEST_SURELY_UNSET_KEY"]
    );
    let hint = provider.env_gap_hint(&names[..1]);
    assert!(
        hint.contains("container's configuration on the Docker daemon"),
        "{hint}"
    );
    assert!(hint.contains("VIVI_TEST_SURELY_UNSET_KEY=<value>"));
}

#[test]
fn a_spec_without_a_build_dir_gets_no_memory_disk() {
    let mut bare = spec();
    bare.build_dir = None;
    bare.tmpfs = String::new();
    let args = create_args(
        &bare,
        &Limits {
            memory: "4g".into(),
            cpus: None,
        },
        &[],
    );
    assert!(!args.iter().any(|a| a == "--tmpfs"), "{args:?}");
}
