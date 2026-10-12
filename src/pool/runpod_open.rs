//! Making the pod of one dispatch, from "no pod" to "running, seeded, claimed".
//!
//! In order, under a lock against other dispatches on this machine: pick the
//! lowest unused slot index (refusing when the pool already holds `slots`
//! pods); rent a pod, trying the configured GPU types in order, with a
//! provider-side deadline, and giving each new pod `ready_timeout` seconds to
//! answer on SSH (a pod that does not is terminated, confirmed gone, counted
//! and charged to the report, and the next type is tried); pin its SSH host key; check the claim against other
//! dispatchers; run the seed (the same boot script as every other host, as
//! root); claim the slot inside the pod. Whatever fails after the pod exists
//! terminates the pod before the error is returned, and says so if even that
//! fails. The claim is checked once more just before the job starts (see
//! [`PoolProvider::start`]).

use std::time::{Duration, Instant};

use super::collect::TIMEOUT_GRACE_SECS;
use super::docker_cli::RunOutput;
use super::provider::{Abandoned, OpenRequest, OpenedSlot, PoolProvider};
use super::record::dollars;
use super::requires;
use super::run::ONCE_ENV;
use super::runpod::{RunpodProvider, iso};
use super::runpod_claim::{Entry, Verdict, lowest_free, verdict};
use super::runpod_cli::{create_timeout, redact};
use super::runpod_config::PoolRunpod;
use super::runpod_lock;
use super::runpod_name::{PodName, new_nonce, task_field};
use super::runpod_pin;
use super::runpod_pod::{self as pod_api, GpuType, Pod};
use super::slot::{META_SLOT, META_STATE, PoolId, STATE_READY};
use super::ssh::SshProvider;
use crate::error::VivariumError;

/// How long to wait for a created pod to show its SSH address.
const ENDPOINT_WAIT: Duration = Duration::from_secs(90);

/// How one create ended.
enum Created {
    /// The pod, made and checked.
    Pod(Pod),
    /// The create failed (no stock, or the pod did not answer in time): what to
    /// say of it, and everything it printed (it may name a pod it kept).
    Refused { summary: String, said: String },
}

