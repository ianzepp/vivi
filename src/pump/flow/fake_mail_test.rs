//! An in-memory fake of the mailspace port for flow tests.
//!
//! It implements `MailWriter` and `MailReader` over plain maps and records
//! every call in order, so an engine test can assert "successors sent and
//! bound before the predecessor closed". A need follows the backlog join:
//! it is done when it has bound units and every one is done, or when it is
//! closed directly.

#![allow(dead_code)]

use std::collections::BTreeMap;

use crate::error::VivariumError;
use crate::pump::flow::mail::{
    MailReader, MailWriter, NeedStatus, OpenRecords, TaskState, TaskStatus,
};

/// One recorded port call, in call order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Call {
    SendStep {
        handle: String,
        to: String,
        subject: String,
        depends_on: Vec<String>,
    },
    Bind {
        need: String,
        units: Vec<String>,
    },
    Reply {
        handle: String,
        body: String,
    },
    PostToNeed {
        need: String,
        body: String,
    },
    CloseTask {
        handle: String,
        verdict: String,
    },
    ReopenTask {
        handle: String,
    },
    CloseNeed {
        need: String,
        note: String,
    },
    OpenRecord {
        need: String,
    },
    CloseRecord {
        need: String,
        outcome: String,
    },
}

/// A task as the fake stores it.
#[derive(Debug, Clone)]
pub(crate) struct FakeTask {
    pub from: String,
    pub to: String,
    pub subject: String,
    pub body: String,
    pub depends_on: Vec<String>,
    pub replies: Vec<String>,
    pub state: TaskState,
    pub branch: Option<String>,
    pub commit: Option<String>,
}

/// A need's closed state: who closed it, and whether a join did.
#[derive(Debug, Clone)]
struct NeedClose {
    by: String,
    joined: bool,
}

/// A pump record on a need.
#[derive(Debug, Clone)]
struct FakeRecord {
    handle: String,
    body: String,
    outcome: Option<String>,
    opened: u32,
}

/// The in-memory mailspace port.
#[derive(Debug, Default)]
pub(crate) struct FakeMail {
    pub identity: String,
    pub calls: Vec<Call>,
    pub tasks: BTreeMap<String, FakeTask>,
    bound: BTreeMap<String, Vec<String>>,
    /// Replies posted by someone other than the pump: task -> (sender, body).
    pub foreign: BTreeMap<String, Vec<(String, String)>>,
    marks: BTreeMap<String, Vec<String>>,
    closed_needs: BTreeMap<String, NeedClose>,
    close_notes: BTreeMap<String, String>,
    records: BTreeMap<String, FakeRecord>,
    next: u32,
    opened_count: u32,
    /// Fail the write that would be recorded as call `n` (0-based), once.
    pub fail_at: Option<usize>,
}

impl FakeMail {
    pub fn new(identity: &str) -> Self {
        Self {
            identity: identity.to_string(),
            ..Self::default()
        }
    }

    /// Set the marks a need carries.
    pub fn set_marks(&mut self, need: &str, marks: &[&str]) {
        self.marks.insert(
            need.to_string(),
            marks.iter().map(|m| (*m).to_string()).collect(),
        );
    }

    /// Someone other than the pump identity replies on `task`.
    pub fn foreign_reply(&mut self, task: &str, who: &str, body: &str) {
        self.foreign
            .entry(task.to_string())
            .or_default()
            .push((who.to_string(), body.to_string()));
    }

    /// Someone other than the pump closes a task (verdict `done`).
    pub fn foreign_close(&mut self, handle: &str, who: &str) {
        self.close_inner(handle, who, "done", None, None);
    }

    /// Someone closes a need directly.
    pub fn close_need_by(&mut self, need: &str, who: &str) {
        self.closed_needs.insert(
            need.to_string(),
            NeedClose {
                by: who.to_string(),
                joined: false,
            },
        );
    }

    /// Who closed a need, if it is closed.
    pub fn need_closer(&self, need: &str) -> Option<&str> {
        self.closed_needs.get(need).map(|c| c.by.as_str())
    }

