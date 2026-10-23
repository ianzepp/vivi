//! Tests for slice dispatch: the hand job, retry, scope re-dispatch,
//! landed-slice deviations and the machine-first classification.

use std::collections::BTreeMap;
use std::fs;

use tempfile::{TempDir, tempdir};

use super::{
    AttemptResult, DEVIATIONS_CAP, DEVIATIONS_CUT, DEVIATIONS_MARKER, EXAMPLE_BLOCKED,
    EXAMPLE_BUILT, MAX_ATTEMPTS, Retried, SliceMail, classify, classify_corrected, contract,
    dispatch, redispatch_scope, retry,
};
use crate::error::VivariumError;
use crate::pump::config::PumpConfig;
use crate::pump::envelope::parse;
use crate::pump::envelope::roles::Hand;
use crate::pump::flow::fake_mail::FakeMail;
use crate::pump::flow::index::{RunRow, StepRow, StepState};
use crate::pump::flow::mail::MailWriter;
use crate::pump::flow::scope::{BlockedScope, ScopeState};
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::jobs::{JobOutcome, JobSpec, JobStatus, JobStore, PoolJob};
use crate::pump::store::{DB_FILENAME, PumpStore};
use crate::routing::RoutedRole;

type R = Result<(), VivariumError>;

const NEED: &str = "n1";

#[derive(Default)]
struct Jobs {
    jobs: BTreeMap<String, PoolJob>,
}

impl JobStore for Jobs {
    fn jobs(&self) -> &BTreeMap<String, PoolJob> {
        &self.jobs
    }

    fn jobs_mut(&mut self) -> &mut BTreeMap<String, PoolJob> {
        &mut self.jobs
    }
}

struct Rig {
    _dir: TempDir,
    ctx: Ctx,
    host: std::sync::Arc<FakeHost>,
    store: PumpStore,
    mail: FakeMail,
    jobs: Jobs,
}

fn rig() -> Result<Rig, VivariumError> {
    let dir = tempdir().map_err(|e| VivariumError::Other(e.to_string()))?;
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    let mut routing = (*ctx.routing).clone();
    routing.roles.push(RoutedRole {
        name: String::from("hand"),
        kind: Some(String::from("hand")),
        status: String::from("active"),
        pool: Some(String::from("test-planner-pool")),
        read_only: None,
        model: None,
        models: Vec::new(),
    });
    let ctx = ctx.with_routing(routing);
    let store = PumpStore::open(&dir.path().join(DB_FILENAME))?;
    store.insert_run(&RunRow {
        need: NEED.into(),
        run: 1,
        flow: "build".into(),
        flow_version: 1,
        kind: "build".into(),
        entry: "plan".into(),
        state: "live".into(),
    })?;
    Ok(Rig {
        _dir: dir,
        ctx,
        host,
        store,
        mail: FakeMail::new("pump"),
        jobs: Jobs::default(),
    })
}

impl Rig {
    /// A pending slice with `scope`; its task body carries the target line.
    fn slice(&mut self, id: &str, scope: &[&str]) -> Result<StepRow, VivariumError> {
        let task = self.mail.send_step(
            "pump",
            "pump",
            &format!("slice {id}"),
            &format!("pump-step: {NEED} r1 slice:{id}\ntarget: vivi\n\ntitle {id}\n\nbrief {id}"),
            &[],
        )?;
        let row = StepRow {
            run: 1,
            node: format!("slice:{id}"),
            task: task.clone(),
            state: StepState::Pending,
            verdict: None,
        };
        self.store.insert_step(NEED, &row)?;
        let scope: Vec<String> = scope.iter().map(|path| (*path).to_string()).collect();
        self.store.insert_slice_scope(&task, 1, &scope)?;
        Ok(row)
    }

    fn dispatch(&mut self, slice: &StepRow, attempt: u32) -> Result<String, VivariumError> {
        dispatch(
            &self.ctx,
            &mut self.jobs,
            &self.store,
            &self.mail,
            NEED,
            slice,
            attempt,
        )
    }

