//! Project rules: shared active set, scope mark, and history.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use vivi::storage::{MessageIngestRequest, Storage};

#[test]
fn active_rules_are_visible_to_every_seat() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let root = project.path().to_str().unwrap();
    let body = project.path().join("rule-body.txt");
    std::fs::write(&body, "window text\n").unwrap();

    let unknown = vivi([
        "rule",
        "save",
        "--for",
        "nobody",
        "--subject",
        "nope",
        "--body",
        "nope",
        "--project",
        root,
    ]);
    assert!(!unknown.status.success());
    assert!(stderr(&unknown).contains("unknown"), "{}", stderr(&unknown));

    let local = save_rule(
        root,
        &["--local", "--subject", "alpha door", "--body", "stay here"],
    );
    let shared = save_rule(
        root,
        &[
            "--subject",
            "beta window",
            "--body-file",
            body.to_str().unwrap(),
        ],
    );
    assert_success(&vivi([
        "memo",
        "save",
        "--for",
        "ceo",
        "--subject",
        "desk note",
        "--body",
        "not a rule",
        "--project",
        root,
    ]));

    let listed = stdout(&vivi(["rule", "list", "--project", root]));
    let alpha = line_with(&listed, "alpha door");
    let beta = line_with(&listed, "beta window");
    assert!(listed.find("alpha door").unwrap() < listed.find("beta window").unwrap());
    assert!(
        alpha.split_whitespace().any(|word| word == "local"),
        "{alpha}"
    );
    assert!(
        beta.split_whitespace().any(|word| word == "project"),
        "{beta}"
    );
    assert!(alpha.contains(&local) && beta.contains(&shared), "{listed}");

    let ceo: Value = serde_json::from_str(&stdout(&vivi([
        "rule",
        "list",
        "--for",
        "ceo",
        "--json",
        "--project",
        root,
    ])))
    .unwrap();
    assert_eq!(ceo.as_array().unwrap().len(), 2);
    assert!(
        stdout(&vivi(["rule", "list", "--for", "cto", "--project", root])).contains("no rules")
    );

    let memos = stdout(&vivi(["memo", "list", "--for", "ceo", "--project", root]));
    assert!(memos.contains("desk note"), "{memos}");
    assert!(!memos.contains("alpha door"), "{memos}");

    let shown = stdout(&vivi(["rule", "show", &local, "--project", root]));
    assert!(shown.contains("scope: local"), "{shown}");
    assert!(shown.contains("author: ceo"), "{shown}");
    assert!(shown.contains("state: active"), "{shown}");
    assert!(shown.contains("stay here"), "{shown}");

    let boot = stdout(&vivi(["boot", "--project", root]));
    let rules_at = boot.find("rules: project rules").expect(&boot);
    let memos_at = boot.find("memos: durable role memory").expect(&boot);
    assert!(rules_at < memos_at, "{boot}");
    assert!(
        boot.contains("alpha door") && boot.contains("beta window"),
        "{boot}"
    );
    assert!(boot.contains("local") && boot.contains("project"), "{boot}");
}

#[test]
fn supersede_replaces_the_active_rule() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let root = project.path().to_str().unwrap();
    let old = save_rule(root, &["--subject", "old wording", "--body", "first text"]);

    let replaced = vivi([
        "rule",
        "supersede",
        &old,
        "--for",
        "cto",
        "--local",
        "--subject",
        "new wording",
        "--body",
        "second text",
        "--project",
        root,
    ]);
    assert_success(&replaced);
    let (previous, new) = superseded_handles(&stdout(&replaced));
    assert_eq!(previous, old);

    let active = stdout(&vivi(["rule", "list", "--project", root]));
    assert!(!active.contains(&old), "{active}");
    assert!(
        active.contains(&new) && active.contains("new wording"),
        "{active}"
    );

    let old_show = stdout(&vivi(["rule", "show", &old, "--project", root]));
    assert!(old_show.contains("state: superseded"), "{old_show}");
    assert!(
        old_show.contains(&format!("superseded by: {new}")),
        "{old_show}"
    );
    assert!(old_show.contains("scope: project"), "{old_show}");

    let new_json = show_json(root, &new);
    assert_eq!(new_json["scope"], "local");
    assert_eq!(new_json["author"], "cto");
    assert_eq!(new_json["state"], "active");
    assert!(new_json.get("successor").is_none(), "{new_json}");

    let old_json = show_json(root, &old);
    let old_id = old_json["message_id"].as_str().unwrap();
    let new_id = new_json["message_id"].as_str().unwrap();
    let notes = event_notes(project.path(), &old, "superseded");
    assert_eq!(notes, vec![new_id.to_string()]);
    let actors = event_actors(project.path(), &old, "superseded");
    assert_eq!(actors, vec![Some("cto".to_string())]);

    let bytes = message_text(project.path(), new_id);
    assert!(bytes.contains("X-Vivi-Format: 1"), "{bytes}");
    for header in ["X-Vivi-Rule-Scope", "X-Vivi-Supersedes"] {
        assert!(
            !bytes.contains(&format!("{header}:")),
            "rule fact stayed a header: {bytes}"
        );
    }
    let parsed = vivi::letter::of_message(bytes.as_bytes(), new_id).unwrap();
    assert_eq!(parsed.kind, vivi::letter::LetterKind::Rule);
    let vivi::letter::Front::Rule(front) = parsed.front else {
        panic!("not a rule letter: {bytes}")
    };
    assert_eq!(front.scope, Some(vivi::letter::plain::RuleScope::Local));
    assert_eq!(front.supersedes.as_deref(), Some(old_id));

    let again = vivi([
        "rule",
        "supersede",
        &old,
        "--for",
        "ceo",
        "--subject",
        "third wording",
        "--body",
        "nope",
        "--project",
        root,
    ]);
    assert!(!again.status.success());
    assert!(
        stderr(&again).contains(&format!("{old} is superseded by {new}")),
        "{}",
        stderr(&again)
    );
}

