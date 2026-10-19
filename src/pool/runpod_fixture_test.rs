//! A pretend RunPod account and a pretend pod, shared by the RunPod tests.
//!
//! - [`Ctl`] stands in for `runpodctl`: a stateful account of pods that lists,
//!   describes, creates and deletes, with switches for the failures the real
//!   service has (no stock, a pod hidden from the listing for a while, a
//!   delete that lags or fails, a refused key). It records every call, and it
//!   answers any verb that is not one Vivi uses with an error, so a test can
//!   assert that nothing unexpected was asked.
//! - [`PodShell`] stands in for the pod as the remote script sees it: it
//!   answers the script's verbs from memory (the real script needs Linux
//!   processes).
//! - [`Scan`] stands in for `ssh-keyscan`.
//!
//! Nothing here starts a process, opens a socket or spends anything.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use super::config::PoolConfig;
use super::docker_cli::{RunOutput, Runner};
use super::docker_tar as tar;
use super::progress::Progress;
use super::provider::{CreateSpec, OpenRequest};
use super::runpod::{HostContext, Link, Pace, RunpodProvider};
use super::runpod_name::PodName;
use super::runpod_pin::KeyScanner;
use super::slot::{PoolId, slot_metadata};
use crate::error::VivariumError;

pub const NOW: u64 = 1_790_000_000;
pub const HOST_KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIIBo";
pub const OTHER_KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIXXX";

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// One pod of the pretend account.
#[derive(Debug, Clone)]
pub struct FakePod {
    pub id: String,
    pub name: String,
    pub gpu: String,
    pub gpu_count: u32,
    pub created_ms: i64,
    pub terminate_after: String,
    /// Left out of the next this-many listings (a pod the list has not caught
    /// up with yet). `pod get` still finds it.
    pub hidden: u32,
    /// Told to terminate: still listed for this many more listings.
    pub dying: Option<u32>,
}

type Hook = Box<dyn FnMut(&mut World) + Send>;

/// The pretend account.
#[derive(Default)]
pub struct World {
    pub pods: Vec<FakePod>,
    /// Every `runpodctl` call, in order.
    pub calls: Vec<Vec<String>>,
    /// GPU types a create is refused for ("no instances available").
    pub no_stock: Vec<String>,
    /// GPU types whose create makes the pod and then reports a failure (the
    /// CLI keeps a pod whose SSH did not come up in time).
    pub half_made: Vec<String>,
    /// Like `half_made`, but the CLI's error names the pod it kept.
    pub half_made_with_id: Vec<String>,
    /// Runs right after a create has made its pod (race simulation).
    pub after_create: Option<Hook>,
    /// How many listings a terminated pod still shows in.
    pub delete_lag: u32,
    /// A terminated pod shows as `terminated` instead of staying `running`.
    pub show_terminated: bool,
    /// Every delete is refused (the pod stays).
    pub delete_fails: bool,
    /// The key is refused.
    pub auth_fails: bool,
    /// The account's balance in dollars.
    pub balance: f64,
    /// SSH keys on the account.
    pub ssh_keys: usize,
    /// The `gpu list` answer.
    pub market: String,
    clock_ms: i64,
    next: u32,
}

impl World {
    #[must_use]
    pub fn new() -> Self {
        Self {
            balance: 50.0,
            ssh_keys: 1,
            market: market(&[
                ("NVIDIA A100 80GB PCIe", 1.19, "High"),
                ("NVIDIA H100 PCIe", 2.39, "Low"),
            ]),
            clock_ms: 1_790_000_000_000,
            ..Self::default()
        }
    }

    /// A pod that was there already (another dispatcher's, or a leak), made
    /// `ago_ms` before the clock's now.
    pub fn add(&mut self, name: &str, created_ms: i64) -> String {
        self.next += 1;
        let id = format!("rp{:06}", self.next);
        self.pods.push(FakePod {
            id: id.clone(),
            name: name.to_string(),
            gpu: "NVIDIA A100 80GB PCIe".to_string(),
            gpu_count: 1,
            created_ms,
            terminate_after: String::new(),
            hidden: 0,
            dying: None,
        });
        id
    }

    /// The time the next pod will be created at.
    pub fn tick(&mut self) -> i64 {
        self.clock_ms += 1000;
        self.clock_ms
    }

    #[must_use]
    pub fn clock(&self) -> i64 {
        self.clock_ms
    }

    #[must_use]
    pub fn live(&self) -> Vec<&FakePod> {
        self.pods.iter().filter(|p| p.dying.is_none()).collect()
    }

