//! The `ticket` host: no machines, only job tickets.
//!
//! A dispatch asks the vivi server for a job ticket (the host tool
//! `job_create`), then starts `vivi pool run <server> --job <id>` on an SSH
//! host, reading the ticket on its stdin. With `tunnel` (the default) the SSH
//! connection carries a reverse forward of the server's port, so the worker
//! reaches the server on its own loopback and the dispatcher opens no inbound
//! port. The worker pulls the job, runs it, pushes one branch and posts a
//! receipt; this host reads that receipt back with `job_show`.
//!
//! The job id is the machine id and the slot name. A slot "exists" while its
//! job is open on the server. Per-job state lives in files under
//! `<mailspace>/.vivi/ticket/<pool>/`: `<job_id>.json` (the dispatch's own job
//! name, task, branch, the local `ssh` pid) and `<job_id>.log` (its output).
//! The ticket itself is never written there.
//!
//! Contract: `JOB-CONTRACT.md` beside the server's `jobs` module.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::config::PoolConfig;
use super::doctor::Check;
use super::job::{AGENT_BRIEF, JobPlan};
use super::provider::{
    CreateSpec, ExecOutput, Hold, MachineRecord, OpenedSlot, PoolProvider, SlotLifetime,
    TicketRequest,
};
use super::runpod::{HostContext, iso};
use super::slot::{
    META_CLAIMED, META_MAILSPACE, META_POOL, META_SLOT, META_STATE, META_TASK, STATE_READY,
};
use crate::mcp::HttpUpstream;
use crate::mcp::client::{Client, result_is_error, result_text};
use vivi_error::VivariumError;

/// What a dispatch remembers about its job, in `<job_id>.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    /// The dispatch's own job name (what the result's `job=` must carry).
    job: String,
    task: String,
    branch: String,
    base: String,
    at: u64,
    /// Process id of the local `ssh` that carries the worker.
    pid: u32,
}

/// Pool provider backed by job tickets and an SSH-carried worker.
pub struct TicketProvider {
    config: PoolConfig,
    ctx: HostContext,
}

/// The `job_create` arguments for one dispatch.
fn job_create_arguments(project: &str, target: &str, plan: &JobPlan) -> Value {
    // The worker takes the commit subject from the task's first line: lead
    // with the task's subject. The pool's standing rules are not carried.
    let task = format!("{}\n\n{}", plan.subject.trim(), plan.brief);
    let mut arguments = json!({
        "project": project,
        "target": target,
        "branch": plan.refs.branch,
        "base": plan.refs.from,
        "task": task,
        // The worker runs the harness with the task in WORK_TASK_FILE.
        "harness": plan.harness_cmd.replace(AGENT_BRIEF, "\"$WORK_TASK_FILE\""),
        "timeout_s": plan.budget,
    });
    if !plan.committer.name.is_empty() && !plan.committer.email.is_empty() {
        arguments["committer"] =
            json!({ "name": plan.committer.name, "email": plan.committer.email });
    }
    arguments
}

impl TicketProvider {
    #[must_use]
    pub fn new(config: &PoolConfig, ctx: &HostContext) -> Self {
        Self {
            config: config.clone(),
            ctx: ctx.clone(),
        }
    }

    fn home(&self) -> PathBuf {
        self.ctx.dir.join("ticket").join(&self.ctx.pool)
    }

    fn entry_path(&self, id: &str) -> PathBuf {
        self.home().join(format!("{id}.json"))
    }

