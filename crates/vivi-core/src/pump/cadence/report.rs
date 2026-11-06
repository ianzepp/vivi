//! The reporting step for command checks: it pays what a staged result owes.
//!
//! [`super::result::collect`] stages a genuine red and a green over an open
//! red need; the staged result holds the role's single-flight slot until this
//! step settles it:
//!
//! - a red with no open red need files one need, from the role to the pump's
//!   reply recipient, marked `cadence` and naming the execution's target;
//! - a red while that need is still open replies once on it with the new
//!   commit, exit and task instead of filing again;
//! - a green over an open red need closes it (`<role> is green again at
//!   <commit>`) and records the green commit.
//!
//! A red need closed or withdrawn by someone else counts as no open need: the
//! next red files a fresh one, and a green records green without a recovery
//! report. Only a paid result is applied. A send, reply or close that fails is
//! logged and the result stays staged for the next tick; a send whose handle
//! is unknown raises an alarm and is never retried, so no need is filed twice.
//!
//! Filing is crash-safe through `pump.db` main-state markers keyed by role,
//! each naming the job it belongs to: `sending` is written before `need send`,
//! `filed` (job and handle) right after the handle parses, and `replied` after
//! a reply lands. They are dropped once the in-memory check no longer stages
//! that job, which is only after the applied result was saved.

use std::fs;
use std::path::PathBuf;

use crate::mailspace::Mailspace;
use crate::pump::flow::mail::{MailReader, MailspacePort, NeedStatus};
use crate::pump::flow::residual::parse_need_handle;
use crate::pump::flow::tick::raise_once;
use crate::pump::host::{Ctx, shell_quote};
use crate::pump::jobs;
use crate::pump::record::PumpMailspace;
use crate::pump::state::State;
use crate::pump::store::{DB_FILENAME, PumpStore};
use vivi_error::VivariumError;

use super::CadenceState;
use super::check::{CommandCheck, CommandExecution};
use super::result::{AWAITING_RECOVERY, AWAITING_RED};

/// Alarm: a red need may have been sent but its handle is unknown.
pub const RED_NEED_UNKNOWN: &str = "cadence-command-red-need-unknown";

/// The mark every red need a command check files carries.
pub const MARK: &str = "cadence";

/// Main-state key prefix: the job whose red need is being sent.
const SENDING: &str = "cadence-red-sending";

/// Main-state key prefix: `<job> <handle>` of the red need just filed.
const FILED: &str = "cadence-red-filed";

/// Main-state key prefix: the job whose red was replied on the open need.
const REPLIED: &str = "cadence-red-replied";

/// Seconds a `need send` or `mail reply` may take.
const SEND_TIMEOUT: u64 = 60;

/// Settle every staged command result whose report can be paid now.
pub fn report(ctx: &Ctx, st: &mut State, cadence: &mut CadenceState, open: &PumpMailspace) {
    let store = match PumpStore::open(&ctx.home.join(DB_FILENAME)) {
        Ok(store) => store,
        Err(error) => {
            ctx.log(&format!("cadence command reports skipped: {error}"));
            return;
        }
    };
    let now = ctx.now();
    for (name, entry) in &mut cadence.checks {
        let staged = staged(entry);
        if let Err(error) = drop_stale_markers(&store, name, staged.map(|latest| &latest.job)) {
            ctx.log(&format!("cadence command {name} markers: {error}"));
            continue;
        }
        let Some(latest) = staged.cloned() else {
            continue;
        };
        let task = st
            .jobs
            .get(&latest.job)
            .and_then(|job| job.task.clone())
            .unwrap_or_default();
        let report = Report {
            ctx,
            store: &store,
            open,
            name,
            latest: &latest,
            task: &task,
        };
        match report.pay(entry) {
            Ok(true) => {
                if let Some(saved) = entry.latest.as_mut() {
                    saved.awaiting = None;
                }
                jobs::mark_applied(st, &latest.job, now);
            }
            Ok(false) => {}
            Err(error) => ctx.log(&format!(
                "cadence command {name} report failed; staged for the next tick: {error}"
            )),
        }
    }
}

/// The latest execution when it is terminal and still owes a report.
fn staged(entry: &CommandCheck) -> Option<&CommandExecution> {
    entry.latest.as_ref().filter(|latest| {
        latest.awaiting.is_some() && latest.ended_at.is_some() && !latest.job.is_empty()
    })
}

fn key(prefix: &str, name: &str) -> String {
    format!("{prefix}:{name}")
}

/// Drop the markers of a job the check no longer stages: its result was
/// applied and saved, so they can no longer prevent a duplicate.
fn drop_stale_markers(
    store: &PumpStore,
    name: &str,
    staged: Option<&String>,
) -> Result<(), VivariumError> {
    for prefix in [SENDING, FILED, REPLIED] {
        let state_key = key(prefix, name);
        let Some(value) = store.main_state(&state_key)? else {
            continue;
        };
        let job = value.split_whitespace().next().unwrap_or_default();
        if staged.is_none_or(|staged| staged != job) {
            store.clear_main_state(&state_key)?;
        }
    }
    Ok(())
}