    /// Calls whose first words are `verb`.
    #[must_use]
    pub fn calls_to(&self, verb: &[&str]) -> Vec<&Vec<String>> {
        self.calls
            .iter()
            .filter(|c| c.len() >= verb.len() && c.iter().zip(verb).all(|(a, b)| a == b))
            .collect()
    }

    /// Calls that make, change or start something that bills.
    #[must_use]
    pub fn spending_calls(&self) -> usize {
        self.calls
            .iter()
            .filter(|c| {
                matches!(
                    c.get(1).map(String::as_str),
                    Some("create" | "start" | "restart" | "update")
                )
            })
            .count()
    }
}

/// A `gpu list` answer for `(id, secure price, stock)` rows.
#[must_use]
pub fn market(rows: &[(&str, f64, &str)]) -> String {
    let items: Vec<String> = rows
        .iter()
        .map(|(id, price, stock)| {
            format!(
                r#"{{"gpuId":"{id}","displayName":"{id}","secureCloud":true,"communityCloud":true,"securePricePerHr":{price},"communityPricePerHr":{},"stockStatus":"{stock}","dataCenterAvailability":[{{"dataCenterId":"EU-RO-1","stockStatus":"{stock}"}}]}}"#,
                price - 0.3
            )
        })
        .collect();
    format!("[{}]", items.join(","))
}

fn created_text(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|t| t.format("%Y-%m-%d %H:%M:%S%.3f +0000 UTC").to_string())
        .unwrap_or_default()
}

fn ok(text: &str) -> Result<RunOutput, VivariumError> {
    Ok(RunOutput {
        code: 0,
        stdout: text.as_bytes().to_vec(),
        stderr: String::new(),
    })
}

fn fail(error: &str) -> Result<RunOutput, VivariumError> {
    Ok(RunOutput {
        code: 1,
        stdout: Vec::new(),
        stderr: format!(r#"{{"error":"{error}","code":"E","status":500}}"#),
    })
}

fn row(p: &FakePod, terminated: bool) -> String {
    let (desired, runtime) = if terminated {
        ("TERMINATED", "terminated")
    } else {
        ("RUNNING", "running")
    };
    format!(
        r#"{{"id":"{}","name":"{}","desiredStatus":"{desired}","runtimeStatus":"{runtime}","costPerHr":1.19,"gpuCount":{},"imageName":"img"}}"#,
        p.id, p.name, p.gpu_count
    )
}

fn full(p: &FakePod) -> String {
    let n: u32 = p.id.trim_start_matches("rp").parse().unwrap_or(0);
    format!(
        r#"{{"id":"{}","name":"{}","desiredStatus":"RUNNING","runtimeStatus":"running","costPerHr":1.19,"gpuCount":{},"imageName":"img","createdAt":"{}","machine":{{"gpuId":"{}","secureCloud":true}},"ssh":{{"ip":"203.0.113.{}","port":{}}}}}"#,
        p.id,
        p.name,
        p.gpu_count,
        created_text(p.created_ms),
        p.gpu,
        n % 250 + 1,
        22_000 + n
    )
}

/// The account, as `runpodctl` answers for it.
#[derive(Clone)]
pub struct Ctl(pub Arc<Mutex<World>>);

impl Ctl {
    pub fn world(&self) -> MutexGuard<'_, World> {
        lock(&self.0)
    }
}

impl Runner for Ctl {
    fn run(
        &self,
        args: &[String],
        _stdin: Option<&[u8]>,
        _timeout: Duration,
    ) -> Result<RunOutput, VivariumError> {
        let mut w = lock(&self.0);
        w.calls.push(args.to_vec());
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        if w.auth_fails {
            return fail("unauthorized");
        }
        match a.as_slice() {
            ["user"] => ok(&format!(
                r#"{{"id":"user_test","clientBalance":{},"currentSpendPerHr":0.0}}"#,
                w.balance
            )),
            ["gpu", "list"] => {
                let text = w.market.clone();
                ok(&text)
            }
            ["ssh", "list-keys"] => ok(&format!(
                r#"{{"keys":[{}]}}"#,
                vec![
                    r#"{"name":"k","type":"ssh-ed25519","key":"ssh-ed25519 AAAA\n","fingerprint":"f"}"#;
                    w.ssh_keys
                ]
                .join(",")
            )),
            ["pod", "list", "--all"] => ok(&list(&mut w)),
            ["pod", "get", id] => match w.pods.iter().find(|p| p.id == *id) {
                Some(p) => ok(&full(p)),
                None => fail("pod not found"),
            },
            ["pod", "delete", id] => delete(&mut w, id),
            ["pod", "create", rest @ ..] => create(&mut w, rest),
            _ => Ok(RunOutput {
                code: 2,
                stdout: Vec::new(),
                stderr: format!("fixture: unexpected call {a:?}"),
            }),
        }
    }
}

