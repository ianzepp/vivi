//! The state the steps of the live test share: the provider, the isolated
//! config, a temporary mailspace that holds the throwaway tasks, and the jobs
//! dispatched so far.
//!
//! The mailspace lives in a temporary directory, so the project's own
//! mailspace is never written: no task, event or note of the test reaches it.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use super::collect::{ErrorWatch, Pacing, RoundLine};
use super::config::PoolConfig;
use super::dispatch::{self, DispatchFlags, Launch};
use super::github::Secret;
use super::job;
use super::pools::PoolSet;
use super::progress::{Beat, HEARTBEAT, Progress};
use super::provider::PoolProvider;
use super::record::DispatchRecord;
use super::run::{self, CreateOptions};
use super::selftest_source::{self as source, harness_command};
use super::slot::{PoolId, SlotStatus};
use super::work::{self, WaitOptions, Waited};
use crate::error::VivariumError;
use crate::mailspace::{Mailspace, SendRequest};

/// Every wait and pause the test uses. `Default` is for a live provider;
/// [`Timing::instant`] is for tests against the fake.
#[derive(Debug, Clone)]
pub struct Timing {
    pub pacing: Pacing,
    pub create: CreateOptions,
    /// Pause between polls of the provider.
    pub poll: Duration,
    /// Pause between rounds of `wait`.
    pub wait_poll: Duration,
    /// How long a machine may take to change state.
    pub settle: Duration,
    /// How long one short job may take, dispatch to collection.
    pub job: Duration,
    /// How long a finished job's machine may take to stop by itself.
    pub stops_by_itself: Duration,
    /// How long a `wait` that must outlast the provider token runs.
    pub long_wait: Duration,
    /// How often a waiting step says it is still working.
    pub heartbeat: Duration,
}

/// A heartbeat interval no fake-provider test reaches.
const QUIET: Duration = Duration::from_hours(1);

/// How long the `--long` job sleeps: past the six minutes a provider token
/// lives.
pub const LONG_JOB_SECS: u64 = 380;

impl Default for Timing {
    fn default() -> Self {
        Self {
            pacing: Pacing::default(),
            create: CreateOptions {
                slots: Some(2),
                timeout: Duration::from_mins(6),
                ..CreateOptions::default()
            },
            poll: Duration::from_secs(3),
            wait_poll: Duration::from_secs(5),
            settle: Duration::from_secs(90),
            job: Duration::from_mins(5),
            stops_by_itself: Duration::from_mins(5),
            long_wait: Duration::from_mins(12),
            heartbeat: HEARTBEAT,
        }
    }
}

impl Timing {
    /// No pauses and short limits, for the fake provider.
    #[must_use]
    pub fn instant() -> Self {
        let tick = Duration::from_millis(1);
        let limit = Duration::from_millis(400);
        Self {
            pacing: Pacing {
                poll: Duration::ZERO,
                ready_timeout: limit,
                stop_timeout: limit,
                start_grace: Duration::ZERO,
            },
            create: CreateOptions {
                slots: Some(2),
                spacing: Duration::ZERO,
                poll: Duration::ZERO,
                timeout: limit,
                confirm_timeout: limit,
                heartbeat: QUIET,
            },
            poll: tick,
            wait_poll: tick,
            settle: limit,
            job: limit,
            stops_by_itself: limit,
            long_wait: limit,
            heartbeat: QUIET,
        }
    }
}

/// One dispatched job of the test.
#[derive(Debug, Clone)]
pub struct Job {
    pub task: String,
    pub record: DispatchRecord,
}

pub struct World<'a> {
    pub provider: &'a dyn PoolProvider,
    pub config: PoolConfig,
    /// The self-test pool: `<pool>-doctor` of the same mailspace.
    pub id: PoolId,
    pub timing: Timing,
    pub cancelled: &'a dyn Fn() -> bool,
    /// Where the lines of a running step go: its start lines and heartbeats.
    pub progress: &'a Progress,
    /// Set once cleanup starts, so an interrupt cannot abort the cleanup.
    pub cleaning: bool,
    pub jobs: BTreeMap<&'static str, Job>,
    space: Mailspace,
    _dir: tempfile::TempDir,
    counter: u32,
}