    fn job(&self, id: &str) -> &PoolJob {
        &self.jobs.jobs[id]
    }

    /// Close `slice` merged and post its deviations.
    fn land(&mut self, slice: &StepRow, deviations: &str) -> R {
        self.mail
            .reply(&slice.task, &format!("{DEVIATIONS_MARKER} {deviations}"))?;
        self.mail.close_task(&slice.task, "merged", None, None)
    }

    fn exceptions(&self) -> String {
        fs::read_to_string(self.ctx.home.join("exceptions.txt")).unwrap_or_default()
    }
}

// ---- dispatch -------------------------------------------------------------

#[test]
fn dispatch_queues_a_hand_job_on_the_slice_task() -> R {
    let mut rig = rig()?;
    let slice = rig.slice("a", &["src/a.rs", "src/b/"])?;

    let id = rig.dispatch(&slice, 1)?;

    assert_eq!(id, format!("slice-{}-a1", slice.task));
    let job = rig.job(&id);
    assert_eq!(job.kind, "hand");
    assert_eq!(job.role, "hand");
    assert_eq!(job.task.as_deref(), Some(slice.task.as_str()));
    assert_eq!(job.target.as_deref(), Some("vivi"));
    let branch = job.branch.clone().unwrap();
    assert_eq!(branch, format!("vivi/unit/{}-a1", slice.task));
    assert_eq!(job.publish_ref, Some(format!("refs/heads/{branch}")));
    assert!(!job.read_only);
    let row = rig.store.step_job(&id)?.unwrap();
    assert_eq!((row.task.as_str(), row.attempt), (slice.task.as_str(), 1));
    assert_eq!(row.node, "slice:a");
    assert_eq!(
        rig.store.step_by_task(&slice.task)?.unwrap().state,
        StepState::Running
    );
    Ok(())
}

#[test]
fn the_body_holds_rules_scope_contract_then_the_task_body() -> R {
    let mut rig = rig()?;
    let slice = rig.slice("a", &["src/a.rs", "src/b/"])?;
    let id = rig.dispatch(&slice, 1)?;
    let body = &rig.job(&id).body;

    let at = |needle: &str| {
        body.find(needle)
            .unwrap_or_else(|| panic!("missing {needle:?}"))
    };
    let rules = at("You are a Hand");
    let scope = at("WRITE SCOPE:\n- src/a.rs\n- src/b/");
    let contract = at("HAND ENVELOPE CONTRACT");
    let task = at("pump-step: n1 r1 slice:a");
    assert!(
        rules < scope && scope < contract && contract < task,
        "{body}"
    );
    assert!(body.contains("answer = \"blocked-scope\""));
    assert!(body.contains("brief a"));
    assert!(!body.contains("Deviations of landed slices"));
    Ok(())
}

#[test]
fn dispatching_the_same_attempt_again_queues_nothing_new() -> R {
    let mut rig = rig()?;
    let slice = rig.slice("a", &["src/a.rs"])?;
    let first = rig.dispatch(&slice, 1)?;
    let second = rig.dispatch(&slice, 1)?;
    assert_eq!(first, second);
    assert_eq!(rig.jobs.jobs.len(), 1);
    assert_eq!(rig.store.attempts_of(&slice.task)?, 1);
    Ok(())
}

#[test]
fn a_slice_task_without_a_target_line_is_refused() -> R {
    let mut rig = rig()?;
    let task = rig.mail.send_step(
        "pump",
        "pump",
        "s",
        "pump-step: n1 r1 slice:x\n\nno target",
        &[],
    )?;
    let row = StepRow {
        run: 1,
        node: "slice:x".into(),
        task: task.clone(),
        state: StepState::Pending,
        verdict: None,
    };
    rig.store.insert_step(NEED, &row)?;
    rig.store
        .insert_slice_scope(&task, 1, &["src/x.rs".to_string()])?;
    assert!(rig.dispatch(&row, 1).is_err());
    assert!(rig.jobs.jobs.is_empty());
    Ok(())
}

