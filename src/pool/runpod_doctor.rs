//! The machine-host half of `vivi pool doctor` for the `runpod` host, and the
//! price the live test must quote before it may spend. Everything here reads:
//! the CLI's version, the key file's mode, the account's identity, the SSH
//! keys the account holds, the GPU market, and the account's pods. Nothing is
//! created, changed or terminated.

use std::process::{Command, Stdio};

use super::doctor::Check;
use super::runpod::{RunpodProvider, iso};
use super::runpod_cli::check_key_file;
use super::runpod_name::{PREFIX, PodName};
use super::runpod_pod::{self as pod_api, GpuType, Pod, User};
use super::slot::DOCTOR_SUFFIX;

pub const GROUP: &str = "Provider (RunPod)";
/// A balance under this many dollars is worth a warning.
pub const LOW_BALANCE: f64 = 5.0;
/// The time the live test is expected to rent a pod for, in minutes, for the
/// price it quotes.
const TEST_MINUTES: f64 = 15.0;

/// One pod of the pool, as the check lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct PodLine {
    pub id: String,
    pub name: PodName,
    pub state: String,
    pub cost_per_hr: Option<f64>,
}

/// The account's Vivi-named pods, sorted by whose they are.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Survey {
    /// This pool's.
    pub mine: Vec<PodLine>,
    /// This mailspace's, in a pool the mailspace no longer (or never) had.
    pub strays: Vec<PodLine>,
    /// Another mailspace's: not touched, only counted.
    pub others: usize,
    /// Named like Vivi's but not in its layout: `(id, name)`.
    pub malformed: Vec<(String, String)>,
}

impl Survey {
    /// Sort `pods` for the pool `pool` of mailspace `mailspace`, whose
    /// sibling pools are `pools` (a pool's `-doctor` self-test pool counts as
    /// known).
    #[must_use]
    pub fn of(pods: &[Pod], mailspace: &str, pool: &str, pools: &[String]) -> Self {
        let mut survey = Self::default();
        for pod in pods.iter().filter(|p| p.name.starts_with(PREFIX)) {
            let Some(name) = PodName::decode(&pod.name) else {
                survey.malformed.push((pod.id.clone(), pod.name.clone()));
                continue;
            };
            let line = PodLine {
                id: pod.id.clone(),
                state: pod.state().to_string(),
                cost_per_hr: pod.cost_per_hr,
                name,
            };
            if line.name.mailspace != mailspace {
                survey.others += 1;
            } else if line.name.pool == pool {
                survey.mine.push(line);
            } else if !known(&line.name.pool, pools) {
                survey.strays.push(line);
            }
        }
        survey
    }
}

fn known(pool: &str, pools: &[String]) -> bool {
    pools
        .iter()
        .any(|p| p == pool || pool.strip_suffix(DOCTOR_SUFFIX) == Some(p.as_str()))
}

/// What the probes saw. `None` means a probe was not attempted because one it
/// depends on failed.
#[derive(Debug, Clone)]
pub struct RunpodFacts {
    pub pool: String,
    pub key_file: String,
    pub cli: Result<String, String>,
    pub key: Result<(), String>,
    pub account: Option<Result<User, String>>,
    pub ssh_keys: Option<Result<usize, String>>,
    /// Names of the local tools that are missing (`ssh`, `ssh-keyscan`).
    pub missing_tools: Vec<String>,
    pub gpus: Option<Result<Vec<GpuType>, String>>,
    pub pods: Option<Result<Survey, String>>,
    pub types: Vec<String>,
    pub cloud: String,
    pub centers: Vec<String>,
    pub gpu_count: u32,
    pub image: String,
    pub now: u64,
}