impl<'a> World<'a> {
    /// Build the world for the real pool `real_id`: a temporary mailspace and
    /// the isolated config. The test slots belong to `real_id`'s mailspace and
    /// to its `-doctor` pool.
    ///
    /// # Errors
    /// Returns an error if the temporary mailspace cannot be created.
    pub fn new(
        provider: &'a dyn PoolProvider,
        real: &PoolConfig,
        real_id: &PoolId,
        timing: Timing,
        cancelled: &'a dyn Fn() -> bool,
        progress: &'a Progress,
    ) -> Result<Self, VivariumError> {
        let dir = tempfile::tempdir()?;
        let mut space = Mailspace::init(Some(dir.path()))?;
        space.add_identity("mind")?;
        space.add_identity("hand")?;
        Ok(Self {
            provider,
            config: source::test_config(real),
            id: real_id.doctor(),
            timing,
            cancelled,
            progress,
            cleaning: false,
            jobs: BTreeMap::new(),
            space,
            _dir: dir,
            counter: 0,
        })
    }

    /// The pool's slots as the provider lists them now.
    ///
    /// # Errors
    /// Returns the provider's error as text.
    pub fn slots(&self) -> Result<Vec<SlotStatus>, String> {
        run::pool_status(self.provider, &self.id).map_err(|e| e.to_string())
    }

    /// `name=state` for each slot, for an observed-text.
    #[must_use]
    pub fn describe(slots: &[SlotStatus]) -> String {
        if slots.is_empty() {
            return "no slots".to_string();
        }
        let parts: Vec<String> = slots
            .iter()
            .map(|s| format!("{}={}", s.name, s.state))
            .collect();
        parts.join(", ")
    }

    /// Poll `probe` until it returns a value, an error, or `limit` passes.
    ///
    /// # Errors
    /// Returns the probe's error, `interrupted`, or a timeout naming `what`.
    pub fn until<T>(
        &self,
        what: &str,
        limit: Duration,
        mut probe: impl FnMut() -> Result<Option<T>, String>,
    ) -> Result<T, String> {
        let started = Instant::now();
        let mut beat = Beat::new(self.timing.heartbeat);
        loop {
            if let Some(found) = probe()? {
                return Ok(found);
            }
            if !self.cleaning && (self.cancelled)() {
                return Err("interrupted".to_string());
            }
            if started.elapsed() >= limit {
                return Err(format!(
                    "timed out after {}s waiting for {what}",
                    started.elapsed().as_secs()
                ));
            }
            if beat.due(started.elapsed()) {
                self.progress.say(&format!(
                    "waiting  for {what}  {}s of {}s limit",
                    started.elapsed().as_secs(),
                    limit.as_secs()
                ));
            }
            std::thread::sleep(self.timing.poll);
        }
    }