#[test]
fn the_contract_examples_parse_as_hand_envelopes() {
    for example in [EXAMPLE_BUILT, EXAMPLE_BLOCKED] {
        parse::<Hand>(example).unwrap_or_else(|e| panic!("{e}: {example}"));
    }
    let text = contract();
    assert!(text.contains(EXAMPLE_BUILT) && text.contains(EXAMPLE_BLOCKED));
}

// ---- retry ----------------------------------------------------------------

fn failed_job(id: &str) -> PoolJob {
    let mut job = PoolJob::from(JobSpec::new(id, "hand", "hand", "s", "b"));
    job.status = JobStatus::Finished;
    job.outcome = Some(JobOutcome {
        state: "finished".into(),
        detail: String::new(),
        receipt: receipt("pushed vivi/unit/x-a1 abcdef0123", 1, "I gave up."),
        at: 0,
    });
    job
}

#[test]
fn retry_runs_the_same_task_with_a_new_job_id_and_exhausts_at_the_cap() -> R {
    let mut rig = rig()?;
    let slice = rig.slice("a", &["src/a.rs"])?;
    let first = rig.dispatch(&slice, 1)?;

    let step = rig.store.step_by_task(&slice.task)?.unwrap();
    let got = retry(
        &rig.ctx,
        &mut rig.jobs,
        &rig.store,
        &rig.mail,
        NEED,
        &step,
        &failed_job(&first),
        1,
    )?;
    let Retried::Dispatched(second) = got else {
        panic!("expected a second attempt, got {got:?}");
    };
    assert_ne!(first, second);
    assert_eq!(second, format!("slice-{}-a2", slice.task));
    assert_eq!(rig.job(&second).task, rig.job(&first).task);
    assert_eq!(rig.store.step_job(&second)?.unwrap().attempt, 2);
    assert_eq!(rig.store.attempts_of(&slice.task)?, 2);
    // The failed attempt's record went to the slice task.
    let mails = rig.host.commands();
    let record = format!("mail reply {} ", slice.task);
    assert!(mails.iter().any(|cmd| cmd.contains(&record)), "{mails:?}");

    assert_eq!(MAX_ATTEMPTS, 2);
    let got = retry(
        &rig.ctx,
        &mut rig.jobs,
        &rig.store,
        &rig.mail,
        NEED,
        &step,
        &failed_job(&second),
        2,
    )?;
    assert_eq!(got, Retried::Exhausted);
    assert_eq!(rig.jobs.jobs.len(), 2, "no third attempt");
    let row = rig.store.step_by_task(&slice.task)?.unwrap();
    assert_eq!(row.state, StepState::Failed);
    assert_eq!(row.verdict.as_deref(), Some("failed"));
    Ok(())
}

// ---- scope re-dispatch ------------------------------------------------------

#[test]
fn a_scope_redispatch_keeps_the_attempt_and_carries_the_widened_scope() -> R {
    let mut rig = rig()?;
    let slice = rig.slice("a", &["src/a.rs"])?;
    let first = rig.dispatch(&slice, 1)?;
    rig.store
        .widen_slice_scope(&slice.task, &["src/extra.rs".to_string()])?;
    rig.store.set_scope_state(&slice.task, ScopeState::Wait)?;

    let again = redispatch_scope(
        &rig.ctx,
        &mut rig.jobs,
        &rig.store,
        &rig.mail,
        NEED,
        &slice,
        1,
    )?;
    let more = redispatch_scope(
        &rig.ctx,
        &mut rig.jobs,
        &rig.store,
        &rig.mail,
        NEED,
        &slice,
        1,
    )?;

    assert_eq!(again, format!("slice-{}-a1-s1", slice.task));
    assert_eq!(more, format!("slice-{}-a1-s2", slice.task));
    let job = rig.job(&again);
    assert_eq!(job.task.as_deref(), Some(slice.task.as_str()));
    assert!(
        job.body.contains("- src/a.rs\n- src/extra.rs"),
        "{}",
        job.body
    );
    assert_ne!(job.branch, rig.job(&first).branch);
    assert_eq!(rig.store.step_job(&again)?.unwrap().attempt, 1);
    assert_eq!(rig.store.scope_state(&slice.task)?, Some(ScopeState::Clear));
    Ok(())
}