    fn read_entry(&self, id: &str) -> Option<Entry> {
        let path = self.entry_path(id);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(e) => {
                tracing::warn!("cannot read ticket entry {}: {e}", path.display());
                return None;
            }
        };
        match serde_json::from_str(&text) {
            Ok(entry) => Some(entry),
            Err(e) => {
                tracing::warn!("ticket entry {} is not an entry: {e}", path.display());
                None
            }
        }
    }

    fn entries(&self) -> Vec<(String, Entry)> {
        let Ok(dir) = fs::read_dir(self.home()) else {
            return Vec::new();
        };
        let mut found: Vec<(String, Entry)> = dir
            .filter_map(|entry| match entry {
                Ok(entry) => Some(entry),
                Err(e) => {
                    tracing::warn!("cannot list a ticket entry: {e}");
                    None
                }
            })
            .filter_map(|e| {
                // A name that is not UTF-8 is not one of ours.
                let Ok(name) = e.file_name().into_string() else {
                    return None;
                };
                let id = name.strip_suffix(".json")?.to_string();
                let entry = self.read_entry(&id)?;
                Some((id, entry))
            })
            .collect();
        found.sort_by_key(|(_, e)| e.at);
        found
    }

    fn server(&self) -> &str {
        self.config
            .ticket
            .server
            .as_deref()
            .unwrap_or_default()
            .trim_end_matches('/')
    }

    /// One host tool call: the result's JSON body, or why it failed.
    fn call(&self, tool: &str, arguments: &Value) -> Result<Value, VivariumError> {
        let fail = |what: String| VivariumError::Message(format!("{tool}: {what}"));
        let server = self.server();
        let bearer = crate::machine_key::sign_in(server, true)?;
        let upstream = HttpUpstream::new(server);
        let mut client = Client::connect(&upstream, &bearer)
            .map_err(|e| fail(format!("cannot connect: {e}")))?;
        let result = client
            .call_tool(tool, arguments)
            .map_err(|e| fail(e.to_string()))?;
        let text = result_text(&result);
        if result_is_error(&result) {
            return Err(fail(text));
        }
        serde_json::from_str(&text).map_err(|e| fail(format!("unreadable answer: {e}")))
    }

    /// The mailspace's own config file, parsed.
    fn mailspace_file(&self) -> Result<toml::Value, VivariumError> {
        let text = fs::read_to_string(self.ctx.dir.join("mailspace.toml"))?;
        toml::from_str(&text).map_err(|e| VivariumError::Config(format!("mailspace.toml: {e}")))
    }

    /// The project jobs are for: the configured one, else the mailspace's
    /// own name (what a hosted project is called on its server).
    fn project(&self) -> Result<String, VivariumError> {
        if let Some(project) = &self.config.ticket.project {
            return Ok(project.clone());
        }
        Ok(self
            .mailspace_file()?
            .get("name")
            .and_then(toml::Value::as_str)
            .map_or_else(|| self.ctx.mailspace.clone(), str::to_string))
    }

    /// The target jobs are for: the configured one, else the project's
    /// primary target (`default`, or the only one).
    fn target(&self) -> Result<String, VivariumError> {
        if let Some(target) = &self.config.ticket.target {
            return Ok(target.clone());
        }
        // The pump targets of the mailspace that holds this pool.
        let file = self.mailspace_file()?;
        let names: Vec<String> = file
            .get("pump")
            .and_then(|pump| pump.get("targets"))
            .and_then(toml::Value::as_table)
            .map(|targets| targets.keys().cloned().collect())
            .unwrap_or_default();
        let ids: Vec<&str> = names.iter().map(String::as_str).collect();
        match ids.as_slice() {
            [only] => Ok((*only).to_string()),
            _ if ids.contains(&"default") => Ok("default".to_string()),
            _ => Err(VivariumError::Config(
                "[ticket] target is required: the project has no single primary target".into(),
            )),
        }
    }

    /// The URL the worker uses for the server, and the forwarded port when
    /// the connection tunnels.
    fn worker_view(&self) -> Result<(String, Option<u16>), VivariumError> {
        let ticket = &self.config.ticket;
        if !ticket.tunnel() {
            let url = ticket.worker_server.clone().unwrap_or_default();
            return Ok((url.trim_end_matches('/').to_string(), None));
        }
        let server = self.server();
        let (scheme, rest) = server.split_once("://").ok_or_else(|| {
            VivariumError::Config(format!("[ticket] server {server:?} is no URL"))
        })?;
        let authority = rest.split('/').next().unwrap_or_default();
        // A suffix that is not a number (an IPv6 literal, say) is no port.
        let explicit = authority
            .rsplit_once(':')
            .and_then(|(_, p)| match p.parse::<u16>() {
                Ok(port) => Some(port),
                Err(_) => None,
            });
        let port = explicit
            .or(match scheme {
                "http" => Some(80),
                "https" => Some(443),
                _ => None,
            })
            .ok_or_else(|| {
                VivariumError::Config(format!("[ticket] server {server:?} has no usable port"))
            })?;
        Ok((format!("{scheme}://127.0.0.1:{port}"), Some(port)))
    }

    /// Start the carrier `ssh`, detached, with the ticket on its stdin.
    fn spawn_worker(&self, job_id: &str, ticket: &str) -> Result<u32, VivariumError> {
        let settings = &self.config.ticket;
        let host = settings.host.clone().unwrap_or_default();
        let (view, forward) = self.worker_view()?;
        let mut remote = String::new();
        if let Some(home) = &settings.worker_home {
            remote.push_str(&format!("VIVI_HOME=\"{home}\" "));
        }
        remote.push_str(&format!(
            "{} pool run {view} --job {job_id}",
            settings.vivi_or_default()
        ));
        let mut ssh = Command::new("ssh");
        ssh.args([
            "-T",
            "-o",
            "BatchMode=yes",
            "-o",
            "ExitOnForwardFailure=yes",
        ])
        .args([
            "-o",
            "ServerAliveInterval=15",
            "-o",
            "ServerAliveCountMax=4",
        ])
        .args(["-o", "ConnectTimeout=15", "-o", "RemoteCommand=none"]);
        if let Some(port) = forward {
            ssh.arg("-R").arg(format!("{port}:127.0.0.1:{port}"));
        }
        ssh.arg("--").arg(&host).arg(&remote);
        fs::create_dir_all(self.home())?;
        let log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.home().join(format!("{job_id}.log")))?;
        ssh.stdin(Stdio::piped())
            .stdout(log.try_clone()?)
            .stderr(log);
        // SAFETY: setsid is async-signal-safe and touches no memory of ours.
        unsafe {
            ssh.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = ssh
            .spawn()
            .map_err(|e| VivariumError::Message(format!("cannot start ssh to {host}: {e}")))?;
        let mut stdin = child.stdin.take().ok_or_else(|| {
            VivariumError::Message("ssh has no standard input to carry the ticket".into())
        })?;
        stdin.write_all(format!("{ticket}\n").as_bytes())?;
        drop(stdin);
        Ok(child.id())
    }

    fn job_show(&self, id: &str) -> Result<Option<Value>, VivariumError> {
        match self.call("job_show", &json!({ "job_id": id })) {
            Ok(view) => Ok(Some(view)),
            Err(e) if e.to_string().contains("no such job") => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn marks(&self, id: &str, entry: Option<&Entry>) -> BTreeMap<String, String> {
        let mut marks = BTreeMap::from([
            (META_MAILSPACE.to_string(), self.ctx.mailspace.clone()),
            (META_POOL.to_string(), self.ctx.pool.clone()),
            (META_SLOT.to_string(), "0".to_string()),
            (META_STATE.to_string(), STATE_READY.to_string()),
            ("ticket_job".to_string(), id.to_string()),
        ]);
        if let Some(entry) = entry {
            marks.insert(META_TASK.to_string(), entry.task.clone());
            marks.insert(META_CLAIMED.to_string(), entry.at.to_string());
        }
        marks
    }

    fn record(&self, id: &str, state: &str, entry: Option<&Entry>) -> MachineRecord {
        MachineRecord {
            id: id.to_string(),
            name: id.to_string(),
            state: state.to_string(),
            region: self.config.ticket.host.clone().unwrap_or_default(),
            created_at: entry.map(|e| iso(e.at)).unwrap_or_default(),
            metadata: self.marks(id, entry),
            exit_code: None,
        }
    }
}

fn alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 only checks that the process exists.
    pid > 0 && unsafe { libc::kill(pid, 0) } == 0
}