impl RunpodProvider {
    /// Probe `runpodctl`, the account and the market. Reads only.
    pub(super) fn gather(&self) -> RunpodFacts {
        let cfg = &self.config.runpod;
        let cli = self
            .bare
            .run(
                &["--version".to_string()],
                None,
                super::runpod_cli::CALL_TIMEOUT,
            )
            .map_err(|e| e.to_string())
            .and_then(|o| {
                if o.succeeded() {
                    Ok(o.stdout_text())
                } else {
                    Err(o.stderr.trim().to_string())
                }
            });
        let key = check_key_file(&cfg.key_path()).map_err(|e| e.to_string());
        let usable = cli.is_ok() && key.is_ok();
        let read = |what: &str, args: Vec<String>| {
            usable.then(|| self.require(what, &args).map_err(|e| e.to_string()))
        };
        let account = read("user", pod_api::user_args())
            .map(|r| r.and_then(|t| pod_api::parse_user(&t).map_err(|e| e.to_string())));
        let reachable = matches!(account, Some(Ok(_)));
        let on_account = |what: &str, args: Vec<String>| {
            reachable.then(|| self.require(what, &args).map_err(|e| e.to_string()))
        };
        let ssh_keys = on_account("ssh list-keys", pod_api::ssh_keys_args())
            .map(|r| r.and_then(|t| pod_api::count_ssh_keys(&t).map_err(|e| e.to_string())));
        let gpus = on_account("gpu list", pod_api::gpus_args())
            .map(|r| r.and_then(|t| pod_api::parse_gpus(&t).map_err(|e| e.to_string())));
        let pods = on_account("pod list", pod_api::list_args()).map(|r| {
            r.and_then(|t| pod_api::parse_pods(&t).map_err(|e| e.to_string()))
                .map(|pods| {
                    Survey::of(
                        &pods,
                        &self.ctx.mailspace,
                        &self.pool_name(),
                        &self.ctx.pools,
                    )
                })
        });
        RunpodFacts {
            pool: self.pool_name(),
            key_file: cfg.key_path().display().to_string(),
            cli,
            key,
            account,
            ssh_keys,
            missing_tools: missing_local_tools(),
            gpus,
            pods,
            types: cfg.gpu_types.clone(),
            cloud: cfg.cloud_type().to_string(),
            centers: cfg.data_centers.clone(),
            gpu_count: cfg.gpu_count,
            image: self.config.image.clone(),
            now: super::work::epoch_secs(),
        }
    }

    /// The pool this provider serves.
    fn pool_name(&self) -> String {
        self.ctx.pool.clone()
    }

    /// The provider's checks, as the pool doctor lists them.
    #[must_use]
    pub fn doctor_checks(&self, _harness_env: &[String]) -> Vec<Check> {
        checks(&self.gather())
    }

    /// The words the live test is refused with unless `--allow-spend` is given.
    #[must_use]
    pub fn spend_notice(&self) -> String {
        let cfg = &self.config.runpod;
        let market = self.market();
        let first = cfg.gpu_types.first().map_or("?", String::as_str);
        let price = market
            .iter()
            .flatten()
            .find(|g| g.gpu_id == first)
            .and_then(|g| g.price(cfg.cloud_type()));
        spend_text(&cfg.gpu_types, cfg.gpu_count, cfg.cloud_type(), price)
    }
}

/// The refusal text of the live test.
#[must_use]
pub fn spend_text(types: &[String], count: u32, cloud: &str, price: Option<f64>) -> String {
    let cost = match price {
        Some(per_gpu) => {
            let hourly = per_gpu * f64::from(count);
            format!(
                "{} costs ${per_gpu:.2}/h per GPU on the {} cloud, so {count} GPU(s) cost ${hourly:.2}/h (about ${:.2} for {TEST_MINUTES:.0} minutes)",
                types.first().map_or("?", String::as_str),
                cloud.to_lowercase(),
                hourly * TEST_MINUTES / 60.0
            )
        }
        None => "its price could not be read just now (`runpodctl gpu list` shows it)".to_string(),
    };
    format!(
        "`vivi pool doctor --test` on a RunPod pool would rent a real GPU pod ({} tried in order), run a stand-in job on it for several minutes and terminate it; the pod bills by the second from the moment it exists, and a run that is interrupted or fails can leave a pod billing until `vivi pool reconcile` or its provider-side deadline. For the first type, {cost}. Nothing was rented. To run the test anyway, pass --allow-spend",
        types.join(", ")
    )
}

fn missing_local_tools() -> Vec<String> {
    ["ssh", "ssh-keyscan"]
        .into_iter()
        .filter(|tool| {
            // Any exit status means the tool exists; only "not found" is missing.
            Command::new(tool)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_err()
        })
        .map(str::to_string)
        .collect()
}