// ---- landed deviations ------------------------------------------------------

#[test]
fn deviations_of_merged_siblings_come_newest_first_and_skip_unmerged_ones() -> R {
    let mut rig = rig()?;
    let a = rig.slice("a", &["src/a.rs"])?;
    let b = rig.slice("b", &["src/b.rs"])?;
    let c = rig.slice("c", &["src/c.rs"])?;
    let me = rig.slice("me", &["src/me.rs"])?;
    rig.land(&a, "the old one")?;
    rig.land(&b, "the new one")?;
    rig.mail.reply(
        &c.task,
        &format!("{DEVIATIONS_MARKER} from an unmerged slice"),
    )?;
    rig.mail
        .reply(&me.task, &format!("{DEVIATIONS_MARKER} my own"))?;

    let id = rig.dispatch(&me, 1)?;
    let body = &rig.job(&id).body;

    assert!(body.contains("Deviations of landed slices"), "{body}");
    let newer = body.find("the new one").expect("new one");
    let older = body.find("the old one").expect("old one");
    assert!(newer < older, "{body}");
    assert!(body.contains(&format!("- slice:b ({})", b.task)));
    assert!(!body.contains("unmerged slice"));
    assert!(!body.contains("my own"));
    // The deviations come before the slice task body.
    assert!(older < body.find("pump-step: n1 r1 slice:me").unwrap());
    Ok(())
}

#[test]
fn deviations_are_capped_with_a_cut_marker() -> R {
    let mut rig = rig()?;
    let big = rig.slice("big", &["src/big.rs"])?;
    let new = rig.slice("new", &["src/new.rs"])?;
    let me = rig.slice("me", &["src/me.rs"])?;
    rig.land(&big, &"x".repeat(DEVIATIONS_CAP * 2))?;
    rig.land(&new, "kept because it is newest")?;

    let id = rig.dispatch(&me, 1)?;
    let body = &rig.job(&id).body;

    assert!(body.contains("kept because it is newest"));
    assert!(body.contains(DEVIATIONS_CUT));
    assert!(!body.contains(&"x".repeat(DEVIATIONS_CAP * 2)));
    let section = body
        .split("Deviations of landed slices (newest first):\n")
        .nth(1)
        .and_then(|rest| rest.split(DEVIATIONS_CUT).next())
        .unwrap();
    assert!(section.len() <= DEVIATIONS_CAP + 1, "{}", section.len());
    Ok(())
}

// ---- classify ---------------------------------------------------------------

/// A pool task receipt: summary lines (with the runner's push word), then the
/// hand's final message.
fn receipt(push: &str, exit: i32, message: &str) -> String {
    format!(
        "pool result j1: finished\npool p  slot s1  model m\nfrom main  default\n\
         push: {push}\nagent exit {exit} in 12s, job 20s, started from 1234567890\n\n\
         --- receipt\n{message}\n\n--- stderr tail\n\n\n--- runner log tail\nlog line"
    )
}

fn hand_job(id: &str, status: JobStatus, receipt: Option<String>) -> PoolJob {
    let mut spec = JobSpec::new(id, "hand", "hand", "s", "b");
    spec.task = Some("t0001".into());
    spec.branch = Some("vivi/unit/t0001-a1".into());
    let mut job = PoolJob::from(spec);
    job.status = status;
    job.outcome = receipt.map(|receipt| JobOutcome {
        state: "finished".into(),
        detail: String::new(),
        receipt,
        at: 0,
    });
    job
}

fn classified(rig: &Rig, push: &str, exit: i32, message: &str) -> AttemptResult {
    let job = hand_job(
        "slice-t0001-a1",
        JobStatus::Finished,
        Some(receipt(push, exit, message)),
    );
    classify(&rig.ctx, &job).expect("terminal job")
}

