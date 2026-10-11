//! Making the pod of one dispatch, from "no pod" to "running, seeded, claimed".
//!
//! In order, under a lock against other dispatches on this machine: pick the
//! lowest unused slot index (refusing when the pool already holds `slots`
//! pods); rent a pod, trying the configured GPU types in order, with a
//! provider-side deadline; pin its SSH host key; check the claim against other
//! dispatchers; run the seed (the same boot script as every other host, as
//! root); claim the slot inside the pod. Whatever fails after the pod exists
//! terminates the pod before the error is returned, and says so if even that
//! fails. The claim is checked once more just before the job starts (see
//! [`PoolProvider::start`]).

use std::time::{Duration, Instant};

use super::collect::TIMEOUT_GRACE_SECS;
use super::provider::{OpenRequest, OpenedSlot, PoolProvider};
use super::requires;
use super::run::ONCE_ENV;
use super::runpod::{RunpodProvider, SETUP_LIMIT, iso};
use super::runpod_claim::{Entry, Verdict, lowest_free, verdict};
use super::runpod_cli::{CREATE_TIMEOUT, redact};
use super::runpod_lock;
use super::runpod_name::{PodName, new_nonce, task_field};
use super::runpod_pin;
use super::runpod_pod::{self as pod_api, GpuType, Pod};
use super::slot::{META_SLOT, META_STATE, PoolId, STATE_READY};
use super::ssh::SshProvider;
use crate::error::VivariumError;

/// How long to wait for a created pod to show its SSH address.
const ENDPOINT_WAIT: Duration = Duration::from_secs(90);

impl RunpodProvider {
    pub(super) fn open(&self, request: &OpenRequest) -> Result<OpenedSlot, VivariumError> {
        let say = |line: &str| request.progress.say(line);
        let id = request.pool;
        let lock = runpod_lock::path_for(&self.locks_dir(), &id.mailspace, &id.pool);
        let _held = runpod_lock::acquire(&lock, self.pace.lock_wait, self.pace.poll, &say)?;
        let started = Instant::now();
        let index = self.next_index(id)?;
        let name = PodName {
            mailspace: id.mailspace.clone(),
            pool: id.pool.clone(),
            index,
            task: task_field(request.task),
            claimed: request.now,
            deadline: deadline_of(request.now, request.budget_secs),
            nonce: new_nonce(),
        };
        let text = name.encode()?;
        say(&format!(
            "{}: renting a pod for slot {index} (it ends by {} whatever happens)",
            id.pool,
            iso(name.deadline)
        ));
        let pod = self.rent(&text, &name, request)?;
        match self.ready(request, &pod, &name, started) {
            Ok(slot) => Ok(slot),
            Err(e) => Err(self.abandon(&pod.id, name.deadline, e)),
        }
    }

    /// The lowest unused slot index, or why the pool is full.
    fn next_index(&self, id: &PoolId) -> Result<u32, VivariumError> {
        let pods = self.pool_pods(id)?;
        if pods.len() >= self.config.slots as usize {
            let held: Vec<String> = pods
                .iter()
                .map(|(pod, name)| {
                    format!(
                        "{}-{}-slot-{} {} ({})",
                        name.mailspace,
                        name.pool,
                        name.index,
                        pod.state(),
                        name.task
                    )
                })
                .collect();
            return Err(VivariumError::Message(format!(
                "no free slot in pool {}: it allows {} pod(s) at once and holds {}",
                id.pool,
                self.config.slots,
                held.join(", ")
            )));
        }
        let used: Vec<u32> = pods.iter().map(|(_, n)| n.index).collect();
        Ok(lowest_free(&used))
    }

