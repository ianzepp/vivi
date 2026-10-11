//! `runpodctl` as the pool provider: a slot is one rented GPU pod that lives
//! for one job.
//!
//! A pod bills by the second from the moment it exists, a stopped pod keeps
//! nothing, and the provider offers no labels and no lease. So this host has
//! no resting slots (see [`SlotLifetime::PerDispatch`]):
//!
//! - `dispatch` makes the pod, pins its SSH host key, seeds it, claims it and
//!   hands it the job (see [`runpod_open`](super::runpod_open)).
//! - Collecting the result destroys the pod and waits until the provider's
//!   list no longer shows it. `reconcile` destroys any pod of the pool that no
//!   dispatch explains.
//! - Everything about a pod that Vivi needs is in its name (see
//!   [`runpod_name`](super::runpod_name)); nothing is kept locally except the
//!   pinned host key of each pod.
//! - Inside the pod the work is done by the same remote script and the same
//!   boot and job scripts as the `ssh` host, run over SSH as root: this type
//!   hands the calls that act on a slot to an [`SshProvider`] aimed at the pod.
//! - The API key is never read by Vivi (see [`runpod_cli`](super::runpod_cli))
//!   and never sent to the pod.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use super::config::PoolConfig;
use super::docker_cli::{RunOutput, Runner};
use super::doctor::Check;
use super::provider::{
    ExecOutput, Hold, MachineRecord, OpenRequest, OpenedSlot, PoolProvider, SlotLifetime,
};
use super::runpod_cli::{BareCli, CALL_TIMEOUT, CtlCli, redact};
use super::runpod_name::PodName;
use super::runpod_pin::{KeyScanner, SshKeyscan, forget, pin_path};
use super::runpod_pod::{self as pod_api, Pod};
use super::slot::{
    META_CLAIMED, META_MAILSPACE, META_POOL, META_SLOT, META_STATE, META_TASK, STATE_READY,
};
use super::ssh::SshProvider;
use super::ssh_cli::SshCli;
use super::ssh_state;
use crate::error::VivariumError;

/// The slot root inside a pod (the scripts' `/work`).
pub const POD_ROOT: &str = "/work";

/// Everything the provider waits for or paces itself with.
#[derive(Debug, Clone)]
pub struct Pace {
    /// Pause between polls.
    pub poll: Duration,
    /// Pause between the two host-key scans.
    pub scan_gap: Duration,
    /// How long a terminated pod may take to leave the provider's list.
    pub absent: Duration,
    /// Pause before the first claim check, so a pod made just before is listed.
    pub settle: Duration,
    /// How long a dispatch waits for another on this machine.
    pub lock_wait: Duration,
    /// How long a dispatch may take, from the start to the job running.
    pub setup_limit: Duration,
}

impl Default for Pace {
    fn default() -> Self {
        Self {
            poll: Duration::from_secs(3),
            scan_gap: super::runpod_pin::SCAN_GAP,
            absent: Duration::from_mins(3),
            settle: Duration::from_secs(8),
            lock_wait: Duration::from_mins(15),
            setup_limit: SETUP_LIMIT,
        }
    }
}

/// The longest a dispatch may take to get a job running on a new pod: create,
/// seed and deliver. It bounds what a half-made pod can cost.
pub const SETUP_LIMIT: Duration = Duration::from_mins(30);

/// Where the host's own files live and which pools exist, for the checks that
/// need to know a pod's siblings.
#[derive(Debug, Clone, Default)]
pub struct HostContext {
    /// The mailspace's `.vivi` directory.
    pub dir: PathBuf,
    /// The mailspace slug.
    pub mailspace: String,
    /// The pool this provider serves.
    pub pool: String,
    /// The mailspace's configured pool names.
    pub pools: Vec<String>,
}

/// Where a pod's SSH server is and which key file to trust it by.
pub struct Link<'a> {
    pub ip: &'a str,
    pub port: u16,
    pub pin: &'a std::path::Path,
}