const BUILT: &str = "+++\nanswer = \"built\"\nchange = [\"src/a.rs\"]\n+++\nDone.";
const BLOCKED: &str = "+++\nanswer = \"blocked-scope\"\nscope_paths = [\"src/x.rs\", \"src/y.rs\"]\nscope_reason = 'needs the helper'\nscope_design = true\n+++\nChanged nothing.";

#[test]
fn a_blocked_scope_envelope_carries_two_paths_and_the_design_flag() -> R {
    let rig = rig()?;
    let got = classified(&rig, "no_changes", 0, BLOCKED);
    assert_eq!(
        got,
        AttemptResult::BlockedScope(BlockedScope {
            paths: vec!["src/x.rs".into(), "src/y.rs".into()],
            reason: "needs the helper".into(),
            design: true,
        })
    );
    assert!(rig.exceptions().is_empty());
    Ok(())
}

#[test]
fn built_with_a_pushed_branch_is_built_on_the_runners_branch() -> R {
    let rig = rig()?;
    let got = classified(&rig, "pushed vivi/unit/t0001-a1 abcdef0123", 0, BUILT);
    let AttemptResult::Built { envelope, branch } = got else {
        panic!("expected built, got {got:?}");
    };
    assert_eq!(branch, "vivi/unit/t0001-a1");
    assert_eq!(envelope.change, vec!["src/a.rs".to_string()]);
    let committed = classified(&rig, "committed vivi/unit/t0001-a1 abcdef0123", 0, BUILT);
    assert!(
        matches!(committed, AttemptResult::Built { .. }),
        "{committed:?}"
    );
    assert!(rig.exceptions().is_empty());
    Ok(())
}

#[test]
fn built_with_no_changes_is_no_defect_even_when_change_lists_paths() -> R {
    let rig = rig()?;
    assert_eq!(
        classified(&rig, "no_changes", 0, BUILT),
        AttemptResult::NoDefect
    );
    assert!(rig.exceptions().is_empty());
    Ok(())
}

#[test]
fn built_with_a_nonzero_agent_rc_fails_and_raises_the_disagreement() -> R {
    let rig = rig()?;
    let got = classified(&rig, "pushed vivi/unit/t0001-a1 abcdef0123", 1, BUILT);
    assert_eq!(got, AttemptResult::Failed);
    assert!(rig.exceptions().contains("flow-envelope-disagrees"));
    assert!(rig.exceptions().contains("t0001"));
    Ok(())
}

#[test]
fn built_with_no_push_result_fails_and_raises_the_disagreement() -> R {
    let rig = rig()?;
    let job = hand_job(
        "slice-t0001-a1",
        JobStatus::Finished,
        Some(format!(
            "pool result j1: finished\nagent exit 0 in 1s\n\n--- receipt\n{BUILT}"
        )),
    );
    assert_eq!(classify(&rig.ctx, &job), Some(AttemptResult::Failed));
    assert!(rig.exceptions().contains("flow-envelope-disagrees"));
    let refused = classified(&rig, "push_refused", 0, BUILT);
    assert_eq!(refused, AttemptResult::Failed);
    Ok(())
}

#[test]
fn blocked_scope_that_pushed_commits_is_honored_but_alarmed() -> R {
    let rig = rig()?;
    let got = classified(&rig, "pushed vivi/unit/t0001-a1 abcdef0123", 0, BLOCKED);
    assert!(matches!(got, AttemptResult::BlockedScope(_)), "{got:?}");
    assert!(rig.exceptions().contains("flow-envelope-disagrees"));
    Ok(())
}

#[test]
fn a_failed_envelope_and_a_timeout_or_lost_job_are_failed_attempts() -> R {
    let rig = rig()?;
    let failed = "+++\nanswer = \"failed\"\nremaining = ['everything']\n+++\nCould not.";
    assert_eq!(
        classified(&rig, "no_changes", 1, failed),
        AttemptResult::Failed
    );
    for status in [JobStatus::Timeout, JobStatus::Lost] {
        let job = hand_job("slice-t0001-a1", status, None);
        assert_eq!(classify(&rig.ctx, &job), Some(AttemptResult::Failed));
    }
    assert!(rig.exceptions().is_empty());
    Ok(())
}

