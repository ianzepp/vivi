//! State as letters in a mailspace with no `[sync]`: the owner's close and
//! reopen are reply letters on the item, an assignee's report is only a
//! reply, and the folders, nodes, thread and frontier follow the letters.

use std::path::Path;
use std::process::{Command, Output};

const BODY: &str = "done_when: it works";

fn vivi(project: &Path, args: &[&str]) -> Output {
    let mut full: Vec<&str> = args.to_vec();
    full.extend(["--project", project.to_str().unwrap()]);
    Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args(full)
        .output()
        .unwrap()
}

fn ok(project: &Path, args: &[&str]) -> String {
    let output = vivi(project, args);
    assert!(
        output.status.success(),
        "{args:?}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn fails(project: &Path, args: &[&str]) -> String {
    let output = vivi(project, args);
    assert!(!output.status.success(), "{args:?} should fail");
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn project() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    ok(tmp.path(), &["mailspace", "init"]);
    // `mailspace init` itself ensures the operator role of kind operator.
    for (role, kind) in [("mind", "mind"), ("hand", "hand"), ("auditor", "auditor")] {
        ok(tmp.path(), &["role", "add", role, "--kind", kind]);
    }
    tmp
}

/// `(the delivered copy's handle, the sender's copy's handle)`.
fn send(p: &Path, kind: &str, subject: &str) -> (String, String) {
    send_between(p, kind, "mind", "hand", subject)
}

fn send_between(p: &Path, kind: &str, from: &str, to: &str, subject: &str) -> (String, String) {
    let out = ok(
        p,
        &[
            kind,
            "send",
            "--from",
            from,
            "--to",
            to,
            "--subject",
            subject,
            "--body",
            BODY,
        ],
    );
    let after = |prefix: &str| {
        out.lines()
            .find_map(|line| line.strip_prefix(prefix))
            .unwrap()
            .trim()
            .to_string()
    };
    (after(&format!("created {to} ")), after("sent "))
}

/// The folder role of the row `handle` names, from `show`'s JSON thread.
fn role_of(p: &Path, kind: &str, handle: &str) -> String {
    let out = ok(p, &[kind, "show", handle, "--json"]);
    let thread: serde_json::Value = serde_json::from_str(&out).unwrap();
    thread
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["handle"] == handle)
        .unwrap()["role"]
        .as_str()
        .unwrap()
        .to_string()
}

fn state_line(p: &Path, kind: &str, handle: &str) -> Option<String> {
    ok(p, &[kind, "show", handle])
        .lines()
        .find_map(|line| line.strip_prefix("State: "))
        .map(|rest| rest.split(' ').next().unwrap().to_string())
}

fn thread_len(p: &Path, kind: &str, handle: &str) -> usize {
    let out = ok(p, &[kind, "show", handle, "--json"]);
    let thread: serde_json::Value = serde_json::from_str(&out).unwrap();
    thread.as_array().unwrap().len()
}

fn counts(p: &Path) -> String {
    ok(p, &["graph", "ready", "backlog"])
        .lines()
        .find(|line| line.trim_start().starts_with("counts"))
        .unwrap()
        .trim()
        .to_string()
}

fn audit_is_clean(p: &Path) {
    let audit = ok(p, &["graph", "audit"]);
    assert!(
        audit.contains("missing=0 state=0 kind=0 orphan=0 foreign=0 edge=0 event=0"),
        "{audit}"
    );
    let repair = ok(p, &["graph", "audit", "--repair"]);
    assert!(!repair.contains("[repaired]"), "{repair}");
}