fn status_of(view: &Value) -> &str {
    view.get("status").and_then(Value::as_str).unwrap_or("open")
}

/// The result file the collector reads, from a worker's receipt.
fn result_lines(entry: &Entry, id: &str, receipt: &Value) -> String {
    let ok = receipt.get("ok").and_then(Value::as_bool) == Some(true);
    let commit = receipt
        .get("commit")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let class = receipt
        .get("class")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let word = match (ok && !commit.is_empty(), class) {
        (true, _) => "pushed",
        (_, "no_change") => "no_changes",
        (_, "gate_red") => "gate_failed",
        (_, "timeout") => "agent_timeout",
        (_, "provider_busy") => "provider_busy",
        (_, "agent") => "agent_failed",
        _ => "push_failed",
    };
    let step_code = |name: &str| {
        receipt
            .pointer(&format!("/steps/{name}/code"))
            .map(Value::to_string)
            .unwrap_or_default()
    };
    let seconds = receipt
        .get("seconds")
        .map(|s| s.to_string())
        .unwrap_or_default();
    let agent_rc = step_code("harness");
    let lines = [
        ("job", entry.job.as_str()),
        ("push", word),
        ("sha", commit),
        ("branch", entry.branch.as_str()),
        ("start", entry.base.as_str()),
        ("agent_rc", agent_rc.as_str()),
        ("agent_s", seconds.as_str()),
        ("job_s", seconds.as_str()),
        ("ticket_job", id),
        ("ticket_class", class),
        (
            "ticket_failed_step",
            receipt
                .get("failed_step")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        ),
    ];
    lines
        .iter()
        .map(|(k, v)| format!("{k}={v}\n"))
        .collect::<String>()
}