#[test]
fn a_legacy_rule_letter_is_history_whose_headers_are_not_read() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let root = project.path().to_str().unwrap();
    let letter = "From: ceo@local.test\r\nTo: ceo@local.test\r\nSubject: old law\r\n\
         Message-ID: <legacy-rule@local.test>\r\nX-Vivi-Kind: rule\r\n\
         X-Vivi-Rule-Scope: local\r\n\
         Date: Sun, 11 Oct 2026 12:00:00 +0000\r\n\r\nold text\r\n";
    let storage = Storage::open_mailspace(&project.path().join(".vivi")).unwrap();
    let stored = storage
        .ingest_message(
            &MessageIngestRequest {
                account: "ceo".into(),
                local_role: "rules".into(),
                read_state: false,
                starred: false,
                message_id_hint: None,
                presented_id: None,
                seed_hint: "legacy-rule".into(),
                remote: None,
            },
            letter.as_bytes(),
        )
        .unwrap();
    let handle = storage.display_handle(&stored.message_id).unwrap();

    let history = stdout(&vivi(["rule", "list", "--all", "--project", root]));
    assert!(history.contains("old law"), "{history}");
    let shown = stdout(&vivi(["rule", "show", &handle, "--project", root]));
    assert!(shown.contains("old text"), "{shown}");

    let json = show_json(root, &handle);
    assert_eq!(json["scope"], "project");
    assert_eq!(json["state"], "active");
}

#[test]
fn retire_and_absorb_leave_readable_history() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let root = project.path().to_str().unwrap();
    let retired = save_rule(root, &["--subject", "leave it", "--body", "old law"]);

    let first = vivi([
        "rule",
        "retire",
        &retired,
        "--for",
        "cto",
        "--note",
        "spent",
        "--project",
        root,
    ]);
    assert_success(&first);
    assert!(
        stdout(&first).contains(&format!("retired {retired}")),
        "{}",
        stdout(&first)
    );
    assert!(!stdout(&vivi(["rule", "list", "--project", root])).contains(&retired));

    let history = stdout(&vivi(["rule", "list", "--all", "--project", root]));
    let row = line_with(&history, "leave it");
    assert!(row.contains("retired"), "{row}");

    let second = vivi([
        "rule",
        "retire",
        &retired,
        "--for",
        "cto",
        "--project",
        root,
    ]);
    assert_success(&second);
    assert!(
        stdout(&second).contains(&format!("{retired} already retired")),
        "{}",
        stdout(&second)
    );
    assert_eq!(event_notes(project.path(), &retired, "retired").len(), 1);

    assert_success(&vivi([
        "rule",
        "absorb",
        &retired,
        "--for",
        "ceo",
        "--project",
        root,
    ]));
    let after_absorb = stdout(&vivi(["rule", "show", &retired, "--project", root]));
    assert!(after_absorb.contains("state: retired"), "{after_absorb}");
    assert_eq!(event_notes(project.path(), &retired, "absorbed").len(), 1);
    assert_success(&vivi([
        "rule",
        "absorb",
        &retired,
        "--for",
        "ceo",
        "--project",
        root,
    ]));
    assert_eq!(event_notes(project.path(), &retired, "absorbed").len(), 1);

    let sealed = save_rule(root, &["--subject", "seal me", "--body", "freeze"]);
    assert_success(&vivi([
        "rule",
        "absorb",
        &sealed,
        "--for",
        "cto",
        "--project",
        root,
    ]));
    assert!(!stdout(&vivi(["rule", "list", "--project", root])).contains("seal me"));
    let shown = stdout(&vivi(["rule", "show", &sealed, "--project", root]));
    assert!(shown.contains("state: absorbed"), "{shown}");
    let refused = vivi(["rule", "retire", &sealed, "--for", "ceo", "--project", root]);
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains("is absorbed and can no longer be changed"),
        "{}",
        stderr(&refused)
    );
}