fn list(w: &mut World) -> String {
    let terminated = w.show_terminated;
    let mut rows = Vec::new();
    w.pods.retain_mut(|p| {
        if p.hidden > 0 {
            p.hidden -= 1;
            return true;
        }
        match p.dying {
            Some(0) => return false,
            Some(left) => p.dying = Some(left - 1),
            None => {}
        }
        rows.push(row(p, terminated && p.dying.is_some()));
        true
    });
    format!("[{}]", rows.join(","))
}

fn delete(w: &mut World, id: &str) -> Result<RunOutput, VivariumError> {
    if w.delete_fails {
        return fail("could not terminate");
    }
    let lag = w.delete_lag;
    match w.pods.iter().position(|p| p.id == id) {
        None => fail("pod not found"),
        Some(at) if lag == 0 => {
            w.pods.remove(at);
            ok(r#"{"deleted":true}"#)
        }
        Some(at) => {
            w.pods[at].dying = Some(lag);
            ok(r#"{"deleted":true}"#)
        }
    }
}

fn create(w: &mut World, rest: &[&str]) -> Result<RunOutput, VivariumError> {
    let flag = |name: &str| {
        rest.windows(2)
            .find(|p| p[0] == name)
            .map(|p| p[1].to_string())
    };
    let (Some(name), Some(gpu)) = (flag("--name"), flag("--gpu-id")) else {
        return fail("missing --name or --gpu-id");
    };
    if w.no_stock.contains(&gpu) {
        return fail("There are no instances currently available");
    }
    let created_ms = w.tick();
    let id = w.add(&name, created_ms);
    if let Some(p) = w.pods.iter_mut().find(|p| p.id == id) {
        p.gpu = gpu;
        p.gpu_count = flag("--gpu-count")
            .and_then(|c| c.parse().into_iter().next())
            .unwrap_or(1);
        p.terminate_after = flag("--terminate-after").unwrap_or_default();
    }
    if let Some(mut hook) = w.after_create.take() {
        hook(w);
        w.after_create = Some(hook);
    }
    if w.half_made_with_id.contains(
        &w.pods
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.gpu.clone())
            .unwrap_or_default(),
    ) {
        return fail(&format!(
            "timed out waiting for ssh on pod {id}; the pod was kept"
        ));
    }
    if w.half_made.contains(
        &w.pods
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.gpu.clone())
            .unwrap_or_default(),
    ) {
        return fail("timed out waiting for the pod's SSH port");
    }
    let text = w
        .pods
        .iter()
        .find(|p| p.id == id)
        .map(full)
        .unwrap_or_default();
    ok(&text)
}

/// What the pretend pod holds and has been asked.
#[derive(Debug, Default)]
pub struct PodState {
    /// Every script verb, in order.
    pub verbs: Vec<String>,
    /// Files in the slot, by path below the slot directory.
    pub files: BTreeMap<String, Vec<u8>>,
    pub marks: BTreeMap<String, String>,
    pub slot: String,
    /// Tools the probe reports missing.
    pub missing: Vec<String>,
    /// How the seed ended.
    pub seed_exit: i64,
    pub claimed_by: Option<String>,
    pub exec_out: String,
    /// Every connection made: `(ip, port, pin file)`.
    pub links: Vec<(String, u16, std::path::PathBuf)>,
    /// A call the pod refuses with this text (the host is unreachable).
    pub unreachable: Option<String>,
}

/// The pod, as the remote script answers for it.
#[derive(Clone)]
pub struct PodShell(pub Arc<Mutex<PodState>>);

impl PodShell {
    pub fn state(&self) -> MutexGuard<'_, PodState> {
        lock(&self.0)
    }
}

/// The value assigned to `name=` on its own line of the script header.
fn assigned(line: &str, name: &str) -> Option<String> {
    let rest = line.strip_prefix(name)?.strip_prefix('=')?;
    let inner = rest.strip_prefix('\'')?.strip_suffix('\'')?;
    Some(inner.replace(r"'\''", "'"))
}

/// The script inside `bash -c '<script>'`, with its one layer of quoting removed.
fn unwrap_script(line: &str) -> String {
    let word = line.split_once(" -c ").map_or(line, |(_, rest)| rest);
    let inner = word
        .strip_prefix('\'')
        .and_then(|w| w.strip_suffix('\''))
        .unwrap_or(word);
    inner.replace(r"'\''", "'")
}

