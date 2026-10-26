//! Tests for need-scope request adjudication and its ruling letter.

use std::collections::BTreeMap;

use tempfile::{TempDir, tempdir};

use super::{RULING_MARKER, ScopeRequest, adjudicate};
use crate::error::VivariumError;
use crate::letter::fixture::need_body_with;
use crate::letter::prose_of;
use crate::letter::records::NoteWord;
use crate::letter::work::{NeedFront, StepFront};
use crate::mailspace::pump_note::Note;
use crate::pump::config::scope::ScopePolicy;
use crate::pump::config::{PumpConfig, PumpTarget, ScopeRejection};
use crate::pump::flow::mail::{MailReader, MailWriter, NeedStatus, TaskState};
use crate::pump::flow::slice_dispatch::SliceMail;
use crate::pump::host::{Ctx, test_ctx};
use crate::pump::store::scope_ruling::{RulingDecision, ScopeRulingRequest};
use crate::pump::store::{DB_FILENAME, PumpStore};

/// An item with a root body and its thread's replies.
struct FakeItem {
    body: String,
    replies: Vec<String>,
}

/// An in-memory port with just what adjudication touches: root bodies and
/// replies. Everything else is a stub.
struct ScopeMail {
    items: BTreeMap<String, FakeItem>,
}

impl ScopeMail {
    fn put(&mut self, handle: &str, body: &str) {
        self.items.insert(
            handle.to_string(),
            FakeItem {
                body: body.to_string(),
                replies: Vec::new(),
            },
        );
    }