#[test]
fn the_owners_flow_with_no_sync_is_letters_all_the_way() {
    let tmp = project();
    let p = tmp.path();
    let (need, need_sent) = send(p, "need", "N the need");
    let (task, task_sent) = send(p, "task", "T the unit");
    ok(p, &["need", "bind", &need, &task]);
    assert!(counts(p).contains("ready=2"), "{}", counts(p));
    assert_eq!(role_of(p, "task", &task), "tasks");
    assert_eq!(state_line(p, "task", &task), None);

    // The assignee reports with a reply. Nothing of the owner's moves.
    ok(
        p,
        &[
            "mail",
            "reply",
            &task,
            "--from",
            "hand",
            "--body",
            "T is done: it works",
        ],
    );
    assert_eq!(role_of(p, "task", &task), "tasks");
    assert_eq!(
        state_line(p, "task", &task),
        None,
        "a report is not an event"
    );
    assert!(counts(p).contains("ready=2"), "{}", counts(p));
    assert_eq!(thread_len(p, "task", &task), 2);

    // The owner closes T: an event letter, the unit's row, the node, and
    // through the join the need.
    ok(p, &["task", "done", &task_sent, "--for", "mind"]);
    assert_eq!(role_of(p, "task", &task), "done");
    assert_eq!(state_line(p, "task", &task).as_deref(), Some("done"));
    assert_eq!(thread_len(p, "task", &task), 3);
    assert_eq!(role_of(p, "need", &need), "done", "the join closed N");
    assert_eq!(state_line(p, "need", &need).as_deref(), Some("done"));
    assert!(counts(p).contains("ready=0"), "{}", counts(p));
    let step = ok(p, &["step"]);
    assert!(step.contains("no dispatches"), "{step}");
    audit_is_clean(p);

    // An auditor asks for a reopen; the owner reopens N with a note.
    ok(
        p,
        &[
            "mail",
            "reply",
            &need,
            "--from",
            "auditor",
            "--body",
            "please reopen N",
        ],
    );
    assert_eq!(role_of(p, "need", &need), "done", "a request moves nothing");
    let refused = fails(p, &["need", "reopen", &need, "--for", "hand"]);
    assert!(refused.contains("only the owner can reopen"), "{refused}");
    ok(
        p,
        &[
            "need",
            "reopen",
            &need_sent,
            "--for",
            "mind",
            "--note",
            "reopening on the audit",
        ],
    );
    assert_eq!(role_of(p, "need", &need), "needs");
    assert_eq!(state_line(p, "need", &need).as_deref(), Some("reopened"));
    assert!(counts(p).contains("ready=1"), "{}", counts(p));
    let ready = ok(p, &["graph", "ready", "backlog"]);
    assert!(ready.contains(&need), "{ready}");

    // A second unit, reported and closed; the join closes N again.
    let (task2, task2_sent) = send(p, "task", "T2 the second unit");
    ok(p, &["need", "bind", &need, &task2]);
    assert!(ready_has(p, &task2));
    ok(
        p,
        &[
            "mail",
            "reply",
            &task2,
            "--from",
            "hand",
            "--body",
            "T2 is done",
        ],
    );
    ok(p, &["task", "done", &task2_sent, "--for", "mind"]);
    assert_eq!(role_of(p, "task", &task2), "done");
    assert_eq!(role_of(p, "need", &need), "done");
    assert_eq!(state_line(p, "need", &need).as_deref(), Some("done"));
    assert!(counts(p).contains("ready=0"), "{}", counts(p));

    // The need's thread holds the item, the audit's request, the binds and
    // the events in order.
    let out = ok(p, &["need", "show", &need, "--json"]);
    let thread: serde_json::Value = serde_json::from_str(&out).unwrap();
    let bodies: Vec<String> = thread
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["body"].as_str().unwrap().trim().to_string())
        .collect();
    assert_eq!(bodies.len(), 7, "{bodies:?}");
    for wanted in ["please reopen N", "reopening on the audit"] {
        assert!(bodies.iter().any(|b| b == wanted), "{wanted}: {bodies:?}");
    }
    assert_eq!(bodies.iter().filter(|b| *b == "bound").count(), 2);
    assert_eq!(bodies.iter().filter(|b| *b == "done").count(), 2);

    // Converged: an audit and a repair change nothing.
    audit_is_clean(p);
    let again = ok(p, &["need", "show", &need, "--json"]);
    assert_eq!(again, out);
}

#[test]
fn a_need_close_records_and_shows_its_integration_location() {
    let tmp = project();
    let p = tmp.path();
    let (need, _) = send(p, "need", "N with branch provenance");

    ok(
        p,
        &[
            "need",
            "done",
            &need,
            "--for",
            "mind",
            "--branch",
            "mind/review",
            "--commit",
            "deadbeef",
        ],
    );

    let shown = ok(p, &["need", "show", &need]);
    assert!(shown.contains("Done on: mind/review deadbeef"), "{shown}");
    let json: serde_json::Value =
        serde_json::from_str(&ok(p, &["need", "show", &need, "--json"])).unwrap();
    let item = json
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["handle"] == need)
        .unwrap();
    assert_eq!(item["close_branch"], "mind/review");
    assert_eq!(item["close_commit"], "deadbeef");

    ok(p, &["need", "reopen", &need, "--for", "mind"]);
    let reopened = ok(p, &["need", "show", &need]);
    assert!(!reopened.contains("Done on:"), "{reopened}");
    ok(p, &["need", "done", &need, "--for", "mind"]);
    let closed_without_location = ok(p, &["need", "show", &need]);
    assert!(
        !closed_without_location.contains("Done on:"),
        "{closed_without_location}"
    );
}