    /// Rent a pod named `text`: the first configured GPU type that can be had.
    fn rent(
        &self,
        text: &str,
        name: &PodName,
        request: &OpenRequest,
    ) -> Result<Pod, VivariumError> {
        let cfg = &self.config.runpod;
        let market = self.market();
        let types = order_types(
            &cfg.gpu_types,
            market.as_deref(),
            cfg.cloud_type(),
            &cfg.data_centers,
        );
        let mut last = String::from("no GPU type was tried");
        for gpu in &types {
            request.progress.say(&format!("trying {gpu}"));
            let args =
                pod_api::create_args(cfg, &self.config.image, text, gpu, &iso(name.deadline));
            match self.call(&args, CREATE_TIMEOUT) {
                Ok(out) if out.succeeded() => {
                    return self.made(&String::from_utf8_lossy(&out.stdout), name);
                }
                Ok(out) => last = format!("{gpu}: {}", super::runpod::failure("pod create", &out)),
                Err(e) => last = format!("{gpu}: {e}"),
            }
            // The CLI keeps a pod whose SSH did not come up in time, and a lost
            // answer may hide a pod that was made: remove it before the next try.
            self.remove_by_nonce(&name.nonce)?;
        }
        Err(VivariumError::Message(format!(
            "no pod could be rented ({} GPU type(s) tried; last: {})",
            types.len(),
            redact(&last)
        )))
    }

    /// The pod a successful create printed, checked against the request.
    fn made(&self, stdout: &str, name: &PodName) -> Result<Pod, VivariumError> {
        let found = match pod_api::parse_pod(stdout) {
            Ok(pod) => pod,
            Err(_) => self.find_by_nonce(&name.nonce)?.ok_or_else(|| {
                VivariumError::Message(
                    "the create printed no pod and none carries this dispatch's name".into(),
                )
            })?,
        };
        let ours = PodName::decode(&found.name).is_some_and(|n| n.nonce == name.nonce);
        let wanted = self.config.runpod.gpu_count;
        let problem = if !ours {
            Some(format!(
                "pod {} does not carry the name it was made with",
                found.id
            ))
        } else if found.gpu_count != wanted {
            Some(format!(
                "pod {} has {} GPU(s), not the {wanted} asked for",
                found.id, found.gpu_count
            ))
        } else {
            None
        };
        if let Some(why) = problem {
            return Err(self.abandon(&found.id, name.deadline, VivariumError::Message(why)));
        }
        Ok(found)
    }

    fn find_by_nonce(&self, nonce: &str) -> Result<Option<Pod>, VivariumError> {
        Ok(self
            .vivi_pods()?
            .into_iter()
            .find(|(_, n)| n.nonce == nonce)
            .map(|(pod, _)| pod))
    }

    fn remove_by_nonce(&self, nonce: &str) -> Result<(), VivariumError> {
        match self.find_by_nonce(nonce)? {
            Some(pod) => self.destroy(&pod.id),
            None => Ok(()),
        }
    }

    /// Everything after the pod exists; the caller terminates the pod on error.
    fn ready(
        &self,
        request: &OpenRequest,
        pod: &Pod,
        name: &PodName,
        started: Instant,
    ) -> Result<OpenedSlot, VivariumError> {
        let say = |line: &str| request.progress.say(line);
        let (ip, port) = self.endpoint_of(pod)?;
        say(&format!(
            "pod {} is up on {}; pinning its SSH host key",
            pod.id,
            pod.hardware()
        ));
        let pin = runpod_pin::pin_path(&self.pins_dir(), &pod.id);
        runpod_pin::capture(self.scanner.as_ref(), (&ip, port), &pin, self.pace.scan_gap)?;
        let (ssh, slot) = self.adopt_session(&pod.id, name, (&ip, port));
        std::thread::sleep(self.pace.settle);
        self.verify(&request.pool.clone(), pod, name)?;
        check_tools(&ssh)?;
        let spec = slot_spec(request, name, &slot);
        say("seeding the pod");
        ssh.create(&spec)?;
        self.wait_seed(&ssh, &slot, started)?;
        ssh.seal(&slot, &ONCE_ENV, META_STATE, STATE_READY)?;
        let hold = ssh
            .claim(&slot, request.task, request.now)?
            .ok_or_else(|| {
                VivariumError::Message("the pod would not be claimed after its seed".into())
            })?;
        Ok(OpenedSlot {
            id: pod.id.clone(),
            name: format!("{}-{}-slot-{}", name.mailspace, name.pool, name.index),
            index: name.index,
            hold,
            hardware: pod.hardware(),
            created_at: iso(name.claimed),
        })
    }