impl RunpodProvider {
    pub(super) fn open(&self, request: &OpenRequest) -> Result<OpenedSlot, VivariumError> {
        let say = |line: &str| request.progress.say(line);
        let id = request.pool;
        let lock = runpod_lock::path_for(&self.locks_dir(), &id.mailspace, &id.pool);
        let _held = runpod_lock::acquire(&lock, self.pace.lock_wait, self.pace.poll, &say)?;
        let cfg = &self.config.runpod;
        let ready_secs = request.ready_timeout_secs.unwrap_or(cfg.ready_timeout);
        let setup = setup_secs(cfg.attempt_count(), ready_secs, self.pace.seed_limit);
        let index = self.next_index(id)?;
        let name = PodName {
            mailspace: id.mailspace.clone(),
            pool: id.pool.clone(),
            index,
            task: task_field(request.task),
            claimed: request.now,
            deadline: deadline_of(request.now, request.budget_secs, setup),
            nonce: new_nonce(),
        };
        let text = name.encode()?;
        say(&format!(
            "{}: renting a pod for slot {index} (it ends by {} whatever happens; a new pod gets {ready_secs}s to answer on SSH)",
            id.pool,
            iso(name.deadline)
        ));
        let (pod, abandoned) = self.rent(&text, &name, request, ready_secs)?;
        match self.ready(request, &pod, &name, Instant::now()) {
            Ok(mut slot) => {
                slot.abandoned = abandoned;
                Ok(slot)
            }
            Err(e) => Err(with_abandoned(
                self.abandon(&pod.id, name.deadline, e),
                &abandoned,
            )),
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

    /// Rent a pod named `text`: the first configured GPU type that can be had
    /// and whose pod answers on SSH within `ready_secs`. Every pod that does
    /// not is terminated and confirmed gone before the next type is tried, and
    /// returned (with the pod that works) so the dispatch can report it.
    fn rent(
        &self,
        text: &str,
        name: &PodName,
        request: &OpenRequest,
        ready_secs: u64,
    ) -> Result<(Pod, Vec<Abandoned>), VivariumError> {
        let cfg = &self.config.runpod;
        let (types, first_word) = create_attempts(cfg, || self.market());
        let mut last = String::from(first_word);
        let mut abandoned: Vec<Abandoned> = Vec::new();
        for gpu in &types {
            request.progress.say(&format!("trying {gpu}"));
            let began = Instant::now();
            let attempt = self
                .create_one(text, name, gpu, ready_secs)
                .map_err(|e| with_abandoned(e, &abandoned))?;
            let said = match attempt {
                Created::Pod(pod) => return Ok((pod, abandoned)),
                Created::Refused { summary, said } => {
                    last = summary;
                    said
                }
            };
            // The CLI keeps a pod whose SSH did not come up in time, and a lost
            // answer may hide a pod that was made: remove it before the next try.
            if let Some(gone) = self
                .reclaim(&said, name, gpu, began)
                .map_err(|e| with_abandoned(e, &abandoned))?
            {
                request
                    .progress
                    .say(&abandoned_line(&gone, ready_secs, &last));
                abandoned.push(gone);
            }
        }
        Err(with_abandoned(
            VivariumError::Message(refusal_text(cfg, &types, &last)),
            &abandoned,
        ))
    }

    /// One `pod create` of GPU type `gpu`, waiting up to `ready_secs` for the
    /// pod to answer on SSH.
    fn create_one(
        &self,
        text: &str,
        name: &PodName,
        gpu: &str,
        ready_secs: u64,
    ) -> Result<Created, VivariumError> {
        let cfg = &self.config.runpod;
        let args = pod_api::create_args(
            cfg,
            &self.config.image,
            text,
            gpu,
            (&iso(name.deadline), ready_secs),
        );
        Ok(match self.call(&args, create_timeout(ready_secs)) {
            Ok(out) if out.succeeded() => {
                Created::Pod(self.made(&String::from_utf8_lossy(&out.stdout), name)?)
            }
            Ok(out) => Created::Refused {
                summary: format!("{gpu}: {}", super::runpod::failure("pod create", &out)),
                said: raw_text(&out),
            },
            Err(e) => Created::Refused {
                summary: format!("{gpu}: {e}"),
                said: e.to_string(),
            },
        })
    }

    /// Terminate the pod a failed create may have left, and confirm it gone.
    /// It is found by an id named in the CLI's output (checked: the pod must
    /// carry this dispatch's nonce), else by the nonce in its name. `None`: no
    /// such pod exists (the create failed before making one).
    fn reclaim(
        &self,
        said: &str,
        name: &PodName,
        gpu: &str,
        began: Instant,
    ) -> Result<Option<Abandoned>, VivariumError> {
        let Some(pod) = self.pod_of_failed_create(said, &name.nonce)? else {
            return Ok(None);
        };
        self.destroy(&pod.id).map_err(|undo| {
            VivariumError::Message(format!(
                "pod {} was made but did not become reachable, and could not be terminated ({undo}). It bills until it is terminated or its provider-side deadline passes ({}). Terminate it: {}",
                pod.id,
                iso(name.deadline),
                self.destroy_command(&pod.id)
            ))
        })?;
        let waited = began.elapsed();
        Ok(Some(Abandoned {
            id: pod.id,
            gpu: gpu.to_string(),
            waited_secs: waited.as_secs(),
            cost_micros: pod.cost_per_hr.map(|price| charge_micros(price, waited)),
        }))
    }

    fn pod_of_failed_create(&self, said: &str, nonce: &str) -> Result<Option<Pod>, VivariumError> {
        for id in pod_api::pod_ids_in(said).into_iter().take(4) {
            if let Ok(pod) = self.pod_detail(&id)
                && PodName::decode(&pod.name).is_some_and(|n| n.nonce == nonce)
            {
                return Ok(Some(pod));
            }
        }
        self.find_by_nonce(nonce)
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
        } else if !self.config.runpod.is_cpu() && found.gpu_count != wanted {
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
            pod.hardware_of_tier(self.config.runpod.is_cpu())
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
            hardware: pod.hardware_of_tier(self.config.runpod.is_cpu()),
            created_at: iso(name.claimed),
            abandoned: Vec::new(),
        })
    }