fn ready_has(p: &Path, handle: &str) -> bool {
    ok(p, &["graph", "ready", "backlog"]).contains(handle)
}

#[test]
fn an_assignees_done_moves_its_own_copy_and_writes_no_event() {
    let tmp = project();
    let p = tmp.path();
    let (task, _) = send(p, "task", "T the unit");

    ok(
        p,
        &["task", "done", &task, "--for", "hand", "--note", "finished"],
    );

    assert_eq!(role_of(p, "task", &task), "done");
    assert_eq!(state_line(p, "task", &task), None, "a report, not an event");
    assert_eq!(thread_len(p, "task", &task), 2, "the item and the note");
    audit_is_clean(p);
}

#[test]
fn a_self_addressed_need_closes_and_reopens_by_its_own_letters() {
    let tmp = project();
    let p = tmp.path();
    let out = ok(
        p,
        &[
            "need",
            "send",
            "--from",
            "mind",
            "--to",
            "mind",
            "--subject",
            "mine",
            "--body",
            BODY,
        ],
    );
    let need = out
        .lines()
        .find_map(|l| l.strip_prefix("created mind "))
        .unwrap()
        .trim()
        .to_string();

    ok(
        p,
        &[
            "need",
            "done",
            &need,
            "--for",
            "mind",
            "--note",
            "finished it",
        ],
    );
    assert_eq!(role_of(p, "need", &need), "done");
    assert_eq!(state_line(p, "need", &need).as_deref(), Some("done"));
    let status = ok(p, &["mailspace", "status"]);
    assert!(status.contains("total unread mail: 0"), "{status}");

    ok(p, &["need", "reopen", &need, "--for", "mind"]);
    assert_eq!(role_of(p, "need", &need), "needs");
    assert_eq!(state_line(p, "need", &need).as_deref(), Some("reopened"));
    audit_is_clean(p);
}

/// The bodies of an item's thread, in order.
fn bodies(p: &Path, kind: &str, handle: &str) -> Vec<String> {
    let out = ok(p, &[kind, "show", handle, "--json"]);
    let thread: serde_json::Value = serde_json::from_str(&out).unwrap();
    thread
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["body"].as_str().unwrap().trim().to_string())
        .collect()
}

#[test]
fn a_mind_closes_an_operators_need_it_was_handed_with_no_sync() {
    let tmp = project();
    let p = tmp.path();
    let (need, need_sent) = send_between(p, "need", "operator", "mind", "N from the operator");
    assert_eq!(state_line(p, "need", &need), None);

    // The mind holds the delivered copy and is not the author: its close is
    // still the event, because a mind closes work for everyone.
    let out = ok(p, &["need", "done", &need, "--for", "mind"]);
    assert!(out.starts_with(&format!("done {need}; event ")), "{out}");
    assert!(!out.contains("reported"), "{out}");

    assert_eq!(role_of(p, "need", &need), "done");
    assert_eq!(state_line(p, "need", &need).as_deref(), Some("done"));
    assert_eq!(state_line(p, "need", &need_sent).as_deref(), Some("done"));
    assert_eq!(thread_len(p, "need", &need), 2, "the item and its event");
    let thread = bodies(p, "need", &need);
    assert_eq!(
        thread.iter().filter(|b| *b == "done").count(),
        1,
        "{thread:?}"
    );
    assert!(counts(p).contains("ready=0"), "{}", counts(p));
    audit_is_clean(p);

    // And reopens it the same way.
    let out = ok(p, &["need", "reopen", &need, "--for", "mind"]);
    assert!(
        out.starts_with(&format!("reopened {need}; event ")),
        "{out}"
    );
    assert_eq!(role_of(p, "need", &need), "needs");
    assert_eq!(state_line(p, "need", &need).as_deref(), Some("reopened"));
    assert_eq!(thread_len(p, "need", &need), 3);
    audit_is_clean(p);
}