    /// The pod's SSH address; created pods carry it, but ask again for a while.
    fn endpoint_of(&self, pod: &Pod) -> Result<(String, u16), VivariumError> {
        if let Some((ip, port)) = pod.endpoint() {
            return Ok((ip.to_string(), port));
        }
        let deadline = Instant::now() + ENDPOINT_WAIT.min(self.pace.setup_limit);
        loop {
            let detail = self.pod_detail(&pod.id)?;
            if let Some((ip, port)) = detail.endpoint() {
                return Ok((ip.to_string(), port));
            }
            if Instant::now() >= deadline {
                return Err(VivariumError::Message(format!(
                    "pod {} showed no SSH address within {}s",
                    pod.id,
                    ENDPOINT_WAIT.as_secs()
                )));
            }
            std::thread::sleep(self.pace.poll);
        }
    }

    /// Wait for the seed boot to end and read how it ended.
    fn wait_seed(
        &self,
        ssh: &SshProvider,
        slot: &str,
        started: Instant,
    ) -> Result<(), VivariumError> {
        loop {
            let seen = ssh.machine(slot)?;
            if seen.state == "stopped" {
                return match seen.exit_code {
                    Some(0) => Ok(()),
                    code => Err(seed_failure(ssh, slot, code)),
                };
            }
            if started.elapsed() >= self.pace.setup_limit {
                return Err(VivariumError::Message(format!(
                    "the seed did not finish within {}s of the dispatch starting",
                    self.pace.setup_limit.as_secs()
                )));
            }
            std::thread::sleep(self.pace.poll);
        }
    }

    /// Apply the claim rule to pod `mine` among its pool's pods; a pod that
    /// loses is terminated before the error returns.
    pub(super) fn verify(
        &self,
        pool: &PoolId,
        mine: &Pod,
        name: &PodName,
    ) -> Result<(), VivariumError> {
        let pods = self.pool_pods(pool)?;
        let entries: Vec<Entry> = pods
            .iter()
            .map(|(pod, n)| Entry::of(&pod.id, n, self.created_ms(pod)))
            .collect();
        let me = Entry::of(&mine.id, name, self.created_ms(mine));
        let slots = self.config.slots;
        match verdict(&me, &entries, slots) {
            Verdict::Keep => Ok(()),
            lost => {
                let why = lost.reason(name.index, slots).unwrap_or_default();
                let e = VivariumError::Message(format!(
                    "this dispatch lost its slot: {why}; run it again"
                ));
                Err(self.abandon(&mine.id, name.deadline, e))
            }
        }
    }

    /// The provider's creation time of a pod, from `pod get` when the listing
    /// lacks it.
    fn created_ms(&self, pod: &Pod) -> Option<i64> {
        if let Some(ms) = pod.created_ms() {
            return Some(ms);
        }
        match self.pod_detail(&pod.id) {
            Ok(detail) => detail.created_ms(),
            Err(_) => None,
        }
    }

    /// Check the claim of an existing pod (just before its job starts).
    pub(super) fn recheck_claim(&self, id: &str) -> Result<(), VivariumError> {
        let Some((pod, name)) = self.vivi_pods()?.into_iter().find(|(p, _)| p.id == id) else {
            return Err(VivariumError::Message(format!(
                "pod {id} is gone; it cannot start a job"
            )));
        };
        let pool = PoolId {
            mailspace: name.mailspace.clone(),
            pool: name.pool.clone(),
        };
        self.verify(&pool, &pod, &name)
    }

    /// Terminate a pod after a failure; the error to return says what failed
    /// and, if the pod could not be terminated, that it is still billing.
    pub(super) fn abandon(&self, id: &str, deadline: u64, why: VivariumError) -> VivariumError {
        match self.destroy(id) {
            Ok(()) => why,
            Err(undo) => VivariumError::Message(format!(
                "{why}; and pod {id} could not be terminated ({undo}). It bills until it is terminated or its provider-side deadline passes ({}). Terminate it: {}",
                iso(deadline),
                self.destroy_command(id)
            )),
        }
    }