impl PoolProvider for TicketProvider {
    fn slot_lifetime(&self) -> SlotLifetime {
        SlotLifetime::PerDispatch
    }

    fn submit_ticket(&self, request: &TicketRequest) -> Option<Result<OpenedSlot, VivariumError>> {
        Some(self.submit(request))
    }

    fn billing_note(&self) -> String {
        "a job ticket holds no machine".to_string()
    }

    fn ensure_home(&self) -> Result<Option<String>, VivariumError> {
        Ok(None)
    }

    fn doctor(&self, _harness_env: &[String]) -> Vec<Check> {
        let host = self.config.ticket.host.clone().unwrap_or_default();
        let probe = Command::new("ssh")
            .args(["-T", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "--"])
            .arg(&host)
            .arg("true")
            .output();
        match probe {
            Ok(out) if out.status.success() => vec![Check::ok("ticket", "host", host)],
            _ => vec![Check::fail(
                "ticket",
                "host",
                format!("cannot ssh to {host}"),
                format!("check that `ssh -o BatchMode=yes {host} true` succeeds without a prompt"),
            )],
        }
    }

    fn harness_env_gaps(&self, _names: &[String]) -> Result<Vec<String>, VivariumError> {
        Ok(Vec::new())
    }

    fn env_gaps_at_dispatch(&self, _names: &[String]) -> Result<Vec<String>, VivariumError> {
        Ok(Vec::new())
    }

    fn env_gap_hint(&self, missing: &[String]) -> String {
        format!(
            "the worker host's own environment must supply: {}",
            missing.join(", ")
        )
    }

    fn live_test_notice(&self) -> String {
        "a ticket pool has no live test".to_string()
    }

    fn supplies_build_mount(&self) -> bool {
        false
    }

    fn build_persists(&self) -> bool {
        true
    }

    fn runs_unprivileged(&self) -> bool {
        true
    }

    /// This pool's open jobs, from the local per-job files.
    fn list(&self) -> Result<Vec<MachineRecord>, VivariumError> {
        let mut machines = Vec::new();
        for (id, entry) in self.entries() {
            let open = self
                .job_show(&id)?
                .is_some_and(|view| status_of(&view) == "open");
            if open {
                machines.push(self.record(&id, "running", Some(&entry)));
            }
        }
        Ok(machines)
    }

    fn create(&self, _spec: &CreateSpec) -> Result<String, VivariumError> {
        Err(VivariumError::Message(
            "a ticket pool has no resting slots: a job ticket is made for each dispatch (`vivi pool dispatch`)".into(),
        ))
    }

    /// End the local carrier of the job, if it still runs, and forget the
    /// job's files. The ticket itself ends with its receipt or its expiry.
    fn destroy(&self, id: &str) -> Result<(), VivariumError> {
        if let Some(entry) = self.read_entry(id)
            && alive(entry.pid)
            && let Ok(pid) = i32::try_from(entry.pid)
        {
            // SAFETY: ends the ssh process this host started for the job.
            unsafe { libc::kill(pid, libc::SIGTERM) };
        }
        match fs::remove_file(self.entry_path(id)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(VivariumError::Message(format!(
                "cannot forget the files of job {id}: {e}"
            ))),
        }
    }

    fn destroy_command(&self, id: &str) -> String {
        format!(
            "kill the ssh process of {id} (its pid is in {})",
            self.entry_path(id).display()
        )
    }