#[test]
fn an_operator_reopens_and_closes_a_need_it_neither_authored_nor_holds() {
    let tmp = project();
    let p = tmp.path();
    // The mind's need to the hand: the operator is neither author nor holder.
    let (need, need_sent) = send_between(p, "need", "mind", "hand", "N past the operator");
    assert_eq!(state_line(p, "need", &need), None);

    // The operator's close is the event, exactly as a mind's is: the
    // folders, the state lines, the node and the audit all follow.
    let out = ok(p, &["need", "done", &need, "--for", "operator"]);
    assert!(out.starts_with(&format!("done {need}; event ")), "{out}");
    assert!(!out.contains("reported"), "{out}");

    assert_eq!(role_of(p, "need", &need), "done");
    assert_eq!(state_line(p, "need", &need).as_deref(), Some("done"));
    assert_eq!(state_line(p, "need", &need_sent).as_deref(), Some("done"));
    assert_eq!(thread_len(p, "need", &need), 2, "the item and its event");
    assert!(counts(p).contains("ready=0"), "{}", counts(p));
    audit_is_clean(p);

    // And reopens it the same way.
    let out = ok(p, &["need", "reopen", &need, "--for", "operator"]);
    assert!(
        out.starts_with(&format!("reopened {need}; event ")),
        "{out}"
    );
    assert_eq!(role_of(p, "need", &need), "needs");
    assert_eq!(state_line(p, "need", &need).as_deref(), Some("reopened"));
    assert_eq!(thread_len(p, "need", &need), 3);
    assert!(counts(p).contains("ready=1"), "{}", counts(p));
    audit_is_clean(p);

    // And closes it again.
    let out = ok(p, &["need", "done", &need, "--for", "operator"]);
    assert!(out.starts_with(&format!("done {need}; event ")), "{out}");
    assert_eq!(role_of(p, "need", &need), "done");
    assert_eq!(state_line(p, "need", &need).as_deref(), Some("done"));
    assert_eq!(thread_len(p, "need", &need), 4);
    assert!(counts(p).contains("ready=0"), "{}", counts(p));
    audit_is_clean(p);
}

#[test]
fn a_hand_is_still_refused_and_the_refusal_names_operator_kind_beside_mind_kind() {
    let tmp = project();
    let p = tmp.path();
    let (_, need_sent) = send_between(p, "need", "mind", "hand", "N the need");

    // The hand names the author's sent copy it does not hold: refused, and
    // the refusal names operator-kind beside mind-kind.
    let error = fails(p, &["need", "done", &need_sent, "--for", "hand"]);
    assert!(error.contains("held by identity 'mind'"), "{error}");
    assert!(error.contains("--for mind"), "{error}");
    assert!(
        error.contains("for its author 'mind', a mind-kind role or an operator-kind role"),
        "{error}"
    );
    assert!(error.contains("'hand' is neither"), "{error}");
}

#[test]
fn a_hands_close_reports_and_the_minds_close_of_the_same_handle_is_the_event() {
    let tmp = project();
    let p = tmp.path();
    let (task, task_sent) = send(p, "task", "T the unit");
    assert!(counts(p).contains("ready=1"), "{}", counts(p));

    // The hand reports on its delivered copy: no event, no owner state. The
    // copy moves, and the backlog node follows the folder as it always has.
    let out = ok(
        p,
        &["task", "done", &task, "--for", "hand", "--note", "finished"],
    );
    assert!(out.starts_with(&format!("done {task}; reported")), "{out}");
    assert_eq!(state_line(p, "task", &task), None);
    assert_eq!(thread_len(p, "task", &task), 2, "the item and the note");

    // The mind closes with the very same (delivered) handle: the event.
    let out = ok(
        p,
        &["task", "done", &task, "--for", "mind", "--note", "accepted"],
    );
    assert!(out.starts_with(&format!("done {task}; event ")), "{out}");
    assert_eq!(state_line(p, "task", &task).as_deref(), Some("done"));
    assert_eq!(state_line(p, "task", &task_sent).as_deref(), Some("done"));
    assert_eq!(thread_len(p, "task", &task), 3);
    assert!(counts(p).contains("ready=0"), "{}", counts(p));
    audit_is_clean(p);

    // Closing again is not a second event.
    let again = fails(p, &["task", "done", &task, "--for", "mind"]);
    assert!(again.contains("already closed"), "{again}");
    assert_eq!(thread_len(p, "task", &task), 3);

    // The mind reopens through the delivered handle too.
    let out = ok(p, &["task", "reopen", &task, "--for", "mind"]);
    assert!(
        out.starts_with(&format!("reopened {task}; event ")),
        "{out}"
    );
    assert_eq!(role_of(p, "task", &task), "tasks");
    assert_eq!(state_line(p, "task", &task).as_deref(), Some("reopened"));
    assert!(counts(p).contains("ready=1"), "{}", counts(p));
    audit_is_clean(p);
}