    /// The GPU market, if the CLI can say (a hint, never required).
    pub(super) fn market(&self) -> Option<Vec<GpuType>> {
        let text = self
            .require("gpu list", &pod_api::gpus_args())
            .into_iter()
            .next()?;
        let market = pod_api::parse_gpus(&text).into_iter().next()?;
        (!market.is_empty()).then_some(market)
    }
}

/// When a pod must be gone: setup, the job's budget, and the time `wait` grants
/// a silent job before it calls it lost.
#[must_use]
pub fn deadline_of(now: u64, budget_secs: u64) -> u64 {
    now + SETUP_LIMIT.as_secs() + budget_secs + TIMEOUT_GRACE_SECS
}

/// What a dispatch to a pod must tell its caller: the pod is billing, and what
/// ends it.
#[must_use]
pub fn uncollected_note(made: &super::record::DispatchRecord) -> String {
    format!(
        "note: pod {} ({}) bills by the second until `vivi pool wait` or `vivi pool reconcile` collects and terminates it; the provider was asked to terminate it by {} at the latest (whether that fires without vivi running is not verified). Collect it promptly",
        made.machine,
        made.hardware,
        iso(deadline_of(made.at, made.budget))
    )
}

/// The configured GPU types in the order to try them: those the market shows
/// in stock first, in the configured order; if the market shows none, or none
/// is readable, all of them in the configured order.
#[must_use]
pub fn order_types(
    configured: &[String],
    market: Option<&[GpuType]>,
    cloud: &str,
    centers: &[String],
) -> Vec<String> {
    let Some(market) = market else {
        return configured.to_vec();
    };
    let stocked: Vec<String> = configured
        .iter()
        .filter(|t| {
            market
                .iter()
                .find(|m| &m.gpu_id == *t)
                .is_none_or(|m| m.in_stock(cloud, centers))
        })
        .cloned()
        .collect();
    if stocked.is_empty() {
        configured.to_vec()
    } else {
        stocked
    }
}

/// The tools the scripts need must be in the pod.
fn check_tools(ssh: &SshProvider) -> Result<(), VivariumError> {
    let probe = ssh.probe().map_err(VivariumError::Message)?;
    if probe.missing.is_empty() {
        return Ok(());
    }
    Err(VivariumError::Message(format!(
        "the pod's image lacks what the slot scripts need: {} (choose an image that has them)",
        probe.missing.join(", ")
    )))
}

/// The slot as the SSH provider makes it: the request's spec, with the slot
/// index the pod was named for, and the harness environment values from this
/// process (they travel inside the SSH connection, into a file only root reads).
fn slot_spec(request: &OpenRequest, name: &PodName, slot: &str) -> super::provider::CreateSpec {
    let mut spec = request.spec.clone();
    spec.name = slot.to_string();
    spec.metadata
        .insert(META_SLOT.to_string(), name.index.to_string());
    for var in &spec.passthrough_env.clone() {
        if let Some(value) = std::env::var_os(var).filter(|v| !v.is_empty()) {
            spec.env
                .insert(var.clone(), value.to_string_lossy().into_owned());
        }
    }
    spec
}

/// Why the seed failed, as plainly as the pod can say.
fn seed_failure(ssh: &SshProvider, slot: &str, code: Option<i64>) -> VivariumError {
    let said = ssh
        .read_stopped(slot, &[requires::FAILURE_PATH])
        .into_iter()
        .flatten()
        .find_map(|files| files.get(requires::FAILURE_PATH).cloned())
        .map(|b| String::from_utf8_lossy(&b).trim().to_string())
        .filter(|t| !t.is_empty());
    let tail = ssh
        .exec(
            slot,
            "tail -n 3 \"${WORK:-/work}/boot.log\" 2>/dev/null",
            20,
        )
        .map_or_else(|_| String::new(), |o| o.stdout.trim().replace('\n', "; "));
    let code = code.map_or_else(|| "unknown".to_string(), |c| c.to_string());
    VivariumError::Message(format!(
        "the pod's seed failed (exit {code}): {}",
        said.unwrap_or(tail)
    ))
}

#[cfg(test)]
#[path = "runpod_open_test.rs"]
mod tests;