#[test]
fn search_reads_the_body_and_a_need_is_not_a_rule() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let root = project.path().to_str().unwrap();
    let rule = save_rule(
        root,
        &[
            "--subject",
            "door policy",
            "--body",
            "the token is coral-badge",
        ],
    );
    let hits = stdout(&vivi(["rule", "search", "coral-badge", "--project", root]));
    assert!(hits.contains(&rule), "{hits}");
    let subjects = stdout(&vivi([
        "rule",
        "search",
        "coral-badge",
        "--subject",
        "--project",
        root,
    ]));
    assert!(subjects.contains("no rules"), "{subjects}");

    let need = send_need(root);
    let refused = vivi(["rule", "show", &need, "--project", root]);
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains("is not a rule"),
        "{}",
        stderr(&refused)
    );
}

#[test]
fn rules_do_not_enter_the_backlog() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let root = project.path().to_str().unwrap();
    let rule = save_rule(
        root,
        &["--subject", "rule-not-work", "--body", "stand still"],
    );
    let need = send_need(root);

    let step = stdout(&vivi(["step", "--json", "--project", root]));
    assert!(step.contains("a real need"), "{step}");
    assert!(
        !step.contains("rule-not-work") && !step.contains(&rule),
        "{step}"
    );

    let ready = stdout(&vivi(["graph", "ready", "backlog", "--project", root]));
    assert!(ready.contains(&need), "{ready}");
    assert!(
        !ready.contains(&rule) && !ready.contains("rule-not-work"),
        "{ready}"
    );
}

fn save_rule(root: &str, extra: &[&str]) -> String {
    let mut args = vec!["rule", "save", "--for", "ceo", "--project", root];
    args.extend(extra);
    let output = vivi(args);
    assert_success(&output);
    handle_after(&stdout(&output), "saved ")
}

fn send_need(root: &str) -> String {
    let output = vivi([
        "need",
        "send",
        "--project",
        root,
        "--from",
        "ceo",
        "--to",
        "cto",
        "--subject",
        "a real need",
        "--done-when",
        "it exists",
        "--body",
        "it exists",
    ]);
    assert_success(&output);
    handle_after(&stdout(&output), "created cto ")
}

fn show_json(root: &str, handle: &str) -> Value {
    let output = vivi(["rule", "show", handle, "--json", "--project", root]);
    assert_success(&output);
    serde_json::from_str(&stdout(&output)).unwrap()
}

fn superseded_handles(output: &str) -> (String, String) {
    let line = output
        .lines()
        .find(|line| line.starts_with("superseded "))
        .unwrap_or_else(|| panic!("missing supersede receipt in:\n{output}"));
    let mut parts = line.split_whitespace();
    assert_eq!(parts.next(), Some("superseded"));
    let previous = parts.next().unwrap().to_string();
    assert_eq!(parts.next(), Some("with"));
    let handle = parts.next().unwrap().to_string();
    (previous, handle)
}

fn line_with<'a>(output: &'a str, needle: &str) -> &'a str {
    output
        .lines()
        .find(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("missing {needle} in:\n{output}"))
}

fn event_notes(project: &Path, handle: &str, event_type: &str) -> Vec<String> {
    events(project, handle)
        .into_iter()
        .filter(|event| event.event_type == event_type)
        .map(|event| event.note.unwrap_or_default())
        .collect()
}

fn event_actors(project: &Path, handle: &str, event_type: &str) -> Vec<Option<String>> {
    events(project, handle)
        .into_iter()
        .filter(|event| event.event_type == event_type)
        .map(|event| event.actor_identity)
        .collect()
}

fn events(project: &Path, handle: &str) -> Vec<vivi::storage::MailspaceEvent> {
    Storage::open_mailspace(&project.join(".vivi"))
        .unwrap()
        .list_mailspace_events(handle)
        .unwrap()
}

fn message_text(project: &Path, message_id: &str) -> String {
    let bytes = Storage::open_mailspace(&project.join(".vivi"))
        .unwrap()
        .read_message(message_id)
        .unwrap();
    String::from_utf8(bytes).unwrap()
}

fn init_roster(project: &Path) {
    assert_success(&vivi([
        "mailspace",
        "init",
        "--project",
        project.to_str().unwrap(),
    ]));
    for identity in ["ceo", "cto"] {
        assert_success(&vivi([
            "mailspace",
            "identity",
            "add",
            identity,
            "--project",
            project.to_str().unwrap(),
        ]));
    }
}

fn vivi<I, S>(args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args(args)
        .output()
        .unwrap()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "status: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        stdout(output),
        stderr(output)
    );
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn handle_after(output: &str, prefix: &str) -> String {
    output
        .lines()
        .find_map(|line| line.strip_prefix(prefix))
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_else(|| panic!("missing '{prefix}' in output:\n{output}"))
        .to_string()
}