type Connect = Box<dyn Fn(&Link) -> Box<dyn Runner>>;

/// The SSH provider aimed at a pod, and the id of the pod's one slot in it.
type Session = (Rc<SshProvider>, String);

/// Pool provider backed by `runpodctl` for the account and `ssh` for the pods.
pub struct RunpodProvider {
    pub(super) config: PoolConfig,
    pub(super) ctx: HostContext,
    pub(super) ctl: Box<dyn Runner>,
    pub(super) bare: Box<dyn Runner>,
    pub(super) scanner: Box<dyn KeyScanner>,
    connect: Connect,
    pub(super) pace: Pace,
    sessions: RefCell<BTreeMap<String, Session>>,
}

impl RunpodProvider {
    #[must_use]
    pub fn new(config: &PoolConfig, ctx: &HostContext) -> Self {
        Self {
            config: config.clone(),
            ctx: ctx.clone(),
            ctl: Box::new(CtlCli::new(&config.runpod)),
            bare: Box::new(BareCli),
            scanner: Box::new(SshKeyscan),
            connect: Box::new(|link: &Link| {
                let host = format!("root@{}", link.ip);
                Box::new(SshCli::pinned(&host, link.port, link.pin))
            }),
            pace: Pace::default(),
            sessions: RefCell::new(BTreeMap::new()),
        }
    }

    /// A provider whose every outside call is replaced (tests).
    #[must_use]
    pub fn with_parts(
        config: &PoolConfig,
        ctx: &HostContext,
        (ctl, scanner): (Box<dyn Runner>, Box<dyn KeyScanner>),
        connect: Box<dyn Fn(&Link) -> Box<dyn Runner>>,
        pace: Pace,
    ) -> Self {
        Self {
            ctl,
            scanner,
            connect,
            pace,
            ..Self::new(config, ctx)
        }
    }

    pub(super) fn pins_dir(&self) -> PathBuf {
        self.ctx.dir.join("runpod").join("pins")
    }

    pub(super) fn locks_dir(&self) -> PathBuf {
        self.ctx.dir.join("runpod")
    }

    /// One `runpodctl` call; a failing exit code is returned, not raised.
    pub(super) fn call(
        &self,
        args: &[String],
        timeout: Duration,
    ) -> Result<RunOutput, VivariumError> {
        self.ctl.run(args, None, timeout)
    }