/// Turn what was seen into checks.
#[must_use]
pub fn checks(f: &RunpodFacts) -> Vec<Check> {
    let mut list = vec![
        cli_check(f),
        key_check(f),
        account_check(f),
        ssh_keys_check(f),
        tools_check(f),
    ];
    list.extend(gpu_checks(f));
    list.push(image_check(f));
    list.extend(pod_checks(f));
    list
}

fn cli_check(f: &RunpodFacts) -> Check {
    match &f.cli {
        Ok(version) => Check::ok(GROUP, "runpodctl", version.clone()),
        Err(e) => Check::fail(
            GROUP,
            "runpodctl",
            e.clone(),
            "install runpodctl (the RunPod command line) and put it on PATH",
        ),
    }
}

fn key_check(f: &RunpodFacts) -> Check {
    match &f.key {
        Ok(()) => Check::ok(
            GROUP,
            "api key file",
            format!("{} (mode ok; not read by vivi)", f.key_file),
        ),
        Err(e) => Check::fail(
            GROUP,
            "api key file",
            e.clone(),
            format!("chmod 600 {}", f.key_file),
        ),
    }
}

fn account_check(f: &RunpodFacts) -> Check {
    match &f.account {
        Some(Ok(user)) => match user.client_balance.filter(|b| *b < LOW_BALANCE) {
            Some(low) => Check::warn(
                GROUP,
                "account",
                format!(
                    "authenticated as {}; balance is low (${low:.2}) and pods stop when it reaches zero",
                    user.id
                ),
                "add credit in the RunPod console",
            ),
            None => Check::ok(GROUP, "account", format!("authenticated as {}", user.id)),
        },
        Some(Err(e)) => Check::fail(
            GROUP,
            "account",
            format!("the API did not accept the key: {e}"),
            format!("check the key in {}", f.key_file),
        ),
        None => Check::skipped(GROUP, "account", "needs runpodctl and a usable key file"),
    }
}

fn ssh_keys_check(f: &RunpodFacts) -> Check {
    match &f.ssh_keys {
        Some(Ok(0)) => Check::warn(
            GROUP,
            "ssh keys",
            "the account holds no SSH key, so a pod will not accept your login",
            "add your public key in the RunPod console settings (runpodctl ssh add-key)",
        ),
        Some(Ok(n)) => Check::ok(
            GROUP,
            "ssh keys",
            format!("{n} key(s) on the account (a pod accepts these)"),
        ),
        Some(Err(e)) => Check::warn(GROUP, "ssh keys", e.clone(), "runpodctl ssh list-keys"),
        None => Check::skipped(GROUP, "ssh keys", "needs a working account"),
    }
}

fn tools_check(f: &RunpodFacts) -> Check {
    if f.missing_tools.is_empty() {
        return Check::ok(GROUP, "local ssh tools", "ssh and ssh-keyscan are present");
    }
    Check::fail(
        GROUP,
        "local ssh tools",
        format!("missing: {}", f.missing_tools.join(", ")),
        "install OpenSSH (ssh and ssh-keyscan) and put it on PATH",
    )
}

fn gpu_checks(f: &RunpodFacts) -> Vec<Check> {
    let market = match &f.gpus {
        Some(Ok(market)) => market,
        Some(Err(e)) => {
            return vec![Check::warn(
                GROUP,
                "gpu types",
                format!("cannot read the GPU market: {e}"),
                "runpodctl gpu list",
            )];
        }
        None => {
            return vec![Check::skipped(
                GROUP,
                "gpu types",
                "needs a working account",
            )];
        }
    };
    f.types.iter().map(|t| gpu_check(f, market, t)).collect()
}

fn gpu_check(f: &RunpodFacts, market: &[GpuType], wanted: &str) -> Check {
    let name = format!("gpu {wanted}");
    let Some(gpu) = market.iter().find(|g| g.gpu_id == wanted) else {
        return Check::warn(
            GROUP,
            &name,
            "not in `runpodctl gpu list` (a misspelled id, or a type RunPod no longer offers)",
            "runpodctl gpu list --include-unavailable",
        );
    };
    let price = gpu.price(&f.cloud).map_or_else(
        || format!("not offered on the {} cloud", f.cloud.to_lowercase()),
        |p| {
            format!(
                "${p:.2}/h per GPU ({} GPU(s) per pod), {} cloud",
                f.gpu_count,
                f.cloud.to_lowercase()
            )
        },
    );
    if gpu.in_stock(&f.cloud, &f.centers) {
        Check::ok(GROUP, &name, format!("{price}; stock {}", gpu.stock_status))
    } else {
        Check::warn(
            GROUP,
            &name,
            format!("{price}; no stock in the configured cloud and data centres right now"),
            "a dispatch tries the next type in `gpu_types`, or fails with no pod rented",
        )
    }
}