/// One staged result and what paying it needs.
struct Report<'a> {
    ctx: &'a Ctx,
    store: &'a PumpStore,
    open: &'a PumpMailspace,
    name: &'a str,
    latest: &'a CommandExecution,
    task: &'a str,
}

impl Report<'_> {
    /// Pay the staged result. `Ok(true)` means its job may be applied;
    /// `Ok(false)` means it stays staged behind a raised alarm.
    fn pay(&self, entry: &mut CommandCheck) -> Result<bool, VivariumError> {
        match self.latest.awaiting.as_deref() {
            Some(AWAITING_RED) => self.pay_red(entry),
            Some(AWAITING_RECOVERY) => self.pay_recovery(entry),
            other => Err(VivariumError::Message(format!(
                "unknown staged report {other:?}"
            ))),
        }
    }

    /// The port writing as the role: the author of the red need it filed.
    fn port(&self) -> MailspacePort {
        MailspacePort {
            mailspace: self.open.clone(),
            pump_identity: self.role().to_string(),
        }
    }

    fn role(&self) -> &str {
        if self.latest.role.is_empty() {
            self.name
        } else {
            &self.latest.role
        }
    }

    /// The red need this check opened, while it is still open. A need closed
    /// or withdrawn by anyone is no open need.
    fn open_red_need(&self, entry: &CommandCheck) -> Result<Option<String>, VivariumError> {
        let Some(need) = entry.red_need.clone() else {
            return Ok(None);
        };
        if self.port().need_state(&need)? == NeedStatus::Open {
            return Ok(Some(need));
        }
        self.ctx.log(&format!(
            "cadence command {} red need {need} was closed elsewhere",
            self.name
        ));
        Ok(None)
    }

    fn pay_red(&self, entry: &mut CommandCheck) -> Result<bool, VivariumError> {
        if let Some(need) = self.open_red_need(entry)? {
            self.reply_red(&need)?;
            return Ok(true);
        }
        entry.red_need = None;
        let Some(need) = self.file_need()? else {
            return Ok(false);
        };
        self.ctx.log(&format!(
            "cadence command {} red at {} filed need {need}",
            self.name,
            self.commit()
        ));
        entry.red_need = Some(need);
        Ok(true)
    }

    fn pay_recovery(&self, entry: &mut CommandCheck) -> Result<bool, VivariumError> {
        let commit = self.commit();
        if let Some(need) = self.open_red_need(entry)? {
            let note = format!("{} is green again at {commit}", self.role());
            self.port().close_need(&need, &note)?;
            self.ctx.log(&format!(
                "cadence command {} green at {commit} closed need {need}",
                self.name
            ));
        }
        entry.red_need = None;
        entry
            .last_green_commit
            .clone_from(&self.latest.tested_commit);
        Ok(true)
    }

    fn commit(&self) -> &str {
        self.latest.tested_commit.as_deref().unwrap_or("unknown")
    }

    fn short_commit(&self) -> String {
        self.commit().chars().take(10).collect()
    }

    fn target_label(&self) -> &str {
        if self.latest.target.is_empty() {
            &self.latest.main
        } else {
            &self.latest.target
        }
    }

    fn subject(&self) -> String {
        format!(
            "{}: {} red on {} at {}",
            self.role(),
            self.latest.command,
            self.target_label(),
            self.short_commit()
        )
    }

    /// The facts a reader needs, with the task whose thread holds the run's
    /// receipt and log.
    fn facts(&self) -> String {
        let exit = self
            .latest
            .exit
            .map_or_else(|| String::from("none"), |exit| exit.to_string());
        format!(
            "role: {}\ncommand: {}\ntarget: {}\nmain: {}\ntested: {}\nexit: {exit}\nattempt: {}\ntask: {}\n\nRead the run's receipt and log with `vivi mail thread {}`.\n",
            self.role(),
            self.latest.command,
            self.target_label(),
            self.latest.main,
            self.commit(),
            self.latest.attempt,
            self.task,
            self.task,
        )
    }

    fn body_file(&self, stage: &str, body: &str) -> Result<PathBuf, VivariumError> {
        let dir = self.ctx.home.join("cadence");
        fs::create_dir_all(&dir)
            .map_err(|e| VivariumError::Message(format!("cannot create {}: {e}", dir.display())))?;
        let safe: String = format!("{}-{}-{stage}", self.name, self.latest.job)
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let path = dir.join(format!("{safe}.txt"));
        fs::write(&path, body)
            .map_err(|e| VivariumError::Message(format!("cannot write {}: {e}", path.display())))?;
        Ok(path)
    }

    /// Reply once on the open red need with this newer red.
    fn reply_red(&self, need: &str) -> Result<(), VivariumError> {
        let replied = key(REPLIED, self.name);
        if self.store.main_state(&replied)?.as_deref() == Some(self.latest.job.as_str()) {
            return Ok(());
        }
        let body = format!(
            "{} is still red at {}.\n\n{}",
            self.role(),
            self.commit(),
            self.facts()
        );
        let path = self.body_file("reply", &body)?;
        let args = format!(
            "mail reply {} --from {} --to {} --body-file {}",
            shell_quote(need),
            shell_quote(self.role()),
            shell_quote(&self.ctx.cfg.reply_to()),
            shell_quote(&path.display().to_string()),
        );
        let out = self.ctx.vivi(&args, SEND_TIMEOUT);
        if out.code != 0 {
            let last = out.text.lines().last().unwrap_or("").trim().to_string();
            return Err(VivariumError::Message(format!(
                "reply on red need {need} exited {}: {last}",
                out.code
            )));
        }
        self.store.set_main_state(&replied, &self.latest.job)
    }

    /// File the red need and return its handle. `None` means a send may have
    /// landed with an unknown handle: the alarm is raised and nothing is sent
    /// again for this result.
    fn file_need(&self) -> Result<Option<String>, VivariumError> {
        let job = self.latest.job.as_str();
        if let Some(filed) = self.store.main_state(&key(FILED, self.name))? {
            let mut parts = filed.split_whitespace();
            if parts.next() == Some(job)
                && let Some(handle) = parts.next()
            {
                return Ok(Some(handle.to_string()));
            }
        }
        if self.store.main_state(&key(SENDING, self.name))?.as_deref() == Some(job) {
            return self.resume_unknown();
        }
        let path = self.body_file("red", &self.facts())?;
        let mut args = format!(
            "need send --from {} --to {} --subject {} --body-file {} --mark {MARK}",
            shell_quote(self.role()),
            shell_quote(&self.ctx.cfg.reply_to()),
            shell_quote(&self.subject()),
            shell_quote(&path.display().to_string()),
        );
        if !self.latest.target.is_empty() {
            args.push_str(&format!(" --target {}", shell_quote(&self.latest.target)));
        }
        args.push_str(&format!(
            " --done-when {}",
            shell_quote(&format!(
                "the {} check passes on {} ({})",
                self.role(),
                self.latest.main,
                self.latest.command
            ))
        ));
        self.store.set_main_state(&key(SENDING, self.name), job)?;
        let out = self.ctx.vivi(&args, SEND_TIMEOUT);
        self.after_send(out.code, &out.text)
    }

    fn after_send(&self, code: i32, text: &str) -> Result<Option<String>, VivariumError> {
        let last = text.lines().last().unwrap_or("").trim();
        if code == 124 {
            self.alarm(&format!("the send timed out: {last}"));
            return Ok(None);
        }
        if let Some(need) = parse_need_handle(text) {
            if code != 0 {
                self.ctx.log(&format!(
                    "cadence red need send exited {code} but created {need}; keeping it"
                ));
            }
            let filed = format!("{} {need}", self.latest.job);
            self.store.set_main_state(&key(FILED, self.name), &filed)?;
            return Ok(Some(need));
        }
        if code != 0 {
            self.store.clear_main_state(&key(SENDING, self.name))?;
            return Err(VivariumError::Message(format!(
                "red need send failed: expected a created need handle; actual: exit {code}: {last}"
            )));
        }
        self.alarm(&format!("unparsed send output: {last}"));
        Ok(None)
    }

    /// An earlier send for this result left no handle. When the role's sent
    /// needs hold exactly one open need with this subject, it is the one the
    /// send filed; otherwise the alarm stands and nothing is sent again.
    fn resume_unknown(&self) -> Result<Option<String>, VivariumError> {
        let subject = self.subject();
        let found = {
            let mailspace = self.open.lock();
            sent_with_subject(&mailspace, self.role(), &subject)?
        };
        let mut open = Vec::new();
        for handle in found {
            if self.port().need_state(&handle)? == NeedStatus::Open {
                open.push(handle);
            }
        }
        if let [need] = open.as_slice() {
            let filed = format!("{} {need}", self.latest.job);
            self.store.set_main_state(&key(FILED, self.name), &filed)?;
            return Ok(Some(need.clone()));
        }
        self.alarm("an earlier send left no handle");
        Ok(None)
    }

    fn alarm(&self, detail: &str) {
        raise_once(
            self.ctx,
            RED_NEED_UNKNOWN,
            self.name,
            &format!(
                "the red need for {} at {} may have been sent but its handle is unknown ({detail}); no second need is filed and the role stays staged until the need is found",
                self.name,
                self.commit()
            ),
        );
    }
}

/// The handles of the needs `role` sent with exactly `subject`.
fn sent_with_subject(
    mailspace: &Mailspace,
    role: &str,
    subject: &str,
) -> Result<Vec<String>, VivariumError> {
    Ok(mailspace
        .list_kind_all(Some(role), "sent", "need")?
        .into_iter()
        .filter(|need| need.subject == subject)
        .map(|need| need.handle)
        .collect())
}