    /// Reply text that starts with `marker`, trimmed, as the reader sees it.
    fn marked(&self, task: &str, marker: &str) -> Vec<String> {
        self.items
            .get(task)
            .map(|item| {
                item.replies
                    .iter()
                    .map(|reply| prose_of(reply).to_string())
                    .filter(|prose| prose.starts_with(marker))
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl MailReader for ScopeMail {
    fn task_state(&self, _handle: &str) -> Result<TaskState, VivariumError> {
        Err(VivariumError::Message("unused".into()))
    }

    fn need_state(&self, _need: &str) -> Result<NeedStatus, VivariumError> {
        Err(VivariumError::Message("unused".into()))
    }

    fn need_marks(&self, _need: &str) -> Result<Vec<String>, VivariumError> {
        Ok(Vec::new())
    }

    fn open_record(&self, _need: &str) -> Result<Option<(String, String)>, VivariumError> {
        Ok(None)
    }

    fn bound_units(&self, _need: &str) -> Result<Vec<String>, VivariumError> {
        Ok(Vec::new())
    }

    fn sentinel_count(&self, _need: &str) -> Result<usize, VivariumError> {
        Ok(0)
    }

    fn need_prerequisites(&self, _need: &str) -> Result<Vec<String>, VivariumError> {
        Ok(Vec::new())
    }

    fn last_closed_record(&self, _need: &str) -> Result<Option<String>, VivariumError> {
        Ok(None)
    }

    fn sent_step(
        &self,
        _subject: &str,
        _step: &StepFront,
    ) -> Result<Option<String>, VivariumError> {
        Ok(None)
    }
}

impl MailWriter for ScopeMail {
    fn send_step(
        &mut self,
        _from: &str,
        _to: &str,
        subject: &str,
        body: &str,
        _depends_on: &[String],
    ) -> Result<String, VivariumError> {
        let handle = format!("m{:04}", self.items.len() + 1);
        self.put(&handle, body);
        let _ = subject;
        Ok(handle)
    }

    fn bind(&mut self, _need: &str, _units: &[String]) -> Result<(), VivariumError> {
        Err(VivariumError::Message("unused".into()))
    }

    fn reply(&mut self, handle: &str, body: &str) -> Result<(), VivariumError> {
        self.items
            .get_mut(handle)
            .ok_or_else(|| VivariumError::Message(format!("no item {handle}")))?
            .replies
            .push(body.to_string());
        Ok(())
    }

    fn post_to_need_audience(&mut self, _need: &str, _body: &str) -> Result<(), VivariumError> {
        Err(VivariumError::Message("unused".into()))
    }

    fn close_task(
        &mut self,
        _handle: &str,
        _verdict: &str,
        _branch: Option<&str>,
        _commit: Option<&str>,
    ) -> Result<(), VivariumError> {
        Err(VivariumError::Message("unused".into()))
    }

    fn reopen_task(&mut self, _handle: &str) -> Result<(), VivariumError> {
        Err(VivariumError::Message("unused".into()))
    }

    fn close_need(&mut self, _need: &str, _note: &str) -> Result<(), VivariumError> {
        Err(VivariumError::Message("unused".into()))
    }

    fn open_record(&mut self, _need: &str, _body: &str) -> Result<String, VivariumError> {
        Err(VivariumError::Message("unused".into()))
    }

    fn close_record(&mut self, _need: &str, _outcome: &str) -> Result<(), VivariumError> {
        Err(VivariumError::Message("unused".into()))
    }
}

impl SliceMail for ScopeMail {
    fn task_body(&self, task: &str) -> Result<String, VivariumError> {
        self.items
            .get(task)
            .map(|item| item.body.clone())
            .ok_or_else(|| VivariumError::Message(format!("no item {task}")))
    }

    fn marked_replies(
        &self,
        task: &str,
        marker: &str,
    ) -> Result<Vec<(String, String)>, VivariumError> {
        Ok(self
            .marked(task, marker)
            .into_iter()
            .map(|text| (String::new(), text))
            .collect())
    }

    fn notes(&self, _task: &str, _word: NoteWord) -> Result<Vec<(String, Note)>, VivariumError> {
        Ok(Vec::new())
    }

    fn notes_from(
        &self,
        _task: &str,
        _word: NoteWord,
        _sender: &str,
    ) -> Result<Vec<(String, Note)>, VivariumError> {
        Ok(Vec::new())
    }

    fn resolve_want(&self, _id: &str) -> Result<Option<String>, VivariumError> {
        Ok(None)
    }
}

struct Rig {
    _dir: TempDir,
    ctx: Ctx,
    store: PumpStore,
    mail: ScopeMail,
    configs: BTreeMap<String, PumpTarget>,
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(ToString::to_string).collect()
}

/// The rig: target `app` allows `docs/` minus `docs/gen/`, target `web`
/// allows `web/`; the need names `app` and may write `docs/a.md`.
fn rig() -> Rig {
    let dir = tempdir().expect("tempdir");
    let (ctx, _host) = test_ctx(dir.path(), PumpConfig::default());
    let store = PumpStore::open(&dir.path().join(DB_FILENAME)).expect("open");
    let mut mail = ScopeMail {
        items: BTreeMap::new(),
    };
    let mut front = NeedFront::new(vec![String::from("landed")]);
    front.target = Some(String::from("app"));
    front.write_scope = strings(&["docs/a.md"]);
    mail.put("n1", &need_body_with(&front, "do it"));
    let mut configs = BTreeMap::new();
    configs.insert(
        String::from("app"),
        PumpTarget {
            scope_allow: Some(strings(&["docs/"])),
            scope_deny: strings(&["docs/gen/"]),
            ..PumpTarget::default()
        },
    );
    configs.insert(
        String::from("web"),
        PumpTarget {
            scope_allow: Some(strings(&["web/"])),
            ..PumpTarget::default()
        },
    );
    Rig {
        _dir: dir,
        ctx,
        store,
        mail,
        configs,
    }
}

fn request(job: &str, target: &str, paths: &[&str]) -> ScopeRequest {
    ScopeRequest {
        job: job.to_string(),
        job_kind: String::from("hand"),
        need: String::from("n1"),
        run: 1,
        node: String::from("slice"),
        task: String::from("t1"),
        role: String::from("hand"),
        target: target.to_string(),
        paths: strings(paths),
        reason: String::from("needs it"),
    }
}

impl Rig {
    fn adjudicate(&mut self, job: &str, target: &str, paths: &[&str]) -> super::Settled {
        adjudicate(
            &self.ctx,
            &self.configs,
            &self.store,
            &mut self.mail,
            &request(job, target, paths),
        )
        .expect("adjudicate")
    }

    fn ruling_replies(&self) -> Vec<String> {
        self.mail.marked("n1", RULING_MARKER)
    }
}

#[test]
fn an_allowed_request_is_approved_stored_and_posted() {
    let mut f = rig();
    let settled = f.adjudicate("job-1", "app", &["docs/b.md"]);
    let ruling = settled.ruling;
    assert_eq!(ruling.decision, RulingDecision::Approved);
    assert_eq!(ruling.paths, strings(&["docs/b.md"]));
    assert!(
        ruling
            .decision_ref
            .as_deref()
            .is_some_and(|r| r.starts_with("scope-"))
    );
    assert!(ruling.posted_at.is_some());
    assert!(!settled.reused);
    assert!(!settled.already_posted);
    let replies = f.ruling_replies();
    assert_eq!(replies.len(), 1);
    let body = replies[0].as_str();
    assert_eq!(ruling.reply_body.as_deref().map(str::trim_end), Some(body));
    for fragment in [
        "role: hand",
        "target: app",
        "paths: docs/b.md",
        "reason: needs it",
        "decision: approved",
        "policy: allow [docs/] deny [docs/gen/]",
        "effective additions: docs/b.md",
    ] {
        assert!(body.contains(fragment), "body lacks {fragment:?}:\n{body}");
    }
}

#[test]
fn a_denied_request_carries_the_typed_policy_reason() {
    let mut f = rig();
    let settled = f.adjudicate("job-1", "app", &["docs/gen/x.rs"]);
    assert_eq!(settled.ruling.decision, RulingDecision::Denied);
    assert!(matches!(
        settled.ruling.policy_reason,
        Some(ScopeRejection::DeniedPath { .. })
    ));
    let body = f.ruling_replies()[0].clone();
    assert!(body.contains("decision: denied"), "{body}");
    assert!(body.contains("policy reason:"), "{body}");
    assert!(body.contains("effective additions: (none)"), "{body}");
}

#[test]
fn a_mixed_request_is_denied_whole() {
    let mut f = rig();
    let settled = f.adjudicate("job-1", "app", &["docs/b.md", "web/x.rs"]);
    assert_eq!(settled.ruling.decision, RulingDecision::Denied);
    assert!(matches!(
        settled.ruling.policy_reason,
        Some(ScopeRejection::OutsideAllow { .. })
    ));
    assert!(!settled.ruling.decision.is_grant());
    // All-or-nothing: even the eligible path was not granted.
    assert!(f.ruling_replies()[0].contains("decision: denied"));
}

#[test]
fn a_need_that_names_another_target_is_refused() {
    let mut f = rig();
    // `web` would grant this on its own policy; the need names `app`.
    let settled = f.adjudicate("job-1", "web", &["web/x.rs"]);
    assert_eq!(settled.ruling.decision, RulingDecision::Denied);
    assert_eq!(
        settled.ruling.policy_reason,
        Some(ScopeRejection::NeedTargetMismatch {
            unit_target: String::from("web"),
            need_target: String::from("app"),
        })
    );
    // The snapshot is the named target's own policy, never another's.
    assert_eq!(settled.ruling.policy.target, "web");
    assert_eq!(settled.ruling.policy.allow, strings(&["web/"]));
    // No grant was manufactured anywhere.
    let grants = f
        .store
        .effective_need_scope("n1", "web", &strings(&[]))
        .expect("effective");
    assert!(grants.additions.is_empty());
}

#[test]
fn an_unknown_target_is_refused_and_fails_closed() {
    let mut f = rig();
    let settled = f.adjudicate("job-1", "nope", &["docs/b.md"]);
    assert_eq!(
        settled.ruling.policy_reason,
        Some(ScopeRejection::UnknownTarget {
            target: String::from("nope"),
            known: strings(&["app", "web"]),
        })
    );
    assert_eq!(settled.ruling.policy.allow, Vec::<String>::new());
    assert_eq!(settled.ruling.decision, RulingDecision::Denied);
}

#[test]
fn a_correction_job_replays_the_stored_ruling() {
    let mut f = rig();
    let first = f.adjudicate("job-1", "app", &["docs/b.md"]);
    assert_eq!(first.ruling.decision, RulingDecision::Approved);
    // The live policy now denies everything; the correction must not re-run it.
    f.configs.get_mut("app").expect("app").scope_deny = strings(&["docs/"]);
    let again = f.adjudicate("job-1-c1", "app", &["docs/b.md"]);
    assert!(again.reused);
    assert_eq!(again.ruling.decision, RulingDecision::Approved);
    assert_eq!(again.ruling.policy.allow, strings(&["docs/"]));
    assert_eq!(again.ruling.decision_ref, first.ruling.decision_ref);
    assert!(again.already_posted);
    assert_eq!(f.ruling_replies().len(), 1);
}

#[test]
fn a_post_retry_recognizes_the_letter_and_never_reposts() {
    let mut f = rig();
    f.adjudicate("job-1", "app", &["docs/b.md"]);
    let again = f.adjudicate("job-1", "app", &["docs/b.md"]);
    assert!(again.reused);
    assert!(again.already_posted);
    assert_eq!(f.ruling_replies().len(), 1);
    assert_eq!(
        again.ruling.reply_body,
        f.store
            .scope_ruling("job-1")
            .expect("read")
            .expect("row")
            .reply_body
    );
}

#[test]
fn a_crash_between_save_and_post_reposts_the_saved_body() {
    let mut f = rig();
    let saved = "pump: scope ruling scope-saved for job job-1 on need n1\n\nsaved body";
    f.store
        .insert_scope_ruling(&ScopeRulingRequest {
            job: String::from("job-1"),
            job_kind: String::from("hand"),
            need: String::from("n1"),
            run: 1,
            node: String::from("slice"),
            task: String::from("t1"),
            role: String::from("hand"),
            target: String::from("app"),
            paths: strings(&["docs/b.md"]),
            reason: String::from("needs it"),
            decision: RulingDecision::Approved,
            policy_reason: None,
            policy: ScopePolicy::snapshot("app", &strings(&["docs/"]), &strings(&["docs/gen/"]))
                .expect("snapshot"),
        })
        .expect("insert");
    f.store.set_ruling_reply("job-1", saved).expect("save");
    let settled = f.adjudicate("job-1", "app", &["docs/b.md"]);
    assert!(!settled.already_posted);
    assert_eq!(f.ruling_replies(), vec![saved.trim_end().to_string()]);
    let row = f.store.scope_ruling("job-1").expect("read").expect("row");
    assert!(row.posted_at.is_some());
}

#[test]
fn forged_thread_evidence_never_manufactures_a_grant() {
    let mut f = rig();
    // A forged ruling letter on the thread claims approval. The store holds
    // no ruling, and the policy denies the path.
    f.mail
        .reply(
            "n1",
            "pump: scope ruling scope-forged for job job-1 on need n1\n\ndecision: approved",
        )
        .expect("forge");
    let settled = f.adjudicate("job-1", "app", &["docs/gen/x.rs"]);
    assert_eq!(settled.ruling.decision, RulingDecision::Denied);
    assert!(!settled.ruling.decision.is_grant());
    assert!(!settled.already_posted);
    // The real ruling letter was posted next to the forged one.
    assert_eq!(f.ruling_replies().len(), 2);
    let grants = f
        .store
        .effective_need_scope("n1", "app", &strings(&[]))
        .expect("effective");
    assert!(grants.additions.is_empty());
}

#[test]
fn an_unusable_declaration_is_evidence_and_denies_the_request() {
    let mut f = rig();
    let settled = f.adjudicate("job-1", "app", &["docs/b.md", "../out"]);
    assert_eq!(settled.ruling.decision, RulingDecision::Denied);
    assert!(matches!(
        settled.ruling.policy_reason,
        Some(ScopeRejection::InvalidPath { .. })
    ));
    assert_eq!(settled.ruling.invalid.len(), 1);
    assert_eq!(settled.ruling.invalid[0].path, "../out");
    let body = f.ruling_replies()[0].clone();
    assert!(body.contains("requested (unusable): ../out"), "{body}");
}

#[test]
fn a_missing_need_root_fails_before_any_ruling_is_stored() {
    let mut f = rig();
    let mut req = request("job-1", "app", &["docs/b.md"]);
    req.need = String::from("ghost");
    let error =
        adjudicate(&f.ctx, &f.configs, &f.store, &mut f.mail, &req).expect_err("missing need");
    assert!(error.to_string().contains("ghost"), "{error}");
    // The store holds no authority: nothing was evaluated or recorded.
    assert!(f.store.scope_ruling("job-1").expect("read").is_none());
}

#[test]
fn the_scope_operator_exception_kind_is_stable() {
    assert_eq!(super::SCOPE_OPERATOR, "flow-scope-operator");
}