    fn machine(&self, id: &str) -> Result<MachineRecord, VivariumError> {
        let entry = self.read_entry(id);
        let Some(view) = self.job_show(id)? else {
            return Ok(MachineRecord::gone(id));
        };
        let state = match status_of(&view) {
            "done" => "stopped",
            "expired" => "failed",
            _ => {
                // The carrier ended and no receipt came: the worker never
                // finished (no free slot there, a lost connection). Look
                // again before believing it, in case the receipt just landed.
                let dead = entry.as_ref().is_some_and(|e| !alive(e.pid));
                let still_open = dead
                    && self
                        .job_show(id)?
                        .is_some_and(|view| status_of(&view) == "open");
                if still_open { "failed" } else { "running" }
            }
        };
        Ok(self.record(id, state, entry.as_ref()))
    }

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
        _id: &str,
        _hold: &Hold,
        _path: &str,
        _content: &[u8],
    ) -> Result<(), VivariumError> {
        Err(no_slots())
    }

    fn seal(
        &self,
        _id: &str,
        _drop_env: &[&str],
        _key: &str,
        _value: &str,
    ) -> Result<(), VivariumError> {
        Err(no_slots())
    }

    fn start(&self, _id: &str) -> Result<(), VivariumError> {
        Err(no_slots())
    }

    fn stop(&self, id: &str) -> Result<(), VivariumError> {
        self.destroy(id)
    }

    fn exec(&self, _id: &str, _script: &str, _timeout: u32) -> Result<ExecOutput, VivariumError> {
        Err(no_slots())
    }

    /// The finished job's result, as the files a slot would have left: the
    /// result lines, the receipt (as `agent.out`), and the worker's tail.
    fn read_stopped(
        &self,
        id: &str,
        paths: &[&str],
    ) -> Result<Option<BTreeMap<String, Vec<u8>>>, VivariumError> {
        let (Some(entry), Some(view)) = (self.read_entry(id), self.job_show(id)?) else {
            return Ok(Some(BTreeMap::new()));
        };
        let mut files = BTreeMap::new();
        let Some(receipt) = view.get("receipt").filter(|r| !r.is_null()) else {
            return Ok(Some(files));
        };
        let tail = receipt
            .get("tail")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let content: [(usize, Vec<u8>); 4] = [
            (0, result_lines(&entry, id, receipt).into_bytes()),
            (1, receipt.to_string().into_bytes()),
            (2, Vec::new()),
            (3, tail.as_bytes().to_vec()),
        ];
        for (index, bytes) in content {
            if let Some(path) = paths.get(index) {
                files.insert((*path).to_string(), bytes);
            }
        }
        Ok(Some(files))
    }
}

fn no_slots() -> VivariumError {
    VivariumError::Message("a ticket pool has no slots; it only makes job tickets".into())
}

impl TicketProvider {
    fn submit(&self, request: &TicketRequest) -> Result<OpenedSlot, VivariumError> {
        let plan = request.plan;
        if plan.read_only || plan.refs.branch.is_empty() {
            return Err(VivariumError::Message(
                "a ticket pool runs jobs that push a branch; read-only jobs are not supported"
                    .into(),
            ));
        }
        let live = self.list()?.len();
        if live >= self.config.slots as usize {
            return Err(VivariumError::Message(format!(
                "no free slot in pool {}: it allows {} open job(s) at once and holds {live}",
                request.pool.pool, self.config.slots
            )));
        }
        let project = self.project()?;
        let target = self.target()?;
        let created = self.call("job_create", &job_create_arguments(&project, &target, plan))?;
        let field = |key: &str| {
            created
                .get(key)
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| {
                    VivariumError::Message(format!("job_create: the answer has no {key}"))
                })
        };
        let (job_id, ticket) = (field("job_id")?, field("ticket")?);
        let pid = self.spawn_worker(&job_id, &ticket)?;
        let entry = Entry {
            job: plan.job.clone(),
            task: plan.task.clone(),
            branch: plan.refs.branch.clone(),
            base: plan.refs.from.clone(),
            at: request.now,
            pid,
        };
        fs::create_dir_all(self.home())?;
        fs::write(
            self.entry_path(&job_id),
            serde_json::to_vec(&entry).unwrap_or_default(),
        )?;
        Ok(OpenedSlot {
            id: job_id.clone(),
            name: job_id,
            index: 0,
            hold: Hold {
                nonce: String::new(),
            },
            hardware: self.config.ticket.host.clone().unwrap_or_default(),
            created_at: iso(request.now),
            abandoned: Vec::new(),
        })
    }
}