    /// One call that must succeed; its standard output.
    pub(super) fn require(&self, what: &str, args: &[String]) -> Result<String, VivariumError> {
        let out = self.call(args, CALL_TIMEOUT)?;
        if out.succeeded() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(failure(what, &out))
        }
    }

    /// Every pod of the account, `--all` (stopped ones too).
    pub(super) fn account_pods(&self) -> Result<Vec<Pod>, VivariumError> {
        pod_api::parse_pods(&self.require("pod list", &pod_api::list_args())?)
    }

    /// One pod in full (`pod get`): its creation time and SSH endpoint.
    pub(super) fn pod_detail(&self, id: &str) -> Result<Pod, VivariumError> {
        pod_api::parse_pod(&self.require("pod get", &pod_api::get_args(id)?)?)
    }

    /// The pods of the account whose names are Vivi's, with their parts.
    pub(super) fn vivi_pods(&self) -> Result<Vec<(Pod, PodName)>, VivariumError> {
        Ok(self
            .account_pods()?
            .into_iter()
            .filter_map(|pod| {
                let name = PodName::decode(&pod.name)?;
                Some((pod, name))
            })
            .collect())
    }

    /// The pods of this provider's pool (its mailspace and pool name), pods
    /// that are already gone left out.
    pub(super) fn pool_pods(
        &self,
        pool: &super::slot::PoolId,
    ) -> Result<Vec<(Pod, PodName)>, VivariumError> {
        Ok(self
            .vivi_pods()?
            .into_iter()
            .filter(|(pod, name)| name.is_of(pool) && pod.state() != "destroyed")
            .collect())
    }

    /// The SSH provider aimed at pod `id`, over its pinned host key.
    fn session(&self, id: &str) -> Result<Session, VivariumError> {
        if let Some((ssh, slot)) = self.sessions.borrow().get(id) {
            return Ok((Rc::clone(ssh), slot.clone()));
        }
        let pod = self.pod_detail(id)?;
        let name = PodName::decode(&pod.name).ok_or_else(|| not_ours(id))?;
        let pin = pin_path(&self.pins_dir(), id);
        if !pin.is_file() {
            return Err(VivariumError::Message(format!(
                "no pinned SSH host key for pod {id} on this machine ({}); it was not dispatched from here, or the pin was removed, so it cannot be reached safely. Terminate it: {}",
                pin.display(),
                self.destroy_command(id)
            )));
        }
        let (ip, port) = pod.endpoint().ok_or_else(|| {
            VivariumError::Message(format!(
                "pod {id} has no SSH address yet (state {})",
                pod.state()
            ))
        })?;
        Ok(self.adopt_session(id, &name, (ip, port)))
    }

    /// Aim an SSH provider at pod `id` at `endpoint` over its pin, and keep it.
    pub(super) fn adopt_session(&self, id: &str, name: &PodName, endpoint: (&str, u16)) -> Session {
        let pin = pin_path(&self.pins_dir(), id);
        let link = Link {
            ip: endpoint.0,
            port: endpoint.1,
            pin: &pin,
        };
        let runner = (self.connect)(&link);
        let ssh = Rc::new(SshProvider::for_machine(
            &format!("pod {id}"),
            POD_ROOT,
            runner,
        ));
        let session = (ssh, slot_id_of(name));
        self.sessions
            .borrow_mut()
            .insert(id.to_string(), (Rc::clone(&session.0), session.1.clone()));
        session
    }

    /// Wait until pod `id` is no longer in the provider's list (or is listed as
    /// terminated). Absence from the list is the only proof a pod stopped
    /// billing.
    fn await_absent(&self, id: &str) -> Result<(), VivariumError> {
        let deadline = Instant::now() + self.pace.absent;
        loop {
            let listed = self.account_pods()?.into_iter().find(|p| p.id == id);
            if listed.is_none_or(|p| p.state() == "destroyed") {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(VivariumError::Message(format!(
                    "pod {id} is still listed {}s after it was told to terminate; it may still be billing. Run `vivi pool reconcile` again, or: {}",
                    self.pace.absent.as_secs(),
                    self.destroy_command(id)
                )));
            }
            std::thread::sleep(self.pace.poll);
        }
    }
}

/// The text of a failed `runpodctl` call: what the CLI said, key-shaped words
/// removed.
pub(super) fn failure(what: &str, out: &RunOutput) -> VivariumError {
    let said = if out.stderr.trim().is_empty() {
        out.stdout_text()
    } else {
        out.stderr.clone()
    };
    let said = serde_json::from_str::<serde_json::Value>(&said)
        .into_iter()
        .find_map(|v| v.get("error")?.as_str().map(str::to_string))
        .unwrap_or(said);
    VivariumError::Message(format!("`runpodctl {what}` failed: {}", redact(&said)))
}

fn not_ours(id: &str) -> VivariumError {
    VivariumError::Message(format!(
        "pod {id} is not a pod of Vivi's (its name is not in the layout Vivi makes); Vivi will not act on it"
    ))
}

/// The slot id inside a pod: `<mailspace>/<pool>/slot-<index>`.
pub(super) fn slot_id_of(name: &PodName) -> String {
    ssh_state::slot_id(&name.mailspace, &name.pool, &name.index.to_string())
}