impl Runner for PodShell {
    fn run(
        &self,
        args: &[String],
        stdin: Option<&[u8]>,
        _timeout: Duration,
    ) -> Result<RunOutput, VivariumError> {
        let mut s = lock(&self.0);
        if let Some(why) = &s.unreachable {
            return Ok(RunOutput {
                code: 255,
                stdout: Vec::new(),
                stderr: why.clone(),
            });
        }
        let line = args.first().cloned().unwrap_or_default();
        let script = unwrap_script(&line);
        let header: Vec<&str> = script.lines().collect();
        let get = |name: &str| {
            header
                .iter()
                .find_map(|l| assigned(l, name))
                .unwrap_or_default()
        };
        let (verb, a1, a2, a3) = (get("VERB"), get("A1"), get("A2"), get("A3"));
        s.verbs.push(verb.clone());
        let out = match verb.as_str() {
            "probe" => {
                let mut text = String::from(
                    "user=root\nhost=pod\nsystem=Linux x86_64\nroot=/work\nroot_exists=1\nroot_writable=1\nroot_anchor=/work\nfree_kb=99999999\n",
                );
                for m in &s.missing {
                    text.push_str(&format!("missing={m}\n"));
                }
                text
            }
            "create" => {
                s.slot = a1;
                for file in tar::read(stdin.unwrap_or_default())? {
                    let name = file.path.trim_start_matches("./").to_string();
                    if let Some(mark) = name.strip_prefix("marks/") {
                        s.marks.insert(
                            mark.to_string(),
                            String::from_utf8_lossy(&file.content).trim().to_string(),
                        );
                    }
                    s.files.insert(name, file.content);
                }
                String::new()
            }
            "machine" if s.slot == a1 && !a1.is_empty() => {
                let mut text = format!(
                    "@@slot {a1}\nstate=stopped\nexit={}\ncreated=2026-10-11T00:00:00Z\n",
                    s.seed_exit
                );
                for (k, v) in &s.marks {
                    text.push_str(&format!("mark.{k}={v}\n"));
                }
                if let Some(task) = &s.claimed_by {
                    text.push_str(&format!("claim.task={task}\nclaim.claimed={NOW}\n"));
                }
                text
            }
            "seal" => {
                s.marks.insert(a2, a3);
                String::new()
            }
            "claim" => {
                if s.claimed_by.is_some() {
                    "taken\n".to_string()
                } else {
                    s.claimed_by = Some(a2);
                    "claimed nonce0123\n".to_string()
                }
            }
            "put" => {
                s.files.insert(a2, stdin.unwrap_or_default().to_vec());
                String::new()
            }
            "read" => {
                let wanted: Vec<String> = a2.split_whitespace().map(str::to_string).collect();
                let found: Vec<(String, Vec<u8>)> = wanted
                    .iter()
                    .filter_map(|w| s.files.get(w).map(|c| (w.clone(), c.clone())))
                    .collect();
                return Ok(RunOutput {
                    code: 0,
                    stdout: tar::build(&found)?,
                    stderr: String::new(),
                });
            }
            "exec" => s.exec_out.clone(),
            _ => String::new(),
        };
        ok(&out)
    }
}

/// `ssh-keyscan`: each scan answers the next scripted text, then repeats the last.
pub struct Scan(pub Arc<Mutex<Vec<String>>>);

#[must_use]
pub fn scan_text(keys: &[&str]) -> String {
    keys.iter()
        .map(|k| format!("[203.0.113.1]:22001 {k}\n"))
        .collect()
}

impl KeyScanner for Scan {
    fn scan(&self, _host: &str, _port: u16) -> Result<String, VivariumError> {
        let mut left = lock(&self.0);
        Ok(if left.len() > 1 {
            left.remove(0)
        } else {
            left.first().cloned().unwrap_or_default()
        })
    }
}

/// Everything a RunPod test pokes at.
pub struct Rig {
    pub provider: RunpodProvider,
    pub config: PoolConfig,
    pub account: Ctl,
    pub pod: PodShell,
    pub scans: Arc<Mutex<Vec<String>>>,
    pub dir: tempfile::TempDir,
    pub id: PoolId,
}