    /// The pod's SSH address; created pods carry it, but ask again for a while.
    fn endpoint_of(&self, pod: &Pod) -> Result<(String, u16), VivariumError> {
        if let Some((ip, port)) = pod.endpoint() {
            return Ok((ip.to_string(), port));
        }
        let deadline = Instant::now() + ENDPOINT_WAIT.min(self.pace.seed_limit);
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
            if started.elapsed() >= self.pace.seed_limit {
                return Err(VivariumError::Message(format!(
                    "the seed did not finish within {}s of the pod answering on SSH",
                    self.pace.seed_limit.as_secs()
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

/// The worst case of getting a job running on a new pod, in seconds: every
/// GPU type waited for in full (`types` x `ready_secs`), then seeding.
#[must_use]
pub fn setup_secs(types: usize, ready_secs: u64, seed_limit: Duration) -> u64 {
    u64::try_from(types).unwrap_or(u64::MAX) * ready_secs + seed_limit.as_secs()
}

/// When a pod must be gone: setup (see [`setup_secs`]), the job's budget, and
/// the time `wait` grants a silent job before it calls it lost.
#[must_use]
pub fn deadline_of(now: u64, budget_secs: u64, setup_secs: u64) -> u64 {
    now + setup_secs + budget_secs + TIMEOUT_GRACE_SECS
}

/// The deadline of a dispatch to this pool: the wait for each pod is the pool's
/// `ready_timeout`, or `ready_override` (`--ready-timeout`).
#[must_use]
pub fn dispatch_deadline(
    config: &super::runpod_config::PoolRunpod,
    ready_override: Option<u64>,
    (now, budget_secs): (u64, u64),
) -> u64 {
    let ready = ready_override.unwrap_or(config.ready_timeout);
    let setup = setup_secs(config.attempt_count(), ready, super::runpod::SEED_LIMIT);
    deadline_of(now, budget_secs, setup)
}

/// What a dispatch to a pod must tell its caller: the pod is billing, and what
/// ends it (`deadline`, epoch seconds, from [`dispatch_deadline`]).
#[must_use]
pub fn uncollected_note(made: &super::record::DispatchRecord, deadline: u64) -> String {
    format!(
        "note: pod {} ({}) bills by the second until `vivi pool wait` or `vivi pool reconcile` collects and terminates it; the provider was asked to terminate it by {} at the latest (whether that fires without vivi running is not verified). Collect it promptly",
        made.machine,
        made.hardware,
        iso(deadline)
    )
}

/// Approximate charge of a pod that existed for `waited`, in millionths of a
/// dollar: its hourly price times the time.
#[must_use]
pub fn charge_micros(price_per_hr: f64, waited: Duration) -> u64 {
    let micros = price_per_hr * waited.as_secs_f64() / 3600.0 * 1_000_000.0;
    if micros.is_finite() && micros > 0.0 {
        // Rounded and non-negative; a charge never nears u64::MAX.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let whole = micros.round() as u64;
        whole
    } else {
        0
    }
}

/// The create attempts of one dispatch: one CPU pseudo-type, or the pool's
/// GPU types ordered against the market. The second element is the "nothing
/// was tried" wording for that tier.
fn create_attempts<F>(cfg: &PoolRunpod, market: F) -> (Vec<String>, &'static str)
where
    F: FnOnce() -> Option<Vec<GpuType>>,
{
    if cfg.is_cpu() {
        return (vec![String::from("CPU")], "no CPU pod was tried");
    }
    let listed = market();
    (
        order_types(
            &cfg.gpu_types,
            listed.as_deref(),
            cfg.cloud_type(),
            &cfg.data_centers,
        ),
        "no GPU type was tried",
    )
}

/// What a dispatch says when every create attempt was refused.
fn refusal_text(cfg: &PoolRunpod, types: &[String], last: &str) -> String {
    if cfg.is_cpu() {
        return format!("no CPU pod could be rented (last: {})", redact(last));
    }
    format!(
        "no pod could be rented ({} GPU type(s) tried; last: {})",
        types.len(),
        redact(last)
    )
}

/// The progress line for a pod given up on.
fn abandoned_line(gone: &Abandoned, ready_secs: u64, said: &str) -> String {
    let charge = gone.cost_micros.map_or_else(
        || "its price is unknown, so no charge can be estimated".to_string(),
        |m| {
            format!(
                "approximate charge ${} (hourly price x time, approximate)",
                dollars(m)
            )
        },
    );
    format!(
        "abandoned pod {} ({}): it did not become reachable over SSH within {ready_secs}s ({}); terminated after {}s; {charge}",
        gone.id,
        gone.gpu,
        redact(said).chars().take(120).collect::<String>(),
        gone.waited_secs
    )
}

/// `error`, with what the pods abandoned before it cost, when any were.
fn with_abandoned(error: VivariumError, abandoned: &[Abandoned]) -> VivariumError {
    if abandoned.is_empty() {
        return error;
    }
    let known: u64 = abandoned.iter().filter_map(|a| a.cost_micros).sum();
    let cost = if known == 0 {
        "price unknown".to_string()
    } else {
        format!("approximately ${}", dollars(known))
    };
    VivariumError::Message(format!(
        "{error}; {} pod(s) were made earlier in this dispatch, did not become reachable and were terminated ({cost})",
        abandoned.len()
    ))
}

/// Everything a failed call printed, as one text.
fn raw_text(out: &RunOutput) -> String {
    format!("{} {}", out.stdout_text(), out.stderr)
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