/// The pool's view of a pod.
pub(super) fn record_of(pod: &Pod, name: &PodName) -> MachineRecord {
    let marks = [
        (META_MAILSPACE, name.mailspace.clone()),
        (META_POOL, name.pool.clone()),
        (META_SLOT, name.index.to_string()),
        (META_STATE, STATE_READY.to_string()),
        (META_TASK, name.task.clone()),
        (META_CLAIMED, name.claimed.to_string()),
    ];
    MachineRecord {
        id: pod.id.clone(),
        name: format!("{}-{}-slot-{}", name.mailspace, name.pool, name.index),
        state: pod.state().to_string(),
        region: pod
            .machine
            .as_ref()
            .map_or_else(String::new, |m| m.gpu_id.clone()),
        created_at: iso(name.claimed),
        metadata: marks.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
        exit_code: None,
    }
}

/// Epoch seconds as `2026-10-11T13:00:00Z`.
#[must_use]
pub fn iso(epoch: u64) -> String {
    let secs = i64::try_from(epoch).unwrap_or(i64::MAX);
    chrono::DateTime::from_timestamp(secs, 0)
        .map_or_else(String::new, |t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
}

impl PoolProvider for RunpodProvider {
    fn slot_lifetime(&self) -> SlotLifetime {
        SlotLifetime::PerDispatch
    }

    fn open_for_dispatch(&self, request: &OpenRequest) -> Result<OpenedSlot, VivariumError> {
        self.open(request)
    }

    fn setup_grace_secs(&self) -> u64 {
        self.pace.setup_limit.as_secs() + 60
    }

    fn billing_note(&self) -> String {
        "billed by the second on RunPod until it is terminated".to_string()
    }

    fn live_test_spend_gate(&self) -> Option<String> {
        Some(self.spend_notice())
    }

    /// The account is the pool's home; nothing to make.
    fn ensure_home(&self) -> Result<Option<String>, VivariumError> {
        Ok(None)
    }

    fn doctor(&self, harness_env: &[String]) -> Vec<Check> {
        self.doctor_checks(harness_env)
    }

    fn harness_env_gaps(&self, names: &[String]) -> Result<Vec<String>, VivariumError> {
        Ok(names
            .iter()
            .filter(|n| std::env::var_os(n).is_none_or(|v| v.is_empty()))
            .cloned()
            .collect())
    }

    /// The values are read when the pod is made, so a dispatch can still be told.
    fn env_gaps_at_dispatch(&self, names: &[String]) -> Result<Vec<String>, VivariumError> {
        self.harness_env_gaps(names)
    }

    fn env_gap_hint(&self, missing: &[String]) -> String {
        format!(
            "warning: {} not set in the environment of this vivi process. A RunPod pod takes its harness environment from it when the pod is made: the values go over the pinned SSH connection into a file only root can read in the pod, and never to the provider's API. Export {} before dispatching.",
            missing.join(", "),
            missing
                .iter()
                .map(|n| format!("{n}=<value>"))
                .collect::<Vec<_>>()
                .join(" ")
        )
    }

    fn live_test_notice(&self) -> String {
        "it rents one billable GPU pod for several minutes, runs a stand-in job on it, and terminates it (Ctrl-C stops it and cleans up)".to_string()
    }

    fn supplies_build_mount(&self) -> bool {
        false
    }

    /// A pod's container disk is a plain directory for the pod's life.
    fn build_persists(&self) -> bool {
        true
    }

    /// The scripts run as root in the container, as on Docker and Fly.
    fn runs_unprivileged(&self) -> bool {
        false
    }

    fn list(&self) -> Result<Vec<MachineRecord>, VivariumError> {
        Ok(self
            .vivi_pods()?
            .iter()
            .map(|(pod, name)| record_of(pod, name))
            .collect())
    }

    /// Pods are made per dispatch: see [`PoolProvider::open_for_dispatch`].
    fn create(&self, _spec: &super::provider::CreateSpec) -> Result<String, VivariumError> {
        Err(VivariumError::Message(
            "a RunPod pool has no resting slots: a pod is made for each dispatch (`vivi pool dispatch`)".into(),
        ))
    }

    /// Terminate the pod and wait until the provider no longer lists it. A pod
    /// that is already gone is a success.
    fn destroy(&self, id: &str) -> Result<(), VivariumError> {
        let args = pod_api::delete_args(id)?;
        let out = self.call(&args, CALL_TIMEOUT)?;
        if !out.succeeded()
            && self
                .account_pods()?
                .iter()
                .any(|p| p.id == id && p.state() != "destroyed")
        {
            return Err(failure("pod delete", &out));
        }
        self.await_absent(id)?;
        forget(&pin_path(&self.pins_dir(), id));
        self.sessions.borrow_mut().remove(id);
        Ok(())
    }

    fn destroy_command(&self, id: &str) -> String {
        format!(
            "RUNPOD_API_KEY=$(cat {}) runpodctl pod delete {id}",
            self.config.runpod.key_path().display()
        )
    }

    /// The provider's word for the pod, refined by the pod's own: a pod the
    /// provider calls running is `stopped` here once its boot process has
    /// ended (the job is over and its result is on the pod's disk).
    fn machine(&self, id: &str) -> Result<MachineRecord, VivariumError> {
        let Some(pod) = self.account_pods()?.into_iter().find(|p| p.id == id) else {
            return Ok(MachineRecord::gone(id));
        };
        let name = PodName::decode(&pod.name).ok_or_else(|| not_ours(id))?;
        let mut record = record_of(&pod, &name);
        if record.state == "started" {
            let (ssh, slot) = self.session(id)?;
            let inner = ssh.machine(&slot)?;
            record.state = inner.state;
            record.exit_code = inner.exit_code;
        }
        Ok(record)
    }

    /// A pod is claimed by the dispatch that makes it.
    fn claim(&self, _id: &str, _task: &str, _now: u64) -> Result<Option<Hold>, VivariumError> {
        Ok(None)
    }

    fn unclaim(&self, _id: &str) -> Result<(), VivariumError> {
        Ok(())
    }

    fn release(&self, _id: &str, _hold: &Hold) -> Result<(), VivariumError> {
        Ok(())
    }

    fn put_file(
        &self,
        id: &str,
        hold: &Hold,
        path: &str,
        content: &[u8],
    ) -> Result<(), VivariumError> {
        let (ssh, slot) = self.session(id)?;
        ssh.put_file(&slot, hold, path, content)
    }

    fn seal(
        &self,
        id: &str,
        drop_env: &[&str],
        key: &str,
        value: &str,
    ) -> Result<(), VivariumError> {
        let (ssh, slot) = self.session(id)?;
        ssh.seal(&slot, drop_env, key, value)
    }

    /// Check the claim once more, then start the job. A pod that lost the race
    /// is terminated here and the start is an error.
    fn start(&self, id: &str) -> Result<(), VivariumError> {
        self.recheck_claim(id)?;
        let (ssh, slot) = self.session(id)?;
        ssh.start(&slot)
    }

    fn stop(&self, id: &str) -> Result<(), VivariumError> {
        let (ssh, slot) = self.session(id)?;
        ssh.stop(&slot)
    }

    fn exec(&self, id: &str, script: &str, timeout_secs: u32) -> Result<ExecOutput, VivariumError> {
        let (ssh, slot) = self.session(id)?;
        ssh.exec(&slot, script, timeout_secs)
    }

    /// A pod the provider no longer runs keeps nothing: it reads as having no
    /// files, which the engine takes as a lost job. A running pod is read over
    /// SSH without touching its processes.
    fn read_stopped(
        &self,
        id: &str,
        paths: &[&str],
    ) -> Result<Option<BTreeMap<String, Vec<u8>>>, VivariumError> {
        let running = self
            .account_pods()?
            .iter()
            .any(|p| p.id == id && p.state() == "started");
        if !running {
            return Ok(Some(BTreeMap::new()));
        }
        let (ssh, slot) = self.session(id)?;
        ssh.read_stopped(&slot, paths)
    }
}

#[cfg(test)]
#[path = "runpod_test.rs"]
mod tests;