    /// The note a need was closed with by the pump, if any.
    pub fn need_close_note(&self, need: &str) -> Option<&str> {
        self.close_notes.get(need).map(String::as_str)
    }

    /// The outcome the pump record on a need was closed with, if any.
    pub fn record_outcome(&self, need: &str) -> Option<&str> {
        self.records.get(need)?.outcome.as_deref()
    }

    /// Position of the first call matching `pred`, for order assertions.
    pub fn position(&self, pred: impl Fn(&Call) -> bool) -> Option<usize> {
        self.calls.iter().position(pred)
    }

    /// The needs with an open pump record, each with its open order as a
    /// zero-padded 8-digit string, oldest first.
    pub fn open_record_needs(&self) -> Vec<(String, String)> {
        let mut open: Vec<(&String, u32)> = self
            .records
            .iter()
            .filter(|(_, r)| r.outcome.is_none())
            .map(|(need, r)| (need, r.opened))
            .collect();
        open.sort_by_key(|(_, opened)| *opened);
        open.into_iter()
            .map(|(need, opened)| (need.clone(), format!("{opened:08}")))
            .collect()
    }

    /// Consume an injected failure when it is due at the next write.
    fn injected(&mut self) -> Result<(), VivariumError> {
        if self.fail_at == Some(self.calls.len()) {
            self.fail_at = None;
            return Err(VivariumError::Message("fake mail: injected failure".into()));
        }
        Ok(())
    }

    fn close_inner(
        &mut self,
        handle: &str,
        who: &str,
        verdict: &str,
        branch: Option<&str>,
        commit: Option<&str>,
    ) {
        if let Some(task) = self.tasks.get_mut(handle) {
            task.state = TaskState {
                status: TaskStatus::Done,
                closer: Some(who.to_string()),
                verdict: Some(verdict.to_string()),
            };
            task.branch = branch.map(str::to_string);
            task.commit = commit.map(str::to_string);
        }
        self.join_needs(handle, who);
    }

    /// Complete every open need whose bound units are all done.
    fn join_needs(&mut self, handle: &str, who: &str) {
        let needs: Vec<String> = self
            .bound
            .iter()
            .filter(|(_, units)| units.iter().any(|u| u == handle))
            .map(|(need, _)| need.clone())
            .collect();
        for need in needs {
            let all_done = self.bound[&need].iter().all(|u| {
                self.tasks
                    .get(u)
                    .is_some_and(|t| t.state.status == TaskStatus::Done)
            });
            if all_done && !self.closed_needs.contains_key(&need) {
                self.closed_needs.insert(
                    need,
                    NeedClose {
                        by: who.to_string(),
                        joined: true,
                    },
                );
            }
        }
    }

    fn require_task(&self, handle: &str) -> Result<&FakeTask, VivariumError> {
        self.tasks
            .get(handle)
            .ok_or_else(|| VivariumError::Message(format!("fake mail: no task {handle}")))
    }
}

impl MailWriter for FakeMail {
    fn send_step(
        &mut self,
        from: &str,
        to: &str,
        subject: &str,
        body: &str,
        depends_on: &[String],
    ) -> Result<String, VivariumError> {
        self.injected()?;
        self.next += 1;
        let handle = format!("t{:04}", self.next);
        self.tasks.insert(
            handle.clone(),
            FakeTask {
                from: from.to_string(),
                to: to.to_string(),
                subject: subject.to_string(),
                body: body.to_string(),
                depends_on: depends_on.to_vec(),
                replies: Vec::new(),
                state: TaskState {
                    status: TaskStatus::Open,
                    closer: None,
                    verdict: None,
                },
                branch: None,
                commit: None,
            },
        );
        self.calls.push(Call::SendStep {
            handle: handle.clone(),
            to: to.to_string(),
            subject: subject.to_string(),
            depends_on: depends_on.to_vec(),
        });
        Ok(handle)
    }

    fn bind(&mut self, need: &str, units: &[String]) -> Result<(), VivariumError> {
        self.injected()?;
        for unit in units {
            self.require_task(unit)?;
        }
        let bound = self.bound.entry(need.to_string()).or_default();
        for unit in units {
            if !bound.contains(unit) {
                bound.push(unit.clone());
            }
        }
        self.calls.push(Call::Bind {
            need: need.to_string(),
            units: units.to_vec(),
        });
        Ok(())
    }