#[must_use]
pub fn pool_toml(slots: u32, runpod: &str, key: &str) -> String {
    format!(
        r#"
provider = "runpod"
slots = {slots}
image = "runpod/pytorch:2.4.0-py3.11-cuda12.4.1-runtime-ubuntu22.04"
repo = "o/r"
build_dir = "target"
[github]
app_id = 1
private_key = "k.pem"
[harness]
name = "fake"
run = "fake --model {{model}} @{{brief}}"
models = ["m/ok"]
[committer]
name = "bot"
email = "bot@example.com"
[runpod]
api_key_file = "{key}"
gpu_types = ["NVIDIA A100 80GB PCIe", "NVIDIA H100 PCIe"]
{runpod}
"#
    )
}

pub fn fast() -> Pace {
    Pace {
        poll: Duration::from_millis(1),
        scan_gap: Duration::ZERO,
        absent: Duration::from_millis(300),
        settle: Duration::ZERO,
        lock_wait: Duration::from_millis(200),
        seed_limit: Duration::from_secs(20),
    }
}

/// A provider over the pretend account and pod, for a pool of `slots` pods.
pub fn rig(slots: u32, runpod: &str) -> Rig {
    rig_keyed(slots, runpod, "/keys/runpod.key")
}

/// `runpodctl --version` answers the same text every time.
pub struct Version(pub String);

impl Runner for Version {
    fn run(
        &self,
        _args: &[String],
        _stdin: Option<&[u8]>,
        _t: Duration,
    ) -> Result<RunOutput, VivariumError> {
        ok(&self.0)
    }
}

/// Like [`rig`], with the key file at `key`.
pub fn rig_keyed(slots: u32, runpod: &str, key: &str) -> Rig {
    let config: PoolConfig = toml::from_str(&pool_toml(slots, runpod, key)).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let id = PoolId::new("ms", "", "gpu");
    config.validate(&id).unwrap();
    let account = Ctl(Arc::new(Mutex::new(World::new())));
    let pod = PodShell(Arc::new(Mutex::new(PodState::default())));
    let scans = Arc::new(Mutex::new(vec![scan_text(&[HOST_KEY])]));
    let ctx = HostContext {
        dir: dir.path().to_path_buf(),
        mailspace: "ms".into(),
        pool: "gpu".into(),
        pools: vec!["gpu".into()],
    };
    let shell = pod.clone();
    let connect = Box::new(move |link: &Link| -> Box<dyn Runner> {
        lock(&shell.0)
            .links
            .push((link.ip.to_string(), link.port, link.pin.to_path_buf()));
        Box::new(shell.clone())
    });
    let mut provider = RunpodProvider::with_parts(
        &config,
        &ctx,
        (
            Box::new(account.clone()),
            Box::new(Scan(Arc::clone(&scans))),
        ),
        connect,
        fast(),
    );
    provider.bare = Box::new(Version("runpodctl version 2.1.9".into()));
    Rig {
        provider,
        config,
        account,
        pod,
        scans,
        dir,
        id,
    }
}

/// The pod name a dispatcher of this pool would have given a pod.
#[must_use]
pub fn pod_name(index: u32, task: &str, nonce: &str) -> String {
    PodName {
        mailspace: "ms".into(),
        pool: "gpu".into(),
        index,
        task: task.into(),
        claimed: NOW,
        deadline: NOW + 5000,
        nonce: nonce.into(),
    }
    .encode()
    .unwrap()
}

#[must_use]
pub fn spec(id: &PoolId, index: u32) -> CreateSpec {
    CreateSpec {
        name: format!("ms-gpu-slot-{index}"),
        image: String::new(),
        metadata: slot_metadata(id, index, "main"),
        env: BTreeMap::from([("BASE".to_string(), "main".to_string())]),
        once_env: BTreeMap::from([("GH_TOKEN".to_string(), "seed-token".to_string())]),
        passthrough_env: Vec::new(),
        files: vec![("/work/boot.sh".into(), "echo boot\n".into())],
        command: vec!["bash".into(), "/work/boot.sh".into()],
        build_dir: Some("target".into()),
        tmpfs: String::new(),
    }
}

/// An open request for task `task` at `NOW`, with a 600 s budget.
pub fn request<'a>(id: &'a PoolId, task: &'a str, progress: &'a Progress) -> OpenRequest<'a> {
    OpenRequest {
        pool: id,
        task,
        now: NOW,
        budget_secs: 600,
        spec: spec(id, 1),
        progress,
        ready_timeout_secs: None,
    }
}

/// Collects what a progress sink was told.
#[must_use]
pub fn recording() -> (Progress, Arc<Mutex<Vec<String>>>) {
    let said = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&said);
    (
        Progress::to(false, move |l| lock(&sink).push(l.to_string())),
        said,
    )
}