fn image_check(f: &RunpodFacts) -> Check {
    if f.image.contains("devel") {
        return Check::warn(
            GROUP,
            "image",
            format!(
                "{} looks like a `devel` image; those did not publish SSH when tried, and a pod needs SSH here",
                f.image
            ),
            "use an image that runs an SSH server, e.g. a `runpod/pytorch` runtime image",
        );
    }
    Check::ok(GROUP, "image", f.image.clone())
}

fn pod_checks(f: &RunpodFacts) -> Vec<Check> {
    let survey = match &f.pods {
        Some(Ok(survey)) => survey,
        Some(Err(e)) => {
            return vec![Check::fail(
                GROUP,
                "pods",
                e.clone(),
                "runpodctl pod list --all",
            )];
        }
        None => return vec![Check::skipped(GROUP, "pods", "needs a working account")],
    };
    let mut list = vec![pods_check(f, &survey.mine)];
    list.push(strays_check(survey));
    list
}

fn pods_check(f: &RunpodFacts, mine: &[PodLine]) -> Check {
    if mine.is_empty() {
        return Check::ok(
            GROUP,
            "pods",
            format!(
                "none of pool {} exist (they exist only while a job runs)",
                f.pool
            ),
        );
    }
    let late: Vec<String> = mine
        .iter()
        .filter(|p| p.name.deadline < f.now)
        .map(|p| format!("{} (deadline {} has passed)", p.id, iso(p.name.deadline)))
        .collect();
    let text = mine
        .iter()
        .map(|p| pod_text(p, f.now))
        .collect::<Vec<_>>()
        .join("; ");
    if late.is_empty() {
        return Check::ok(GROUP, "pods", text);
    }
    Check::fail(
        GROUP,
        "pods",
        format!(
            "{text}; past the provider-side deadline: {}",
            late.join(", ")
        ),
        format!(
            "vivi pool reconcile --pool {} (terminates it), or runpodctl pod delete <id>",
            f.pool
        ),
    )
}

fn pod_text(p: &PodLine, now: u64) -> String {
    let cost = p
        .cost_per_hr
        .map_or_else(String::new, |c| format!(", ${c:.2}/h"));
    format!(
        "{} {} task {} up {}s{cost}, ends by {}",
        p.id,
        p.state,
        p.name.task,
        now.saturating_sub(p.name.claimed),
        iso(p.name.deadline)
    )
}

fn strays_check(survey: &Survey) -> Check {
    let others = if survey.others == 0 {
        String::new()
    } else {
        format!(
            "; {} pod(s) of other mailspaces are left alone",
            survey.others
        )
    };
    if survey.strays.is_empty() && survey.malformed.is_empty() {
        return Check::ok(GROUP, "stray pods", format!("none{others}"));
    }
    let mut found: Vec<String> = survey
        .strays
        .iter()
        .map(|p| format!("{} (pool {}, {})", p.id, p.name.pool, p.state))
        .collect();
    found.extend(
        survey
            .malformed
            .iter()
            .map(|(id, n)| format!("{id} (named {n:?}, not Vivi's layout)")),
    );
    let first = survey
        .strays
        .first()
        .map(|p| p.id.clone())
        .or_else(|| survey.malformed.first().map(|m| m.0.clone()));
    Check::fail(
        GROUP,
        "stray pods",
        format!(
            "pods with Vivi's name prefix that belong to no pool of this mailspace, billing until terminated: {}{others}",
            found.join(", ")
        ),
        format!(
            "runpodctl pod delete {} (reconcile does not touch pods outside its pools)",
            first.unwrap_or_default()
        ),
    )
}

#[cfg(test)]
#[path = "runpod_doctor_test.rs"]
mod tests;