    fn reply(&mut self, handle: &str, body: &str) -> Result<(), VivariumError> {
        self.injected()?;
        self.require_task(handle)?;
        if let Some(task) = self.tasks.get_mut(handle) {
            task.replies.push(body.to_string());
        }
        self.calls.push(Call::Reply {
            handle: handle.to_string(),
            body: body.to_string(),
        });
        Ok(())
    }

    fn post_to_need_audience(&mut self, need: &str, body: &str) -> Result<(), VivariumError> {
        self.injected()?;
        self.calls.push(Call::PostToNeed {
            need: need.to_string(),
            body: body.to_string(),
        });
        Ok(())
    }

    fn close_task(
        &mut self,
        handle: &str,
        verdict: &str,
        branch: Option<&str>,
        commit: Option<&str>,
    ) -> Result<(), VivariumError> {
        self.injected()?;
        self.require_task(handle)?;
        self.calls.push(Call::CloseTask {
            handle: handle.to_string(),
            verdict: verdict.to_string(),
        });
        let identity = self.identity.clone();
        self.close_inner(handle, &identity, verdict, branch, commit);
        Ok(())
    }

    fn reopen_task(&mut self, handle: &str) -> Result<(), VivariumError> {
        self.injected()?;
        self.require_task(handle)?;
        self.calls.push(Call::ReopenTask {
            handle: handle.to_string(),
        });
        if let Some(task) = self.tasks.get_mut(handle) {
            task.state = TaskState {
                status: TaskStatus::Open,
                closer: None,
                verdict: None,
            };
        }
        // A join-closed need is open again once one of its units is.
        let joined: Vec<String> = self
            .bound
            .iter()
            .filter(|(need, units)| {
                units.iter().any(|u| u == handle)
                    && self.closed_needs.get(*need).is_some_and(|c| c.joined)
            })
            .map(|(need, _)| need.clone())
            .collect();
        for need in joined {
            self.closed_needs.remove(&need);
        }
        Ok(())
    }

    fn close_need(&mut self, need: &str, note: &str) -> Result<(), VivariumError> {
        self.injected()?;
        self.calls.push(Call::CloseNeed {
            need: need.to_string(),
            note: note.to_string(),
        });
        self.closed_needs.insert(
            need.to_string(),
            NeedClose {
                by: self.identity.clone(),
                joined: false,
            },
        );
        self.close_notes.insert(need.to_string(), note.to_string());
        Ok(())
    }

    fn open_record(&mut self, need: &str, body: &str) -> Result<String, VivariumError> {
        self.injected()?;
        self.next += 1;
        let handle = format!("r{:04}", self.next);
        self.opened_count += 1;
        self.records.insert(
            need.to_string(),
            FakeRecord {
                handle: handle.clone(),
                body: body.to_string(),
                outcome: None,
                opened: self.opened_count,
            },
        );
        self.calls.push(Call::OpenRecord {
            need: need.to_string(),
        });
        Ok(handle)
    }

    fn close_record(&mut self, need: &str, outcome: &str) -> Result<(), VivariumError> {
        self.injected()?;
        let record = self
            .records
            .get_mut(need)
            .ok_or_else(|| VivariumError::Message(format!("fake mail: no record on {need}")))?;
        record.outcome = Some(outcome.to_string());
        self.calls.push(Call::CloseRecord {
            need: need.to_string(),
            outcome: outcome.to_string(),
        });
        Ok(())
    }
}

impl MailReader for FakeMail {
    fn task_state(&self, handle: &str) -> Result<TaskState, VivariumError> {
        Ok(self.require_task(handle)?.state.clone())
    }

    fn need_state(&self, need: &str) -> Result<NeedStatus, VivariumError> {
        Ok(if self.closed_needs.contains_key(need) {
            NeedStatus::Done
        } else {
            NeedStatus::Open
        })
    }

    fn need_marks(&self, need: &str) -> Result<Vec<String>, VivariumError> {
        Ok(self.marks.get(need).cloned().unwrap_or_default())
    }