    fn new_task(&mut self, label: &str) -> Result<String, VivariumError> {
        self.counter += 1;
        let sent = self.space.send(SendRequest {
            from: "mind".into(),
            to: vec!["hand".into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: format!("vivi pool doctor --test: {label} ({})", self.counter),
            body: "Throwaway task of `vivi pool doctor --test`. It lives in a temporary mailspace that is deleted when the test ends.\n".into(),
            role: "tasks".into(),
            kind: Some("task".into()),
            reply_to: None,
            depends_on: Vec::new(),
        })?;
        sent.delivered
            .first()
            .map(|d| d.handle.clone())
            .ok_or_else(|| VivariumError::Message("the test task was not delivered".into()))
    }

    /// Dispatch one stand-in job as `label`: a throwaway task, a job that
    /// sleeps `sleep` seconds before it writes, a budget of `budget` seconds.
    ///
    /// # Errors
    /// Returns the dispatch error (for example `no free slot`) unchanged.
    pub fn dispatch(
        &mut self,
        label: &'static str,
        sleep: u64,
        budget: u64,
        refs: (&str, &str),
    ) -> Result<DispatchRecord, VivariumError> {
        let task = self.new_task(label)?;
        let mut config = self.config.clone();
        if let Some(harness) = config.harness.as_mut() {
            harness.run = harness_command(sleep);
        }
        let flags = DispatchFlags {
            model: source::MODEL.to_string(),
            from: Some(refs.0.to_string()),
            branch: Some(refs.1.to_string()),
            budget: Some(budget),
            allow_default_branch: true,
            read_only: false,
        };
        let id = dispatch::new_job_id(&task, u128::from(self.counter), std::process::id());
        let plan = dispatch::plan_job(
            &config,
            &self.id.pool,
            (&task, label, ""),
            &flags,
            job::DEFAULT_RULES.to_string(),
            id,
        )?;
        let token = Secret::new(String::new());
        let start = refs.0;
        let launch = Launch {
            config: &config,
            plan: &plan,
            token: &token,
            start,
            progress: self.progress,
        };
        let record = work::submit(
            &self.space,
            self.provider,
            &self.id,
            &launch,
            &self.timing.pacing,
        )?;
        self.jobs.insert(
            label,
            Job {
                task,
                record: record.clone(),
            },
        );
        Ok(record)
    }

    /// The dispatched job `label`.
    ///
    /// # Errors
    /// Returns an error text if the step that dispatches it did not run.
    pub fn job(&self, label: &str) -> Result<&Job, String> {
        self.jobs
            .get(label)
            .ok_or_else(|| format!("job {label} was never dispatched"))
    }

    /// `wait` for the job `label` and return its line.
    ///
    /// # Errors
    /// Returns the wait error as text, or a text for an unexpected result.
    pub fn wait_for(&self, label: &str, limit: Duration) -> Result<RoundLine, String> {
        let task = self.job(label)?.task.clone();
        let options = WaitOptions {
            limit,
            poll: self.timing.wait_poll,
            pacing: self.timing.pacing.clone(),
            heartbeat: self.timing.heartbeat,
            cancelled: if self.cleaning {
                &|| false
            } else {
                self.cancelled
            },
        };
        let mut say = |line: &str| self.progress.say(line);
        let mut watch = ErrorWatch::default();
        let set = PoolSet::single(&self.id, self.provider);
        match work::wait(&self.space, &set, &[task], &options, &mut watch, &mut say) {
            Ok(Waited::Settled(mut lines)) => lines
                .pop()
                .ok_or_else(|| "wait settled with no line".to_string()),
            Ok(Waited::NothingOpen) => Err("the dispatch was already closed".to_string()),
            Err(e) => Err(e.to_string()),
        }
    }

    /// The fields the latest `pool_collected` record of `label` holds.
    ///
    /// # Errors
    /// Returns an error text if the result was never recorded on the task.
    pub fn collected(&self, label: &str) -> Result<BTreeMap<String, String>, String> {
        let task = &self.job(label)?.task;
        let events = self.space.pool_events().map_err(|e| e.to_string())?;
        let note = events
            .iter()
            .rev()
            .find(|e| &e.handle == task && e.event_type == "pool_collected")
            .map(|e| e.note.clone())
            .ok_or_else(|| format!("no result is recorded on the task of {label}"))?;
        let value: serde_json::Value = serde_json::from_str(&note).map_err(|e| e.to_string())?;
        let fields = value.as_object().into_iter().flatten();
        Ok(fields
            .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
            .collect())
    }

    /// Run one `reconcile` round.
    ///
    /// # Errors
    /// Returns an error text if the round could not run.
    pub fn reconcile(&self) -> Result<work::ReconcileReport, String> {
        let set = PoolSet::single(&self.id, self.provider);
        work::reconcile(&self.space, &set, &self.timing.pacing).map_err(|e| e.to_string())
    }

    /// True when the task of `label` still has an open dispatch.
    ///
    /// # Errors
    /// Returns an error text if the event log cannot be read.
    pub fn is_open(&self, label: &str) -> Result<bool, String> {
        let task = &self.job(label)?.task;
        let open = work::open_dispatches(&self.space).map_err(|e| e.to_string())?;
        Ok(open.iter().any(|d| &d.task == task))
    }
}