#[test]
fn a_mind_closes_a_task_it_did_not_hold_by_the_hands_handle_and_by_its_own() {
    let tmp = project();
    let p = tmp.path();
    // The operator's task to the hand: the mind is neither author nor holder.
    let (task, task_sent) = send_between(p, "task", "operator", "hand", "T routed past the mind");

    let out = ok(p, &["task", "done", &task, "--for", "mind"]);
    assert!(out.contains("; event "), "{out}");
    assert_eq!(state_line(p, "task", &task).as_deref(), Some("done"));
    assert_eq!(role_of(p, "task", &task), "done");
    assert_eq!(state_line(p, "task", &task_sent).as_deref(), Some("done"));
    audit_is_clean(p);

    // The mind may reopen what the operator's own mailspace closed.
    ok(p, &["task", "reopen", &task_sent, "--for", "mind"]);
    assert_eq!(role_of(p, "task", &task), "tasks");
    assert_eq!(state_line(p, "task", &task).as_deref(), Some("reopened"));
}

#[test]
fn an_auditors_close_is_never_an_event_on_any_handle() {
    let tmp = project();
    let p = tmp.path();
    let (task, task_sent) = send(p, "task", "T the unit");
    let (need, need_sent) = send_between(p, "need", "operator", "mind", "N the need");

    // The auditor holds neither copy: the refusal names the report command
    // and who may close.
    let error = fails(p, &["task", "done", &task, "--for", "auditor"]);
    assert!(error.contains("held by identity 'hand'"), "{error}");
    assert!(error.contains("--for hand"), "{error}");
    assert!(error.contains("vivi task done"), "{error}");
    assert!(
        error.contains("mind-kind role or an operator-kind role"),
        "{error}"
    );
    let error = fails(p, &["need", "done", &need_sent, "--for", "auditor"]);
    assert!(error.contains("held by identity 'operator'"), "{error}");
    let error = fails(p, &["task", "done", &task_sent, "--for", "auditor"]);
    assert!(error.contains("held by identity 'mind'"), "{error}");

    // Work handed to the auditor itself: its close is a report, whatever
    // the handle. Only that copy moves; no event, no owner state.
    let (own_task, _) = send_between(p, "task", "mind", "auditor", "T for the auditor");
    let (own_need, _) = send_between(p, "need", "operator", "auditor", "N for the auditor");
    for (kind, handle) in [("task", &own_task), ("need", &own_need)] {
        let out = ok(p, &[kind, "done", handle, "--for", "auditor"]);
        assert!(
            out.starts_with(&format!("done {handle}; reported")),
            "{out}"
        );
        assert_eq!(role_of(p, kind, handle), "done");
        assert_eq!(state_line(p, kind, handle), None, "{kind}");
        assert_eq!(thread_len(p, kind, handle), 1, "{kind}: no event letter");
    }
    assert_eq!(state_line(p, "task", &task), None);
    assert_eq!(state_line(p, "need", &need), None);
    assert_eq!(role_of(p, "task", &task), "tasks");
    assert_eq!(role_of(p, "need", &need), "needs");
    audit_is_clean(p);
}

#[test]
fn the_minds_close_with_the_sent_handle_still_works() {
    let tmp = project();
    let p = tmp.path();
    let (task, task_sent) = send(p, "task", "T the unit");

    let out = ok(p, &["task", "done", &task_sent, "--for", "mind"]);
    assert!(
        out.starts_with(&format!("done {task_sent}; event ")),
        "{out}"
    );
    assert_eq!(role_of(p, "task", &task), "done");
    assert_eq!(state_line(p, "task", &task).as_deref(), Some("done"));
    audit_is_clean(p);
}