    fn open_record(&self, need: &str) -> Result<Option<(String, String)>, VivariumError> {
        Ok(self
            .records
            .get(need)
            .filter(|r| r.outcome.is_none())
            .map(|r| (r.handle.clone(), r.body.clone())))
    }

    fn bound_units(&self, need: &str) -> Result<Vec<String>, VivariumError> {
        Ok(self.bound.get(need).cloned().unwrap_or_default())
    }

    fn sentinel_count(&self, need: &str) -> Result<usize, VivariumError> {
        let marker = crate::pump::flow::def::SENTINEL_MARKER;
        Ok(self
            .bound
            .get(need)
            .map(|units| {
                units
                    .iter()
                    .filter(|u| {
                        self.tasks
                            .get(*u)
                            .is_some_and(|t| t.body.starts_with(marker))
                    })
                    .count()
            })
            .unwrap_or(0))
    }

    fn sent_step(&self, subject: &str, first_line: &str) -> Result<Option<String>, VivariumError> {
        Ok(self
            .tasks
            .iter()
            .filter(|(_, task)| {
                task.from == self.identity
                    && task.subject == subject
                    && task.body.lines().next().map(str::trim) == Some(first_line.trim())
                    && task.state.status == TaskStatus::Open
            })
            .map(|(handle, _)| handle.clone())
            .max())
    }
}

impl OpenRecords for FakeMail {
    fn open_record_needs(&self) -> Result<Vec<(String, String)>, VivariumError> {
        Ok(FakeMail::open_record_needs(self))
    }
}

type R = Result<(), VivariumError>;

fn step(mail: &mut FakeMail, body: &str) -> Result<String, VivariumError> {
    mail.send_step("pump", "hand", "subject", body, &[])
}

#[test]
fn need_done_only_when_every_bound_unit_done() -> R {
    let mut mail = FakeMail::new("pump");
    let a = step(&mut mail, "a")?;
    let b = step(&mut mail, "b")?;
    mail.bind("n1", &[a.clone(), b.clone()])?;
    assert_eq!(mail.need_state("n1")?, NeedStatus::Open);
    mail.foreign_close(&a, "mind");
    assert_eq!(mail.need_state("n1")?, NeedStatus::Open);
    mail.foreign_close(&b, "mind");
    assert_eq!(mail.need_state("n1")?, NeedStatus::Done);
    assert_eq!(mail.need_closer("n1"), Some("mind"));
    Ok(())
}

#[test]
fn need_with_no_bound_units_stays_open() -> R {
    let mut mail = FakeMail::new("pump");
    step(&mut mail, "a")?;
    assert_eq!(mail.need_state("n1")?, NeedStatus::Open);
    mail.bind("n1", &[])?;
    assert_eq!(mail.need_state("n1")?, NeedStatus::Open);
    Ok(())
}

#[test]
fn bind_is_idempotent_and_reopen_restores_a_joined_need() -> R {
    let mut mail = FakeMail::new("pump");
    let a = step(&mut mail, "a")?;
    mail.bind("n1", &[a.clone()])?;
    mail.bind("n1", &[a.clone()])?;
    assert_eq!(mail.bound_units("n1")?, vec![a.clone()]);
    mail.close_task(&a, "withdrawn", None, None)?;
    assert_eq!(mail.need_state("n1")?, NeedStatus::Done);
    mail.reopen_task(&a)?;
    assert_eq!(mail.need_state("n1")?, NeedStatus::Open);
    assert_eq!(mail.task_state(&a)?.status, TaskStatus::Open);
    Ok(())
}

#[test]
fn direct_need_close_is_not_undone_by_reopening_a_unit() -> R {
    let mut mail = FakeMail::new("pump");
    let a = step(&mut mail, "a")?;
    mail.bind("n1", &[a.clone()])?;
    mail.close_need_by("n1", "mind");
    mail.reopen_task(&a)?;
    assert_eq!(mail.need_state("n1")?, NeedStatus::Done);
    Ok(())
}