#[test]
fn a_job_that_is_not_terminal_has_no_result_yet() -> R {
    let rig = rig()?;
    let job = hand_job("slice-t0001-a1", JobStatus::Dispatched, None);
    assert_eq!(classify(&rig.ctx, &job), None);
    Ok(())
}

#[test]
fn a_broken_envelope_asks_for_a_correction_and_no_verdict_line_is_read() -> R {
    let rig = rig()?;
    let broken = "+++\nanswer = \"built\"\nproof = 'x'\n+++\nDone.";
    let got = classified(&rig, "pushed vivi/unit/t0001-a1 abcdef0123", 0, broken);
    assert!(matches!(got, AttemptResult::Correct { .. }), "{got:?}");
    let old_style = classified(&rig, "no_changes", 0, "Nothing to do.\nVERDICT: no-defect");
    assert!(
        matches!(old_style, AttemptResult::Correct { .. }),
        "{old_style:?}"
    );
    // A crashed agent with a broken envelope is a plain failed attempt.
    assert_eq!(
        classified(&rig, "no_changes", 1, broken),
        AttemptResult::Failed
    );
    Ok(())
}

#[test]
fn corrections_run_out_and_the_attempt_fails_with_an_alarm() -> R {
    let rig = rig()?;
    let original = hand_job(
        "slice-t0001-a1",
        JobStatus::Finished,
        Some(receipt(
            "pushed vivi/unit/t0001-a1 abcdef0123",
            0,
            "no envelope",
        )),
    );
    let broken = receipt("read_only_unchanged 0", 0, "still no envelope");
    let second = hand_job("slice-t0001-a1-c2", JobStatus::Finished, Some(broken));
    assert_eq!(
        classify_corrected(&rig.ctx, &original, &second),
        Some(AttemptResult::Failed)
    );
    assert!(rig.exceptions().contains("flow-envelope-failed"));
    Ok(())
}

#[test]
fn a_corrected_envelope_is_judged_against_the_original_jobs_machine_facts() -> R {
    let rig = rig()?;
    let original = hand_job(
        "slice-t0001-a1",
        JobStatus::Finished,
        Some(receipt(
            "pushed vivi/unit/t0001-a1 abcdef0123",
            0,
            "no envelope",
        )),
    );
    let fixed = hand_job(
        "slice-t0001-a1-c1",
        JobStatus::Finished,
        Some(receipt("read_only_unchanged 0", 0, BUILT)),
    );
    let got = classify_corrected(&rig.ctx, &original, &fixed);
    assert!(
        matches!(&got, Some(AttemptResult::Built { branch, .. }) if branch == "vivi/unit/t0001-a1"),
        "{got:?}"
    );
    Ok(())
}

// ---- the real mailspace port ------------------------------------------------

#[test]
fn the_real_port_reads_the_task_body_and_marked_replies() {
    use crate::pump::fixture::project;
    use crate::pump::flow::mail::MailspacePort;

    let p = project();
    let port = MailspacePort {
        mailspace: p.mailspace.clone(),
        pump_identity: "mind".into(),
    };
    let task = port
        .send_step(
            "mind",
            "operator",
            "slice",
            "pump-step: n1 r1 slice:a\ntarget: vivi",
            &[],
        )
        .unwrap();
    port.reply(&task, "pump-deviations: first").unwrap();
    port.reply(&task, "pump-attempt: not a deviation").unwrap();
    port.reply(&task, "pump-deviations: second").unwrap();

    assert!(port.task_body(&task).unwrap().contains("target: vivi"));
    let found = port.marked_replies(&task, DEVIATIONS_MARKER).unwrap();
    let texts: Vec<&str> = found.iter().map(|(_, text)| text.as_str()).collect();
    assert_eq!(texts, ["pump-deviations: first", "pump-deviations: second"]);
}