#[test]
fn calls_are_recorded_in_order() -> R {
    let mut mail = FakeMail::new("pump");
    let a = step(&mut mail, "a")?;
    let b = mail.send_step("pump", "hand", "next", "b", &[a.clone()])?;
    mail.bind("n1", &[a.clone(), b.clone()])?;
    mail.close_task(&a, "done", Some("br"), Some("abc"))?;
    let bound = mail.position(|c| matches!(c, Call::Bind { .. }));
    let closed = mail.position(|c| matches!(c, Call::CloseTask { handle, .. } if *handle == a));
    assert!(bound < closed);
    assert_eq!(mail.tasks[&b].depends_on, vec![a.clone()]);
    assert_eq!(mail.tasks[&a].branch.as_deref(), Some("br"));
    let state = mail.task_state(&a)?;
    assert_eq!(state.closer.as_deref(), Some("pump"));
    assert_eq!(state.verdict.as_deref(), Some("done"));
    Ok(())
}

#[test]
fn sentinels_marks_and_records() -> R {
    let mut mail = FakeMail::new("pump");
    let s1 = step(&mut mail, "pump-run-sentinel: n1 r1")?;
    let s2 = step(&mut mail, "pump-run-sentinel: n1 r2")?;
    let plain = step(&mut mail, "pump-step: n1 r2 plan")?;
    mail.bind("n1", &[s1, s2, plain])?;
    assert_eq!(mail.sentinel_count("n1")?, 2);
    mail.set_marks("n1", &["kind-fix"]);
    assert_eq!(mail.need_marks("n1")?, vec!["kind-fix".to_string()]);
    assert!(MailReader::open_record(&mail, "n1")?.is_none());
    let handle = MailWriter::open_record(&mut mail, "n1", "kind: fix")?;
    assert_eq!(
        MailReader::open_record(&mail, "n1")?,
        Some((handle, "kind: fix".to_string()))
    );
    mail.close_record("n1", "landed")?;
    assert!(MailReader::open_record(&mail, "n1")?.is_none());
    assert_eq!(mail.record_outcome("n1"), Some("landed"));
    Ok(())
}

#[test]
fn fail_at_fails_one_write_and_records_nothing() -> R {
    let mut mail = FakeMail::new("pump");
    step(&mut mail, "a")?;
    mail.fail_at = Some(1);
    assert!(step(&mut mail, "b").is_err());
    assert_eq!(mail.calls.len(), 1);
    assert_eq!(mail.tasks.len(), 1);
    let c = step(&mut mail, "c")?;
    assert_eq!(c, "t0002");
    assert_eq!(mail.calls.len(), 2);
    assert_eq!(mail.fail_at, None);
    Ok(())
}

#[test]
fn open_record_needs_lists_open_records_oldest_first() -> R {
    let mut mail = FakeMail::new("pump");
    assert!(OpenRecords::open_record_needs(&mail)?.is_empty());
    MailWriter::open_record(&mut mail, "n2", "x")?;
    MailWriter::open_record(&mut mail, "n1", "y")?;
    assert_eq!(
        OpenRecords::open_record_needs(&mail)?,
        vec![
            ("n2".to_string(), "00000001".to_string()),
            ("n1".to_string(), "00000002".to_string())
        ]
    );
    mail.close_record("n2", "landed")?;
    assert_eq!(
        mail.open_record_needs(),
        vec![("n1".to_string(), "00000002".to_string())]
    );
    Ok(())
}

#[test]
fn sent_step_finds_the_newest_open_task_with_that_subject_and_first_line() -> R {
    let mut mail = FakeMail::new("pump");
    let first = mail.send_step("pump", "hand", "s", "line one\nmore", &[])?;
    let second = mail.send_step("pump", "hand", "s", "line one\nother", &[])?;
    mail.send_step("pump", "hand", "other subject", "line one", &[])?;
    mail.send_step("someone", "hand", "s", "line one", &[])?;
    assert_eq!(mail.sent_step("s", "line one")?, Some(second.clone()));
    mail.close_task(&second, "done", None, None)?;
    assert_eq!(mail.sent_step("s", "line one")?, Some(first));
    assert_eq!(mail.sent_step("s", "line two")?, None);
    Ok(())
}
