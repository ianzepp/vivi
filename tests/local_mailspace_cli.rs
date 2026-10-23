use std::io::Write;
use std::process::{Command, Output, Stdio};

use serde_json::Value;
use vivi::pump::cadence::{CadenceState, RoleRun, set_state};
use vivi::pump::state::State as PumpState;
use vivi::pump::store::{DB_FILENAME, PumpStore};
use vivi::storage::Storage;

#[test]
#[allow(clippy::too_many_lines)]
fn local_mail_send_creates_readable_inbox_and_sent_copy() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());

    let output = vivi([
        "mail",
        "send",
        "--project",
        project.path().to_str().unwrap(),
        "--from",
        "ceo",
        "--to",
        "cto",
        "--subject",
        "review: local delivery",
        "--body",
        "Please review the API shape.",
    ]);
    assert_success(&output);
    let send_stdout = stdout(&output);
    let delivered = handle_after(&send_stdout, "delivered cto");
    let sent = handle_after(&send_stdout, "sent");

    let list = vivi([
        "mail",
        "list",
        "--project",
        project.path().to_str().unwrap(),
        "--for",
        "cto",
    ]);
    assert_success(&list);
    let list_stdout = stdout(&list);
    assert!(list_stdout.contains(&delivered), "{list_stdout}");
    assert!(list_stdout.contains("ceo@"), "{list_stdout}");
    assert!(
        list_stdout.contains("review: local delivery"),
        "{list_stdout}"
    );
    // human list includes message date between handle and from
    assert!(
        list_stdout.contains('T') && list_stdout.contains('+') || list_stdout.contains('Z'),
        "expected timestamp in mail list: {list_stdout}"
    );

    let list_json = vivi([
        "mail",
        "list",
        "--project",
        project.path().to_str().unwrap(),
        "--for",
        "cto",
        "--json",
    ]);
    assert_success(&list_json);
    let list_value: Value = serde_json::from_str(&stdout(&list_json)).unwrap();
    let items = list_value.as_array().expect("mail list json array");
    assert_eq!(items.len(), 1, "{list_value}");
    assert_eq!(items[0]["handle"], delivered);
    assert_eq!(items[0]["subject"], "review: local delivery");
    assert!(items[0]["from"].as_str().unwrap().contains("ceo@"));
    assert!(
        !items[0]["date"].as_str().unwrap_or("").is_empty(),
        "{list_value}"
    );
    assert_eq!(items[0]["role"], "inbox");

    let show = vivi([
        "mail",
        "show",
        "--project",
        project.path().to_str().unwrap(),
        &delivered,
    ]);
    assert_success(&show);
    let show_stdout = stdout(&show);
    assert!(
        show_stdout.contains("review: local delivery"),
        "{show_stdout}"
    );
    assert!(
        show_stdout.contains("Please review the API shape."),
        "{show_stdout}"
    );

    let show_json = vivi([
        "mail",
        "show",
        "--project",
        project.path().to_str().unwrap(),
        "--json",
        &delivered,
    ]);
    assert_success(&show_json);
    let show_json_stdout = stdout(&show_json);
    assert!(show_json_stdout.contains(&delivered), "{show_json_stdout}");
    assert!(
        show_json_stdout.contains("review: local delivery"),
        "{show_json_stdout}"
    );

    let sent_list = vivi([
        "mail",
        "list",
        "--project",
        project.path().to_str().unwrap(),
        "--for",
        "ceo",
        "--folder",
        "sent",
    ]);
    assert_success(&sent_list);
    let sent_list_stdout = stdout(&sent_list);
    assert!(sent_list_stdout.contains(&sent), "{sent_list_stdout}");
    assert!(
        sent_list_stdout.contains("review: local delivery"),
        "{sent_list_stdout}"
    );

    let storage = Storage::open_mailspace(&project.path().join(".vivi")).unwrap();
    let inbox_raw = String::from_utf8(storage.read_message(&delivered).unwrap()).unwrap();
    assert!(inbox_raw.contains("Subject: review: local delivery"));
    assert!(inbox_raw.contains("Please review the API shape."));

    let sent_raw = String::from_utf8(storage.read_message(&sent).unwrap()).unwrap();
    assert!(sent_raw.contains("Subject: review: local delivery"));
    assert!(sent_raw.contains("Please review the API shape."));
}

#[test]
fn mail_list_from_to_filters_headers_and_allows_omitting_for() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    assert_success(&vivi([
        "mailspace",
        "identity",
        "add",
        "hand",
        "--project",
        project.path().to_str().unwrap(),
    ]));
    let project_s = project.path().to_str().unwrap();

    let ceo_to_cto = send_mail(project_s, "ceo", "cto", "ceo to cto", "first");
    let ceo_to_hand = send_mail(project_s, "ceo", "hand", "ceo to hand", "second");
    let cto_to_hand = send_mail(project_s, "cto", "hand", "cto to hand", "third");

    let from_ceo = list_mail_json(project_s, &["--from", "ceo"]);
    assert_eq!(
        handles(&from_ceo),
        sorted(&[ceo_to_cto.as_str(), ceo_to_hand.as_str()])
    );

    let to_cto = list_mail_json(project_s, &["--to", "cto"]);
    assert_eq!(handles(&to_cto), sorted(&[&ceo_to_cto]));

    let corridor = list_mail_json(project_s, &["--from", "ceo", "--to", "hand"]);
    assert_eq!(handles(&corridor), sorted(&[&ceo_to_hand]));

    let mailbox = list_mail_json(project_s, &["--for", "cto", "--from", "ceo"]);
    assert_eq!(handles(&mailbox), sorted(&[&ceo_to_cto]));

    let empty = list_mail_json(project_s, &["--for", "cto", "--from", "hand"]);
    assert!(handles(&empty).is_empty(), "{empty}");

    let sent = list_mail_json(project_s, &["--from", "ceo", "--folder", "sent"]);
    assert_eq!(handles(&sent).len(), 2, "{sent}");

    let other = list_mail_json(project_s, &["--from", "cto"]);
    assert_eq!(handles(&other), sorted(&[&cto_to_hand]));
}

#[test]
fn task_list_from_and_status_all() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let project_s = project.path().to_str().unwrap();
    let open = send_work(project.path(), "task", "cto", "open task", "do it");
    let done = send_work(project.path(), "task", "cto", "done task", "finish it");
    assert_success(&vivi([
        "task",
        "done",
        "--project",
        project_s,
        "--for",
        "cto",
        &done,
    ]));

    let from_ceo = vivi([
        "task",
        "list",
        "--project",
        project_s,
        "--from",
        "ceo",
        "--status",
        "open",
        "--json",
    ]);
    assert_success(&from_ceo);
    let from_items: Value = serde_json::from_str(&stdout(&from_ceo)).unwrap();
    assert_eq!(handles(&from_items), sorted(&[&open]));

    let all = vivi([
        "task",
        "list",
        "--project",
        project_s,
        "--for",
        "cto",
        "--status",
        "all",
        "--json",
    ]);
    assert_success(&all);
    let all_items: Value = serde_json::from_str(&stdout(&all)).unwrap();
    assert_eq!(handles(&all_items), sorted(&[&open, &done]));
}

#[test]
#[allow(clippy::too_many_lines)]
fn task_send_show_done_and_reopen_reads_expected_task() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let task_file = project.path().join("task.md");
    std::fs::write(&task_file, "Implement and test local delivery.").unwrap();

    let output = vivi([
        "task",
        "send",
        "--project",
        project.path().to_str().unwrap(),
        "--from",
        "ceo",
        "--to",
        "cto",
        "--subject",
        "Implement local delivery",
        "--body",
        &format!("@{}", task_file.display()),
    ]);
    assert_success(&output);
    let send_stdout = stdout(&output);
    let handle = handle_after(&send_stdout, "created cto");

    let open_list = vivi([
        "task",
        "list",
        "--project",
        project.path().to_str().unwrap(),
        "--for",
        "cto",
    ]);
    assert_success(&open_list);
    let open_stdout = stdout(&open_list);
    assert!(open_stdout.contains(&handle), "{open_stdout}");
    assert!(
        open_stdout.contains("Implement local delivery"),
        "{open_stdout}"
    );

    let open_json = vivi([
        "task",
        "list",
        "--project",
        project.path().to_str().unwrap(),
        "--for",
        "cto",
        "--json",
    ]);
    assert_success(&open_json);
    let open_items: Value = serde_json::from_str(&stdout(&open_json)).unwrap();
    let open_item = &open_items.as_array().unwrap()[0];
    assert_eq!(open_item["handle"], handle);
    assert_eq!(open_item["kind"], "task");
    assert_eq!(open_item["status"], "open");
    assert_eq!(open_item["last_event"]["command"], "task send");

    let show = vivi([
        "task",
        "show",
        "--project",
        project.path().to_str().unwrap(),
        &handle,
    ]);
    assert_success(&show);
    let show_stdout = stdout(&show);
    assert!(
        show_stdout.contains("Implement local delivery"),
        "{show_stdout}"
    );
    assert!(
        show_stdout.contains("Implement and test local delivery."),
        "{show_stdout}"
    );

    let done = vivi([
        "task",
        "done",
        "--project",
        project.path().to_str().unwrap(),
        &handle,
        "--for",
        "cto",
    ]);
    assert_success(&done);
    assert!(stdout(&done).contains(&format!("done {handle}")));

    let done_list = vivi([
        "task",
        "list",
        "--project",
        project.path().to_str().unwrap(),
        "--for",
        "cto",
        "--status",
        "done",
    ]);
    assert_success(&done_list);
    let done_stdout = stdout(&done_list);
    assert!(done_stdout.contains(&handle), "{done_stdout}");
    assert!(
        done_stdout.contains("Implement local delivery"),
        "{done_stdout}"
    );

    let done_json = vivi([
        "task",
        "list",
        "--project",
        project.path().to_str().unwrap(),
        "--for",
        "cto",
        "--status",
        "done",
        "--json",
    ]);
    assert_success(&done_json);
    let done_items: Value = serde_json::from_str(&stdout(&done_json)).unwrap();
    let done_item = &done_items.as_array().unwrap()[0];
    assert_eq!(done_item["handle"], handle);
    assert_eq!(done_item["status"], "done");
    assert_eq!(done_item["last_event"]["command"], "task done");

    let reopened = vivi([
        "task",
        "reopen",
        "--project",
        project.path().to_str().unwrap(),
        &handle,
        "--for",
        "cto",
    ]);
    assert_success(&reopened);
    assert!(stdout(&reopened).contains(&format!("reopened {handle}")));
}

#[test]
#[allow(clippy::too_many_lines)]
fn dump_commands_filter_mail_and_tasks_for_board_review() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());

    assert_success(&vivi([
        "mail",
        "send",
        "--project",
        project.path().to_str().unwrap(),
        "--from",
        "ceo",
        "--to",
        "cto",
        "--subject",
        "status: blocker review",
        "--body",
        "The release blocker is now assigned.",
    ]));
    let task = vivi([
        "task",
        "send",
        "--project",
        project.path().to_str().unwrap(),
        "--from",
        "ceo",
        "--to",
        "cto",
        "--subject",
        "Resolve release blocker",
        "--body",
        "Validate the release blocker fix.",
    ]);
    assert_success(&task);
    let task_handle = handle_after(&stdout(&task), "created cto");
    assert_success(&vivi([
        "task",
        "done",
        "--project",
        project.path().to_str().unwrap(),
        &task_handle,
        "--for",
        "cto",
        "--note",
        "Validated blocker fix.",
    ]));

    let mail_dump = vivi([
        "mail",
        "dump",
        "--project",
        project.path().to_str().unwrap(),
        "--participant",
        "cto",
        "--body",
        "release blocker",
    ]);
    assert_success(&mail_dump);
    let mail_stdout = stdout(&mail_dump);
    assert!(mail_stdout.contains("# Vivi Mail Dump"), "{mail_stdout}");
    assert!(
        mail_stdout.contains("status: blocker review"),
        "{mail_stdout}"
    );
    assert!(
        mail_stdout.contains("The release blocker is now assigned."),
        "{mail_stdout}"
    );
    assert!(mail_stdout.contains("Events:"), "{mail_stdout}");
    assert!(mail_stdout.contains("mail send delivered"), "{mail_stdout}");

    let default_task_dump = vivi([
        "task",
        "dump",
        "--project",
        project.path().to_str().unwrap(),
        "--participant",
        "cto",
        "--json",
    ]);
    assert_success(&default_task_dump);
    let default_task_stdout = stdout(&default_task_dump);
    assert!(
        !default_task_stdout.contains(&task_handle),
        "{default_task_stdout}"
    );

    let task_dump = vivi([
        "task",
        "dump",
        "--project",
        project.path().to_str().unwrap(),
        "--participant",
        "cto",
        "--status",
        "all",
        "--json",
    ]);
    assert_success(&task_dump);
    let task_stdout = stdout(&task_dump);
    assert!(task_stdout.contains(&task_handle), "{task_stdout}");
    assert!(
        task_stdout.contains("\"status\": \"done\""),
        "{task_stdout}"
    );
    assert!(task_stdout.contains("\"events\""), "{task_stdout}");
    assert!(
        task_stdout.contains("\"command\": \"task send\""),
        "{task_stdout}"
    );
    assert!(
        task_stdout.contains("\"command\": \"task done\""),
        "{task_stdout}"
    );
    assert!(
        task_stdout.contains("\"event_type\": \"moved\""),
        "{task_stdout}"
    );
    assert!(
        task_stdout.contains("\"note\": \"Validated blocker fix.\""),
        "{task_stdout}"
    );
    assert!(
        task_stdout.contains("Resolve release blocker"),
        "{task_stdout}"
    );
}

#[test]
fn stdout_dump_refuses_large_work_exports_without_confirm() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());

    for index in 0..26 {
        assert_success(&vivi([
            "task",
            "send",
            "--project",
            project.path().to_str().unwrap(),
            "--from",
            "ceo",
            "--to",
            "cto",
            "--subject",
            &format!("Bulk task {index}"),
            "--body",
            "Short body.",
        ]));
    }

    let dump = vivi([
        "task",
        "dump",
        "--project",
        project.path().to_str().unwrap(),
        "--for",
        "cto",
    ]);
    assert!(!dump.status.success(), "{}", stdout(&dump));
    assert!(
        stderr(&dump).contains("refusing large stdout dump"),
        "{}",
        stderr(&dump)
    );
    assert!(
        stderr(&dump).contains("--confirm-large"),
        "{}",
        stderr(&dump)
    );

    // JSON stdout is also guarded; --json alone must not bypass the limit.
    let json_dump = vivi([
        "task",
        "dump",
        "--project",
        project.path().to_str().unwrap(),
        "--for",
        "cto",
        "--json",
    ]);
    assert!(!json_dump.status.success(), "{}", stdout(&json_dump));
    assert!(
        stderr(&json_dump).contains("refusing large stdout dump"),
        "{}",
        stderr(&json_dump)
    );

    let confirmed = vivi([
        "task",
        "dump",
        "--project",
        project.path().to_str().unwrap(),
        "--for",
        "cto",
        "--confirm-large",
    ]);
    assert_success(&confirmed);
    let confirmed_stdout = stdout(&confirmed);
    assert!(confirmed_stdout.contains("count: 26"), "{confirmed_stdout}");

    let output_path = project.path().join("tasks.md");
    let output_dump = vivi([
        "task",
        "dump",
        "--project",
        project.path().to_str().unwrap(),
        "--for",
        "cto",
        "--output",
        output_path.to_str().unwrap(),
    ]);
    assert_success(&output_dump);
    let rendered = std::fs::read_to_string(output_path).unwrap();
    assert!(rendered.contains("count: 26"), "{rendered}");
}

#[test]
fn local_send_reads_body_file_and_stdin() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let body_file = project.path().join("body.md");
    std::fs::write(&body_file, "Body from file.").unwrap();

    let need = vivi([
        "need",
        "send",
        "--project",
        project.path().to_str().unwrap(),
        "--from",
        "ceo",
        "--to",
        "cto",
        "--subject",
        "Read file body",
        "--body-file",
        body_file.to_str().unwrap(),
    ]);
    assert_success(&need);
    let need_handle = handle_after(&stdout(&need), "created cto");

    let shown_need = vivi([
        "need",
        "show",
        "--project",
        project.path().to_str().unwrap(),
        &need_handle,
    ]);
    assert_success(&shown_need);
    assert!(stdout(&shown_need).contains("Body from file."));

    let task = vivi_with_stdin(
        [
            "task",
            "send",
            "--project",
            project.path().to_str().unwrap(),
            "--from",
            "ceo",
            "--to",
            "cto",
            "--subject",
            "Read stdin body",
            "--body",
            "-",
        ],
        "Body from stdin.",
    );
    assert_success(&task);
    let task_handle = handle_after(&stdout(&task), "created cto");

    let shown_task = vivi([
        "task",
        "show",
        "--project",
        project.path().to_str().unwrap(),
        &task_handle,
    ]);
    assert_success(&shown_task);
    assert!(stdout(&shown_task).contains("Body from stdin."));
}

#[test]
fn goal_register_list_board_and_drop() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let project_s = project.path().to_str().unwrap();

    let goal_dir = project.path().join("docs/factory");
    std::fs::create_dir_all(&goal_dir).unwrap();
    let goal_path = goal_dir.join("sample-goal.md");
    std::fs::write(&goal_path, "# Goal: sample\n\nKeep this campaign moving.\n").unwrap();

    let add = vivi([
        "goal",
        "add",
        "--project",
        project_s,
        "--path",
        "docs/factory/sample-goal.md",
        "--label",
        "sample",
        "--for",
        "mind",
        "--json",
    ]);
    assert_success(&add);
    let added: Value = serde_json::from_str(&stdout(&add)).unwrap();
    assert!(added["handle"].as_str().unwrap().starts_with("gol_"));
    assert_eq!(added["path"], "docs/factory/sample-goal.md");
    assert_eq!(added["label"], "sample");
    assert_eq!(added["exists"], true);
    assert_eq!(added["registered_by"], "mind");
    let handle = added["handle"].as_str().unwrap().to_string();

    let list = vivi(["goal", "list", "--project", project_s, "--json"]);
    assert_success(&list);
    let listed: Value = serde_json::from_str(&stdout(&list)).unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(listed[0]["handle"], handle);

    let board = vivi(["board", "--project", project_s, "--json"]);
    assert_success(&board);
    let board_value: Value = serde_json::from_str(&stdout(&board)).unwrap();
    assert_eq!(board_value["goals"].as_array().unwrap().len(), 1);
    assert_eq!(
        board_value["goals"][0]["path"],
        "docs/factory/sample-goal.md"
    );

    let board_text = vivi(["board", "--project", project_s]);
    assert_success(&board_text);
    let board_stdout = stdout(&board_text);
    assert!(board_stdout.contains("goals:"), "{board_stdout}");
    assert!(
        board_stdout.contains("docs/factory/sample-goal.md"),
        "{board_stdout}"
    );
    assert!(board_stdout.contains("sample"), "{board_stdout}");

    let drop = vivi(["goal", "drop", "--project", project_s, &handle, "--json"]);
    assert_success(&drop);
    let dropped: Value = serde_json::from_str(&stdout(&drop)).unwrap();
    assert_eq!(dropped["handle"], handle);

    let list_after = vivi(["goal", "list", "--project", project_s, "--json"]);
    assert_success(&list_after);
    let after: Value = serde_json::from_str(&stdout(&list_after)).unwrap();
    assert!(after.as_array().unwrap().is_empty(), "{after}");

    // File remains on disk after unregister.
    assert!(goal_path.is_file());
}

#[test]
#[allow(clippy::too_many_lines)]
fn board_and_status_report_actionable_work() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());

    let task = send_work(project.path(), "task", "cto", "Fix parser", "Patch parser.");
    let need = send_work(
        project.path(),
        "need",
        "cto",
        "Review patch",
        "Review parser.",
    );
    let want_a = send_work(project.path(), "want", "cto", "Track idea A", "Idea A.");
    let want_b = send_work(project.path(), "want", "cto", "Track idea B", "Idea B.");

    let board = vivi([
        "board",
        "--project",
        project.path().to_str().unwrap(),
        "--for",
        "cto",
        "--wants",
        "1",
    ]);
    assert_success(&board);
    let board_stdout = stdout(&board);
    assert!(
        board_stdout.contains("actionable open: 2"),
        "{board_stdout}"
    );
    assert!(board_stdout.contains(&task), "{board_stdout}");
    assert!(board_stdout.contains(&need), "{board_stdout}");
    let visible_wants =
        usize::from(board_stdout.contains(&want_a)) + usize::from(board_stdout.contains(&want_b));
    assert_eq!(visible_wants, 1, "{board_stdout}");
    assert!(
        board_stdout.contains("wants hidden by cap: 1"),
        "{board_stdout}"
    );

    let board_json = vivi([
        "board",
        "--project",
        project.path().to_str().unwrap(),
        "--for",
        "cto",
        "--wants",
        "1",
        "--json",
    ]);
    assert_success(&board_json);
    let board_value: Value = serde_json::from_str(&stdout(&board_json)).unwrap();
    assert_eq!(board_value["totals"]["actionable_open"], 2);
    assert_eq!(board_value["totals"]["wants_open"], 2);
    let identity = &board_value["identities"].as_array().unwrap()[0];
    assert_eq!(identity["actionable_open"], 2);
    assert_eq!(identity["tasks"].as_array().unwrap().len(), 1);
    assert_eq!(identity["needs"].as_array().unwrap().len(), 1);
    assert_eq!(identity["wants"].as_array().unwrap().len(), 1);
    assert_eq!(identity["wants_hidden"], 1);

    let status = vivi([
        "mailspace",
        "status",
        "--project",
        project.path().to_str().unwrap(),
    ]);
    assert_success(&status);
    assert!(
        stdout(&status).contains("total actionable open: 2"),
        "{}",
        stdout(&status)
    );

    let status_json = vivi([
        "mailspace",
        "status",
        "--project",
        project.path().to_str().unwrap(),
        "--json",
    ]);
    assert_success(&status_json);
    let status_value: Value = serde_json::from_str(&stdout(&status_json)).unwrap();
    assert_eq!(status_value["totals"]["actionable_open"], 2);
    assert_eq!(status_value["totals"]["wants_open"], 2);

    let future_board = vivi([
        "board",
        "--project",
        project.path().to_str().unwrap(),
        "--for",
        "cto",
        "--since",
        "2999-01-01T00:00:00Z",
        "--json",
    ]);
    assert_success(&future_board);
    let future_value: Value = serde_json::from_str(&stdout(&future_board)).unwrap();
    assert_eq!(future_value["totals"]["actionable_open"], 0);
    assert_eq!(future_value["identities"][0]["wants_hidden"], 0);

    let watermark = project.path().join("board.watermark");
    std::fs::write(&watermark, "2999-01-01T00:00:00Z").unwrap();
    let watermark_board = vivi([
        "board",
        "--project",
        project.path().to_str().unwrap(),
        "--for",
        "cto",
        "--watermark-file",
        watermark.to_str().unwrap(),
        "--json",
    ]);
    assert_success(&watermark_board);
    let watermark_value: Value = serde_json::from_str(&stdout(&watermark_board)).unwrap();
    assert_eq!(watermark_value["totals"]["actionable_open"], 0);

    let write_watermark = vivi([
        "board",
        "--project",
        project.path().to_str().unwrap(),
        "--for",
        "cto",
        "--since",
        "1h",
        "--watermark-file",
        watermark.to_str().unwrap(),
        "--write-watermark",
        "--json",
    ]);
    assert_success(&write_watermark);
    let written = std::fs::read_to_string(&watermark).unwrap();
    assert!(written.contains('T'), "{written}");
}

#[test]
fn board_process_reports_liveness_per_role() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let p = project.path().to_str().unwrap();

    // ceo self-registers a live pid; cto has no binding.
    let pid = std::process::id();
    assert_success(&vivi([
        "role",
        "set",
        "ceo",
        "--pid",
        &pid.to_string(),
        "--project",
        p,
    ]));

    // Add a subagent harness role with a stale pid. The board must not report
    // it as dead, because OS pid is not a valid liveness signal for subagents.
    assert_success(&vivi([
        "role",
        "add",
        "hand-1",
        "--kind",
        "hand",
        "--harness",
        "subagent",
        "--project",
        p,
    ]));
    assert_success(&vivi([
        "role",
        "set",
        "hand-1",
        "--pid",
        "4000000",
        "--project",
        p,
    ]));

    // Default board has no process block.
    let plain: Value = serde_json::from_str(&stdout(&vivi([
        "board",
        "--for",
        "ceo",
        "--json",
        "--project",
        p,
    ])))
    .unwrap();
    assert!(plain["identities"][0].get("process").is_none(), "{plain}");

    // --process adds a process block per role.
    let board: Value = serde_json::from_str(&stdout(&vivi([
        "board",
        "--process",
        "--json",
        "--project",
        p,
    ])))
    .unwrap();
    let by_name: std::collections::HashMap<&str, &Value> = board["identities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| (id["identity"].as_str().unwrap(), id))
        .collect();

    let ceo = by_name["ceo"];
    assert_eq!(ceo["process"]["state"], "alive");
    assert_eq!(ceo["process"]["running"], true);
    assert!(ceo["process"]["name"].as_str().is_some(), "{ceo}");
    // Board uses the quick probe; cpu is intentionally null.
    assert_eq!(ceo["process"]["cpu_percent"], Value::Null);

    // A role with no binding reads not_set (available to assign).
    let cto = by_name["cto"];
    assert_eq!(cto["process"]["state"], "not_set");
    assert_eq!(cto["process"]["running"], Value::Null);

    // Subagent harness ignores the stored pid and reports subagent.
    let hand = by_name["hand-1"];
    assert_eq!(hand["process"]["state"], "subagent");
    assert_eq!(hand["process"]["running"], Value::Null);

    // Text output surfaces the process line.
    let text = stdout(&vivi(["board", "--process", "--project", p]));
    assert!(text.contains("process: alive running:yes"), "{text}");
    assert!(text.contains("process: not_set running:unknown"), "{text}");
    assert!(text.contains("process: subagent running:unknown"), "{text}");
}

#[test]
fn want_done_drop_and_status_lists_closed_wants() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());

    let done_want = send_work(project.path(), "want", "ceo", "Close me", "Obsolete.");
    let dropped_want = send_work(project.path(), "want", "ceo", "Drop me", "Also obsolete.");

    let done = vivi([
        "want",
        "done",
        "--project",
        project.path().to_str().unwrap(),
        &done_want,
        "--for",
        "ceo",
        "--note",
        "No longer needed.",
    ]);
    assert_success(&done);
    assert!(stdout(&done).contains(&format!("done {done_want}")));

    let dropped = vivi([
        "want",
        "drop",
        "--project",
        project.path().to_str().unwrap(),
        &dropped_want,
        "--for",
        "ceo",
    ]);
    assert_success(&dropped);
    assert!(stdout(&dropped).contains(&format!("dropped {dropped_want}")));

    let open = vivi([
        "want",
        "list",
        "--project",
        project.path().to_str().unwrap(),
        "--for",
        "ceo",
    ]);
    assert_success(&open);
    assert!(!stdout(&open).contains(&done_want));
    assert!(!stdout(&open).contains(&dropped_want));

    let done_list = vivi([
        "want",
        "list",
        "--project",
        project.path().to_str().unwrap(),
        "--for",
        "ceo",
        "--status",
        "done",
        "--json",
    ]);
    assert_success(&done_list);
    let done_items: Value = serde_json::from_str(&stdout(&done_list)).unwrap();
    assert_eq!(done_items.as_array().unwrap().len(), 2);
    assert!(stdout(&done_list).contains(&done_want));
    assert!(stdout(&done_list).contains(&dropped_want));
    assert!(stdout(&done_list).contains("\"kind\": \"want\""));
    assert!(stdout(&done_list).contains("\"status\": \"done\""));

    let all_list = vivi([
        "want",
        "list",
        "--project",
        project.path().to_str().unwrap(),
        "--for",
        "ceo",
        "--status",
        "all",
        "--json",
    ]);
    assert_success(&all_list);
    assert!(stdout(&all_list).contains(&done_want));
    assert!(stdout(&all_list).contains(&dropped_want));
}

#[test]
#[allow(clippy::too_many_lines)]
fn want_promotes_to_need_and_done_without_polluting_task_done() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());

    let want = vivi([
        "want",
        "send",
        "--project",
        project.path().to_str().unwrap(),
        "--from",
        "ceo",
        "--to",
        "ceo",
        "--subject",
        "Improve board visibility",
        "--body",
        "Consider a future governance dashboard.",
    ]);
    assert_success(&want);
    let want_handle = handle_after(&stdout(&want), "created ceo");

    let want_list = vivi([
        "want",
        "list",
        "--project",
        project.path().to_str().unwrap(),
        "--for",
        "ceo",
    ]);
    assert_success(&want_list);
    assert!(stdout(&want_list).contains(&want_handle));

    let want_list_json = vivi([
        "want",
        "list",
        "--project",
        project.path().to_str().unwrap(),
        "--for",
        "ceo",
        "--json",
    ]);
    assert_success(&want_list_json);
    let want_items: Value = serde_json::from_str(&stdout(&want_list_json)).unwrap();
    let want_item = &want_items.as_array().unwrap()[0];
    assert_eq!(want_item["handle"], want_handle);
    assert_eq!(want_item["kind"], "want");
    assert_eq!(want_item["status"], "open");

    let promoted = vivi([
        "want",
        "promote",
        "--project",
        project.path().to_str().unwrap(),
        &want_handle,
        "--for",
        "ceo",
        "--note",
        "Prioritize next cycle.",
    ]);
    assert_success(&promoted);
    assert!(stdout(&promoted).contains(&format!("promoted {want_handle}")));

    let need_list = vivi([
        "need",
        "list",
        "--project",
        project.path().to_str().unwrap(),
        "--for",
        "ceo",
    ]);
    assert_success(&need_list);
    assert!(stdout(&need_list).contains(&want_handle));

    let done = vivi([
        "need",
        "done",
        "--project",
        project.path().to_str().unwrap(),
        &want_handle,
        "--for",
        "ceo",
        "--note",
        "Delegated and completed.",
    ]);
    assert_success(&done);
    assert!(stdout(&done).contains(&format!("done {want_handle}")));

    let done_tasks = vivi([
        "task",
        "list",
        "--project",
        project.path().to_str().unwrap(),
        "--for",
        "ceo",
        "--status",
        "done",
    ]);
    assert_success(&done_tasks);
    assert!(!stdout(&done_tasks).contains(&want_handle));

    let default_need_dump = vivi([
        "need",
        "dump",
        "--project",
        project.path().to_str().unwrap(),
        "--for",
        "ceo",
        "--json",
    ]);
    assert_success(&default_need_dump);
    let default_need_stdout = stdout(&default_need_dump);
    assert!(
        !default_need_stdout.contains(&want_handle),
        "{default_need_stdout}"
    );

    let done_needs = vivi([
        "need",
        "dump",
        "--project",
        project.path().to_str().unwrap(),
        "--for",
        "ceo",
        "--status",
        "done",
        "--json",
    ]);
    assert_success(&done_needs);
    let done_needs_stdout = stdout(&done_needs);
    assert!(
        done_needs_stdout.contains(&want_handle),
        "{done_needs_stdout}"
    );
    assert!(
        done_needs_stdout.contains("\"command\": \"want promote\""),
        "{done_needs_stdout}"
    );
    assert!(
        done_needs_stdout.contains("\"command\": \"need done\""),
        "{done_needs_stdout}"
    );
    assert!(
        done_needs_stdout.contains("\"kind\": \"need\""),
        "{done_needs_stdout}"
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn replies_and_lifecycle_notes_assemble_cross_kind_threads() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());

    let need = send_work(
        project.path(),
        "need",
        "cto",
        "Review API",
        "Please review.",
    );
    let reply = vivi([
        "mail",
        "reply",
        &need,
        "--project",
        project.path().to_str().unwrap(),
        "--from",
        "cto",
        "--body",
        "I reviewed it.",
    ]);
    assert_success(&reply);
    let reply_handle = handle_after(&stdout(&reply), "replied ceo");

    let task = vivi([
        "task",
        "send",
        "--project",
        project.path().to_str().unwrap(),
        "--from",
        "cto",
        "--to",
        "ceo",
        "--subject",
        "Implement follow-up",
        "--body",
        "Follow up on the review.",
        "--reply-to",
        &reply_handle,
    ]);
    assert_success(&task);
    let task_handle = handle_after(&stdout(&task), "created ceo");

    let thread = vivi([
        "mail",
        "thread",
        "--project",
        project.path().to_str().unwrap(),
        "--json",
        &task_handle,
    ]);
    assert_success(&thread);
    let messages: Value = serde_json::from_str(&stdout(&thread)).unwrap();
    assert_eq!(messages.as_array().unwrap().len(), 3);
    assert!(
        messages
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["kind"] == "task")
    );
    assert!(
        messages
            .as_array()
            .unwrap()
            .iter()
            .skip(1)
            .any(|message| message["link_source"] == "captured")
    );

    let done = vivi([
        "task",
        "done",
        "--project",
        project.path().to_str().unwrap(),
        &task_handle,
        "--for",
        "ceo",
        "--note",
        "Follow-up is complete.",
    ]);
    assert_success(&done);

    let done_thread = vivi([
        "task",
        "show",
        "--project",
        project.path().to_str().unwrap(),
        "--json",
        &task_handle,
    ]);
    assert_success(&done_thread);
    let done_messages: Value = serde_json::from_str(&stdout(&done_thread)).unwrap();
    assert_eq!(done_messages.as_array().unwrap().len(), 4);
    assert!(stdout(&done_thread).contains("Follow-up is complete."));

    let dump = vivi([
        "mail",
        "dump",
        "--project",
        project.path().to_str().unwrap(),
        "--for",
        "ceo",
        "--folder",
        "inbox",
        "--json",
    ]);
    assert_success(&dump);
    assert!(stdout(&dump).contains("parent_content_id"));
    assert!(stdout(&dump).contains("captured"));
}

#[test]
fn need_and_want_lists_show_a_thread_only_by_its_root() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let p = project.path().to_str().unwrap();

    for kind in ["need", "want"] {
        let root = send_work(project.path(), kind, "cto", &format!("{kind} root"), "body");
        let reply = vivi([
            kind,
            "send",
            "--project",
            p,
            "--from",
            "ceo",
            "--to",
            "cto",
            "--subject",
            &format!("Re: {kind} root"),
            "--body",
            "a reply",
            "--reply-to",
            &root,
        ]);
        assert_success(&reply);

        let listed = vivi([kind, "list", "--project", p, "--for", "cto", "--json"]);
        assert_success(&listed);
        let rows: Value = serde_json::from_str(&stdout(&listed)).unwrap();
        let subjects: Vec<&str> = rows
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|row| row["subject"].as_str())
            .collect();
        assert!(
            subjects.contains(&format!("{kind} root").as_str()),
            "{subjects:?}"
        );
        assert!(
            !subjects.iter().any(|subject| subject.starts_with("Re:")),
            "a reply is not its own open item: {subjects:?}"
        );

        let show = vivi([kind, "show", "--project", p, "--json", &root]);
        assert_success(&show);
        let messages: Value = serde_json::from_str(&stdout(&show)).unwrap();
        assert_eq!(
            messages.as_array().unwrap().len(),
            2,
            "{kind} thread: {messages}"
        );
    }
}

#[test]
fn mailspace_watch_filters_events_and_advances_cursor() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let task = send_work(
        project.path(),
        "task",
        "cto",
        "Turn end: test",
        "Work finished.",
    );
    let cursor = project.path().join("watch.cursor");

    let first = vivi([
        "mailspace",
        "watch",
        "--project",
        project.path().to_str().unwrap(),
        "--for",
        "cto",
        "--kinds",
        "task",
        "--events",
        "delivered",
        "--match-subject-prefix",
        "Turn end:",
        "--cursor-file",
        cursor.to_str().unwrap(),
        "--write-cursor",
        "--once",
        "--json",
    ]);
    assert_success(&first);
    let event: Value = serde_json::from_str(stdout(&first).trim()).unwrap();
    assert_eq!(event["event"], "delivered");
    assert_eq!(event["kind"], "task");
    assert_eq!(event["for"], "cto");
    assert_eq!(event["handle"], task);
    let cursor_value = std::fs::read_to_string(&cursor).unwrap();
    assert!(!cursor_value.trim().is_empty());

    let done = vivi([
        "task",
        "done",
        "--project",
        project.path().to_str().unwrap(),
        &task,
        "--for",
        "cto",
    ]);
    assert_success(&done);

    let moved = vivi([
        "task",
        "watch",
        "--project",
        project.path().to_str().unwrap(),
        "--for",
        "cto",
        "--handle",
        &task,
        "--events",
        "moved",
        "--statuses",
        "done",
        "--timeout",
        "2s",
        "--json",
    ]);
    assert_success(&moved);
    let moved_event: Value = serde_json::from_str(stdout(&moved).trim()).unwrap();
    assert_eq!(moved_event["status"], "done");
    assert_eq!(moved_event["handle"], task);
}

#[test]
fn inferred_thread_links_are_opt_in_and_marked() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let first = vivi([
        "mail",
        "send",
        "--project",
        project.path().to_str().unwrap(),
        "--from",
        "ceo",
        "--to",
        "cto",
        "--subject",
        "Status report",
        "--body",
        "Initial status.",
    ]);
    assert_success(&first);
    let first_handle = handle_after(&stdout(&first), "delivered cto");
    let second = vivi([
        "mail",
        "send",
        "--project",
        project.path().to_str().unwrap(),
        "--from",
        "cto",
        "--to",
        "ceo",
        "--subject",
        "Re: Status report",
        "--body",
        &format!("Answering {first_handle}: current status."),
    ]);
    assert_success(&second);
    let second_handle = handle_after(&stdout(&second), "delivered ceo");

    let without_inference = vivi([
        "mail",
        "thread",
        "--project",
        project.path().to_str().unwrap(),
        "--json",
        &second_handle,
    ]);
    assert_success(&without_inference);
    assert_eq!(
        serde_json::from_str::<Value>(&stdout(&without_inference))
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let inferred = vivi([
        "mail",
        "thread",
        "--project",
        project.path().to_str().unwrap(),
        "--infer",
        "--json",
        &second_handle,
    ]);
    assert_success(&inferred);
    let messages: Value = serde_json::from_str(&stdout(&inferred)).unwrap();
    assert_eq!(messages.as_array().unwrap().len(), 2);
    assert!(
        messages
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["inferred"] == true && message["link_source"] == "inferred")
    );
}

#[test]
fn mailspace_import_dry_run_reports_without_writing() {
    let source = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    init_roster(source.path());
    init_roster(target.path());
    send_work(
        source.path(),
        "task",
        "cto",
        "recoverable task",
        "Preserve this institutional memory.",
    );

    let dry_run = vivi([
        "mailspace",
        "import",
        "--project",
        target.path().to_str().unwrap(),
        "--from",
        source.path().to_str().unwrap(),
        "--dry-run",
        "--json",
    ]);
    assert_success(&dry_run);
    let report: Value = serde_json::from_str(&stdout(&dry_run)).unwrap();
    assert_eq!(report["dry_run"], true);
    assert_eq!(report["imported_messages"], 2);
    assert_eq!(report["imported_events"], 2);

    let target_tasks = vivi([
        "task",
        "list",
        "--project",
        target.path().to_str().unwrap(),
        "--for",
        "cto",
    ]);
    assert_success(&target_tasks);
    assert!(
        !stdout(&target_tasks).contains("recoverable task"),
        "{}",
        stdout(&target_tasks)
    );
}

#[test]
fn mailspace_import_dry_run_resolves_links_via_source_blobs() {
    // Regression: dry-run validated source links against the target blob set
    // only. Because dry-run does not write source blobs, every link whose
    // child/parent blob lived only in source was falsely reported as
    // "references missing merged blob". A link is resolvable for import when
    // its blobs exist in target OR source.
    let source = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    init_roster(source.path());
    init_roster(target.path());
    let task = send_work(
        source.path(),
        "task",
        "cto",
        "linked task",
        "Carry forward.",
    );
    let done = vivi([
        "task",
        "done",
        "--project",
        source.path().to_str().unwrap(),
        "--for",
        "cto",
        &task,
        "--note",
        "closing note creates a captured thread link",
    ]);
    assert_success(&done);

    let dry_run = vivi([
        "mailspace",
        "import",
        "--project",
        target.path().to_str().unwrap(),
        "--from",
        source.path().to_str().unwrap(),
        "--dry-run",
        "--json",
    ]);
    assert_success(&dry_run);
    let report: Value = serde_json::from_str(&stdout(&dry_run)).unwrap();
    assert_eq!(report["dry_run"], true);
    assert_eq!(
        report["conflicts"].as_array().unwrap().len(),
        0,
        "dry-run must not flag links resolvable via source blobs: {}",
        report["conflicts"]
    );
    assert_eq!(report["imported_links"], 1);

    // Dry-run wrote nothing.
    let target_tasks = vivi([
        "task",
        "list",
        "--project",
        target.path().to_str().unwrap(),
        "--for",
        "cto",
    ]);
    assert_success(&target_tasks);
    assert!(!stdout(&target_tasks).contains("linked task"));
}

#[test]
fn mailspace_import_copies_messages_events_and_dedupes_second_run() {
    let source = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    init_roster(source.path());
    init_roster(target.path());
    let task = send_work(
        source.path(),
        "task",
        "cto",
        "merged task",
        "Carry this forward.",
    );
    let done = vivi([
        "task",
        "done",
        "--project",
        source.path().to_str().unwrap(),
        "--for",
        "cto",
        &task,
        "--note",
        "Recovered completion note",
    ]);
    assert_success(&done);

    let import = vivi([
        "mailspace",
        "import",
        "--project",
        target.path().to_str().unwrap(),
        "--from",
        source.path().join(".vivi").to_str().unwrap(),
        "--json",
    ]);
    assert_success(&import);
    let report: Value = serde_json::from_str(&stdout(&import)).unwrap();
    assert_eq!(report["imported_messages"], 4);
    assert_eq!(report["imported_blobs"], 2);
    assert_eq!(report["deduped_blobs"], 2);
    assert_eq!(report["imported_events"], 3);
    assert_eq!(report["imported_links"], 1);
    assert_eq!(report["conflicts"].as_array().unwrap().len(), 0);

    let dump = vivi([
        "task",
        "dump",
        "--project",
        target.path().to_str().unwrap(),
        "--for",
        "cto",
        "--status",
        "all",
        "--json",
    ]);
    assert_success(&dump);
    let dump_stdout = stdout(&dump);
    assert!(dump_stdout.contains("merged task"), "{dump_stdout}");
    assert!(
        dump_stdout.contains("Recovered completion note"),
        "{dump_stdout}"
    );

    let second = vivi([
        "mailspace",
        "import",
        "--project",
        target.path().to_str().unwrap(),
        "--from",
        source.path().to_str().unwrap(),
        "--json",
    ]);
    assert_success(&second);
    let second_report: Value = serde_json::from_str(&stdout(&second)).unwrap();
    assert_eq!(second_report["imported_messages"], 0);
    assert_eq!(second_report["deduped_messages"], 4);
    assert_eq!(second_report["imported_events"], 0);
}

#[test]
fn mail_absorb_marks_inbox_read_and_clears_unread() {
    // Absorb means "read, processed, loaded into context", so it must mark the
    // message read and drop it from `inbox_unread` — the count boards, sensors,
    // and Minds read on as a neglect signal.
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());

    let send = vivi([
        "mail",
        "send",
        "--project",
        project.path().to_str().unwrap(),
        "--from",
        "ceo",
        "--to",
        "cto",
        "--subject",
        "absorb me",
        "--body",
        "read, processed, loaded into context",
    ]);
    assert_success(&send);
    let handle = handle_after(&stdout(&send), "delivered cto");
    assert_eq!(
        inbox_unread(project.path(), "cto"),
        1,
        "delivered inbox mail starts unread"
    );

    let absorb = vivi([
        "mail",
        "absorb",
        "--project",
        project.path().to_str().unwrap(),
        "--for",
        "cto",
        &handle,
    ]);
    assert_success(&absorb);
    assert_eq!(
        inbox_unread(project.path(), "cto"),
        0,
        "absorbed mail must no longer count as unread"
    );
}

#[test]
fn absorb_seals_records_and_rejects_later_mutation() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let root = project.path().to_str().unwrap();

    let task = send_work(project.path(), "task", "cto", "seal task", "do it");
    assert_success(&vivi([
        "task",
        "absorb",
        "--project",
        root,
        "--for",
        "cto",
        &task,
    ]));
    let done = vivi(["task", "done", "--project", root, "--for", "cto", &task]);
    assert!(!done.status.success(), "{}", stderr(&done));
    assert!(
        stderr(&done).contains("is absorbed and can no longer be changed"),
        "{}",
        stderr(&done)
    );

    let need = send_work(project.path(), "need", "cto", "seal need", "decide");
    assert_success(&vivi([
        "need",
        "absorb",
        "--project",
        root,
        "--for",
        "cto",
        &need,
        "--note",
        "closed",
    ]));
    let reopen = vivi(["need", "reopen", "--project", root, "--for", "cto", &need]);
    assert!(!reopen.status.success(), "{}", stderr(&reopen));
    assert!(
        stderr(&reopen).contains("is absorbed and can no longer be changed"),
        "{}",
        stderr(&reopen)
    );

    let want = send_work(project.path(), "want", "cto", "seal want", "later");
    assert_success(&vivi([
        "want",
        "absorb",
        "--project",
        root,
        "--for",
        "cto",
        &want,
    ]));
    let priority = vivi([
        "want",
        "set-priority",
        "--project",
        root,
        "--for",
        "cto",
        &want,
        "--priority",
        "P1",
    ]);
    assert!(!priority.status.success(), "{}", stderr(&priority));
    assert!(
        stderr(&priority).contains("is absorbed and can no longer be changed"),
        "{}",
        stderr(&priority)
    );

    let memo = vivi([
        "memo",
        "save",
        "--project",
        root,
        "--for",
        "cto",
        "--subject",
        "note",
        "--body",
        "keep",
    ]);
    assert_success(&memo);
    let memo_handle = handle_after(&stdout(&memo), "saved");
    assert_success(&vivi([
        "memo",
        "absorb",
        "--project",
        root,
        "--for",
        "cto",
        &memo_handle,
    ]));
    let delete = vivi([
        "memo",
        "delete",
        "--project",
        root,
        "--for",
        "cto",
        &memo_handle,
    ]);
    assert!(!delete.status.success(), "{}", stderr(&delete));
    assert!(
        stderr(&delete).contains("is absorbed and can no longer be changed"),
        "{}",
        stderr(&delete)
    );

    let again = vivi(["task", "absorb", "--project", root, "--for", "cto", &task]);
    assert_success(&again);
    assert!(stdout(&again).contains(&task), "{}", stdout(&again));
}

#[test]
fn absorb_writes_markdown_into_configured_archive_repo() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let archive = project.path().join("vivi");
    std::fs::create_dir_all(archive.join(".git")).unwrap();
    let root = project.path().to_str().unwrap();
    assert_success(&vivi([
        "mailspace",
        "archive",
        "--project",
        root,
        "--set",
        archive.to_str().unwrap(),
    ]));
    let shown = vivi(["mailspace", "archive", "--project", root]);
    assert_success(&shown);
    assert!(
        stdout(&shown).contains(archive.to_str().unwrap()),
        "{}",
        stdout(&shown)
    );

    let task = send_work(
        project.path(),
        "task",
        "cto",
        "archive me",
        "body for history",
    );
    assert_success(&vivi([
        "task",
        "absorb",
        "--project",
        root,
        "--for",
        "cto",
        &task,
    ]));

    let toml = std::fs::read_to_string(project.path().join(".vivi/mailspace.toml")).unwrap();
    let project_name = toml
        .lines()
        .find_map(|line| line.strip_prefix("name = "))
        .map(|value| value.trim_matches('"'))
        .expect("mailspace name");
    let dir = archive.join(project_name).join("task");
    let found: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(found.len(), 1, "expected one archive file in {dir:?}");
    let rendered = std::fs::read_to_string(&found[0]).unwrap();
    assert!(rendered.contains("kind = \"task\""), "{rendered}");
    assert!(rendered.contains("body for history"), "{rendered}");
}

#[test]
fn archive_export_backfills_already_absorbed_records() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let root = project.path().to_str().unwrap();
    let task = send_work(
        project.path(),
        "task",
        "cto",
        "old absorb",
        "sealed before archive",
    );
    assert_success(&vivi([
        "task",
        "absorb",
        "--project",
        root,
        "--for",
        "cto",
        &task,
    ]));

    let missing = vivi(["mailspace", "archive", "export", "--project", root]);
    assert!(!missing.status.success(), "{}", stderr(&missing));
    assert!(
        stderr(&missing).contains("no archive configured"),
        "{}",
        stderr(&missing)
    );

    let archive = project.path().join("vivi");
    std::fs::create_dir_all(archive.join(".git")).unwrap();
    assert_success(&vivi([
        "mailspace",
        "archive",
        "--project",
        root,
        "--set",
        archive.to_str().unwrap(),
    ]));

    let first = vivi([
        "mailspace",
        "archive",
        "export",
        "--project",
        root,
        "--json",
    ]);
    assert_success(&first);
    let first_report: Value = serde_json::from_str(&stdout(&first)).unwrap();
    assert_eq!(first_report["scanned"], 1);
    assert_eq!(first_report["written"], 1);
    assert_eq!(first_report["unchanged"], 0);

    let second = vivi([
        "mailspace",
        "archive",
        "export",
        "--project",
        root,
        "--json",
    ]);
    assert_success(&second);
    let second_report: Value = serde_json::from_str(&stdout(&second)).unwrap();
    assert_eq!(second_report["scanned"], 1);
    assert_eq!(second_report["written"], 0);
    assert_eq!(second_report["unchanged"], 1);
}

#[allow(clippy::cast_possible_truncation)]
fn inbox_unread(project: &std::path::Path, identity: &str) -> usize {
    let output = vivi([
        "mailspace",
        "status",
        "--project",
        project.to_str().unwrap(),
        "--json",
    ]);
    assert_success(&output);
    let value: Value = serde_json::from_str(&stdout(&output)).unwrap();
    value["identities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|id| id["identity"] == identity)
        .unwrap()["inbox_unread"]
        .as_u64()
        .unwrap() as usize
}

#[test]
#[allow(clippy::too_many_lines)]
fn role_add_set_charter_show_and_rename() {
    let project = tempfile::tempdir().unwrap();
    assert_success(&vivi([
        "mailspace",
        "init",
        "--project",
        project.path().to_str().unwrap(),
    ]));

    let add = vivi([
        "role",
        "add",
        "head-ceo",
        "--kind",
        "head",
        "--harness",
        "subagent",
        "--label",
        "executive",
        "--project",
        project.path().to_str().unwrap(),
    ]);
    assert_success(&add);
    assert!(stdout(&add).contains("head-ceo@"), "{}", stdout(&add));

    let set = vivi([
        "role",
        "set",
        "head-ceo",
        "--provider",
        "zai",
        "--model",
        "glm-5.2",
        "--thinking",
        "high",
        "--project",
        project.path().to_str().unwrap(),
    ]);
    assert_success(&set);

    let charter_path = project.path().join("ceo-charter.md");
    std::fs::write(&charter_path, "You are head-ceo.\nFocus on map health.\n").unwrap();
    let charter = vivi([
        "role",
        "charter",
        "set",
        "head-ceo",
        "--file",
        charter_path.to_str().unwrap(),
        "--project",
        project.path().to_str().unwrap(),
    ]);
    assert_success(&charter);

    let show = vivi([
        "role",
        "show",
        "head-ceo",
        "--json",
        "--project",
        project.path().to_str().unwrap(),
    ]);
    assert_success(&show);
    let value: Value = serde_json::from_str(&stdout(&show)).unwrap();
    assert_eq!(value["name"], "head-ceo");
    assert_eq!(value["kind"], "head");
    assert_eq!(value["harness"], "subagent");
    assert_eq!(value["provider"], "zai");
    assert_eq!(value["model"], "glm-5.2");
    assert_eq!(value["thinking"], "high");
    assert_eq!(value["status"], "active");
    assert_eq!(value["labels"][0], "executive");
    assert_eq!(value["has_charter"], true);
    assert!(
        value["charter"].as_str().unwrap().contains("map health"),
        "{value}"
    );

    // Existing identity CLI still works and coexists.
    assert_success(&vivi([
        "mailspace",
        "identity",
        "add",
        "hand-1",
        "--project",
        project.path().to_str().unwrap(),
    ]));
    assert_success(&vivi([
        "role",
        "set",
        "hand-1",
        "--kind",
        "hand",
        "--provider",
        "openai-codex",
        "--model",
        "gpt-5.5",
        "--project",
        project.path().to_str().unwrap(),
    ]));

    // Mail delivery still works against role names.
    assert_success(&vivi([
        "mail",
        "send",
        "--project",
        project.path().to_str().unwrap(),
        "--from",
        "hand-1",
        "--to",
        "head-ceo",
        "--subject",
        "hello role",
        "--body",
        "pointer boot works",
    ]));

    let rename = vivi([
        "role",
        "rename",
        "head-ceo",
        "chief",
        "--project",
        project.path().to_str().unwrap(),
    ]);
    assert_success(&rename);
    let show_renamed = vivi([
        "role",
        "show",
        "chief",
        "--json",
        "--project",
        project.path().to_str().unwrap(),
    ]);
    assert_success(&show_renamed);
    let renamed: Value = serde_json::from_str(&stdout(&show_renamed)).unwrap();
    assert_eq!(renamed["name"], "chief");
    assert!(
        renamed["aliases"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a == "head-ceo"),
        "{renamed}"
    );
    assert!(
        renamed["charter"].as_str().unwrap().contains("map health"),
        "charter should move on rename: {renamed}"
    );

    let list = vivi([
        "role",
        "list",
        "--json",
        "--project",
        project.path().to_str().unwrap(),
    ]);
    assert_success(&list);
    let listed: Value = serde_json::from_str(&stdout(&list)).unwrap();
    assert!(listed.as_array().unwrap().len() >= 2, "{listed}");
}

#[test]
fn role_add_on_existing_identity_errors_instead_of_silently_dropping_flags() {
    let project = tempfile::tempdir().unwrap();
    assert_success(&vivi([
        "mailspace",
        "init",
        "--project",
        project.path().to_str().unwrap(),
    ]));

    // One-step create applies every flag.
    let first = vivi([
        "role",
        "add",
        "hand-1",
        "--kind",
        "hand",
        "--harness",
        "subagent",
        "--label",
        "floater",
        "--project",
        project.path().to_str().unwrap(),
    ]);
    assert_success(&first);
    assert!(stdout(&first).contains("hand-1@"), "{}", stdout(&first));

    // Re-adding the same name must error, not report success while dropping flags.
    let dup = vivi([
        "role",
        "add",
        "hand-1",
        "--kind",
        "head",
        "--label",
        "executive",
        "--project",
        project.path().to_str().unwrap(),
    ]);
    assert!(
        !dup.status.success(),
        "re-add should fail, not silently no-op: {}",
        stderr(&dup)
    );
    let err = stderr(&dup);
    assert!(err.contains("already exists"), "{err}");
    assert!(
        err.contains("role set"),
        "error should point to role set: {err}"
    );

    // The failed re-add must not have mutated the original role.
    let show = vivi([
        "role",
        "show",
        "hand-1",
        "--json",
        "--project",
        project.path().to_str().unwrap(),
    ]);
    assert_success(&show);
    let value: Value = serde_json::from_str(&stdout(&show)).unwrap();
    assert_eq!(value["kind"], "hand");
    assert_eq!(value["harness"], "subagent");
    assert_eq!(value["labels"][0], "floater");
}

#[test]
fn memo_search_filters_by_subject_and_body() {
    let project = tempfile::tempdir().unwrap();
    assert_success(&vivi([
        "mailspace",
        "init",
        "--project",
        project.path().to_str().unwrap(),
    ]));
    assert_success(&vivi([
        "mailspace",
        "identity",
        "add",
        "mind",
        "--project",
        project.path().to_str().unwrap(),
    ]));
    let p = project.path().to_str().unwrap();

    // Save three memos: subject match, body match, and no match.
    assert_success(&vivi([
        "memo",
        "save",
        "--for",
        "mind",
        "--subject",
        "railway deploy paused",
        "--body",
        "The deployment is paused until Monday.",
        "--project",
        p,
    ]));
    assert_success(&vivi([
        "memo",
        "save",
        "--for",
        "mind",
        "--subject",
        "Weekly standup notes",
        "--body",
        "Discuss railway deploy issues and swarm capacity.",
        "--project",
        p,
    ]));
    assert_success(&vivi([
        "memo",
        "save",
        "--for",
        "mind",
        "--subject",
        "Grocery list",
        "--body",
        "Milk, eggs, bread.",
        "--project",
        p,
    ]));

    let out = vivi(["memo", "search", "railway", "--for", "mind", "--project", p]);
    assert_success(&out);
    let text = stdout(&out);
    assert!(text.contains("railway deploy paused"), "{text}");
    assert!(text.contains("Weekly standup notes"), "{text}");
    assert!(!text.contains("Grocery list"), "{text}");

    // Subject-only search should only match the first memo.
    let out = vivi([
        "memo",
        "search",
        "railway",
        "--for",
        "mind",
        "--subject",
        "--project",
        p,
    ]);
    assert_success(&out);
    let text = stdout(&out);
    assert!(text.contains("railway deploy paused"), "{text}");
    assert!(!text.contains("Weekly standup notes"), "{text}");
    assert!(!text.contains("Grocery list"), "{text}");

    // Body-only search should match the second memo.
    let out = vivi(["memo", "search", "swarm", "--for", "mind", "--project", p]);
    assert_success(&out);
    let text = stdout(&out);
    assert!(!text.contains("railway deploy paused"), "{text}");
    assert!(text.contains("Weekly standup notes"), "{text}");
    assert!(!text.contains("Grocery list"), "{text}");

    // JSON output preserves the list shape.
    let out = vivi([
        "memo",
        "search",
        "railway",
        "--for",
        "mind",
        "--json",
        "--project",
        p,
    ]);
    assert_success(&out);
    let v: Value = serde_json::from_str(&stdout(&out)).unwrap();
    let items = v.as_array().unwrap();
    assert_eq!(items.len(), 2, "{v}");
    let subjects: Vec<&str> = items
        .iter()
        .map(|item| item["subject"].as_str().unwrap())
        .collect();
    assert!(subjects.contains(&"railway deploy paused"));
    assert!(subjects.contains(&"Weekly standup notes"));
}

#[test]
fn memo_list_filters_and_hides_digest() {
    let project = tempfile::tempdir().unwrap();
    assert_success(&vivi([
        "mailspace",
        "init",
        "--project",
        project.path().to_str().unwrap(),
    ]));
    assert_success(&vivi([
        "mailspace",
        "identity",
        "add",
        "mind",
        "--project",
        project.path().to_str().unwrap(),
    ]));
    let p = project.path().to_str().unwrap();

    assert_success(&vivi([
        "memo",
        "save",
        "--for",
        "mind",
        "--subject",
        "plain note",
        "--body",
        "no tags here",
        "--project",
        p,
    ]));
    assert_success(&vivi([
        "memo",
        "save",
        "--for",
        "mind",
        "--subject",
        "Daily digest",
        "--body",
        "the digest",
        "--tag",
        "digest",
        "--project",
        p,
    ]));
    assert_success(&vivi([
        "memo",
        "save",
        "--for",
        "mind",
        "--subject",
        "ops runbook",
        "--body",
        "restart steps",
        "--tag",
        "ops",
        "--project",
        p,
    ]));

    // Default list hides the digest-tagged memo.
    let out = vivi(["memo", "list", "--for", "mind", "--project", p]);
    assert_success(&out);
    let text = stdout(&out);
    assert!(text.contains("plain note"), "{text}");
    assert!(text.contains("ops runbook"), "{text}");
    assert!(!text.contains("Daily digest"), "{text}");

    // Naming the hidden tag reveals it, and only it.
    let out = vivi([
        "memo",
        "list",
        "--for",
        "mind",
        "--tag",
        "digest",
        "--project",
        p,
    ]);
    assert_success(&out);
    let text = stdout(&out);
    assert!(text.contains("Daily digest"), "{text}");
    assert!(!text.contains("plain note"), "{text}");
    assert!(!text.contains("ops runbook"), "{text}");

    // --all shows everything.
    let out = vivi(["memo", "list", "--for", "mind", "--all", "--project", p]);
    assert_success(&out);
    let text = stdout(&out);
    assert!(text.contains("plain note"), "{text}");
    assert!(text.contains("ops runbook"), "{text}");
    assert!(text.contains("Daily digest"), "{text}");

    // --exclude-tag drops that memo.
    let out = vivi([
        "memo",
        "list",
        "--for",
        "mind",
        "--exclude-tag",
        "ops",
        "--project",
        p,
    ]);
    assert_success(&out);
    let text = stdout(&out);
    assert!(text.contains("plain note"), "{text}");
    assert!(!text.contains("ops runbook"), "{text}");
    assert!(!text.contains("Daily digest"), "{text}");

    // A recent duration keeps the memos; a far-future floor drops them all.
    let out = vivi([
        "memo",
        "list",
        "--for",
        "mind",
        "--since",
        "1h",
        "--project",
        p,
    ]);
    assert_success(&out);
    let text = stdout(&out);
    assert!(text.contains("plain note"), "{text}");
    assert!(text.contains("ops runbook"), "{text}");

    let out = vivi([
        "memo",
        "list",
        "--for",
        "mind",
        "--since",
        "2999-01-01",
        "--project",
        p,
    ]);
    assert_success(&out);
    let text = stdout(&out);
    assert!(!text.contains("plain note"), "{text}");
    assert!(text.contains("no memos"), "{text}");

    // --limit 1 prints exactly one memo line.
    let out = vivi([
        "memo",
        "list",
        "--for",
        "mind",
        "--all",
        "--limit",
        "1",
        "--project",
        p,
    ]);
    assert_success(&out);
    let text = stdout(&out);
    let rows: Vec<&str> = text
        .lines()
        .skip(1)
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert_eq!(rows.len(), 1, "{text}");

    // JSON rows carry the tags array alongside handle/date/subject.
    let out = vivi(["memo", "list", "--for", "mind", "--json", "--project", p]);
    assert_success(&out);
    let v: Value = serde_json::from_str(&stdout(&out)).unwrap();
    let items = v.as_array().unwrap();
    assert_eq!(items.len(), 2, "{v}");
    for item in items {
        assert!(item["handle"].as_str().is_some(), "{v}");
        assert!(item["date"].as_str().is_some(), "{v}");
        assert!(item["subject"].as_str().is_some(), "{v}");
        assert!(item["tags"].as_array().is_some(), "{v}");
    }
    let ops = items
        .iter()
        .find(|item| item["subject"] == "ops runbook")
        .unwrap();
    assert_eq!(ops["tags"], serde_json::json!(["ops"]), "{v}");
    let plain = items
        .iter()
        .find(|item| item["subject"] == "plain note")
        .unwrap();
    assert_eq!(plain["tags"], serde_json::json!([]), "{v}");

    // Search applies the same hidden-tag default and the same tag filters.
    let out = vivi(["memo", "search", "digest", "--for", "mind", "--project", p]);
    assert_success(&out);
    let text = stdout(&out);
    assert!(!text.contains("Daily digest"), "{text}");

    let out = vivi([
        "memo",
        "search",
        "digest",
        "--for",
        "mind",
        "--tag",
        "digest",
        "--project",
        p,
    ]);
    assert_success(&out);
    let text = stdout(&out);
    assert!(text.contains("Daily digest"), "{text}");

    let out = vivi([
        "memo",
        "search",
        "restart",
        "--for",
        "mind",
        "--exclude-tag",
        "ops",
        "--project",
        p,
    ]);
    assert_success(&out);
    let text = stdout(&out);
    assert!(text.contains("no memos"), "{text}");
}

fn send_mail(project: &str, from: &str, to: &str, subject: &str, body: &str) -> String {
    let output = vivi([
        "mail",
        "send",
        "--project",
        project,
        "--from",
        from,
        "--to",
        to,
        "--subject",
        subject,
        "--body",
        body,
    ]);
    assert_success(&output);
    handle_after(&stdout(&output), &format!("delivered {to}"))
}

fn list_mail_json(project: &str, extra: &[&str]) -> Value {
    let mut args = vec!["mail", "list", "--project", project, "--json"];
    args.extend_from_slice(extra);
    let output = vivi(args);
    assert_success(&output);
    serde_json::from_str(&stdout(&output)).unwrap()
}

fn handles(items: &Value) -> Vec<String> {
    let mut handles = items
        .as_array()
        .expect("mail list json array")
        .iter()
        .map(|item| item["handle"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    handles.sort();
    handles
}

fn sorted(values: &[&str]) -> Vec<String> {
    let mut values = values
        .iter()
        .map(|value| (*value).to_string())
        .collect::<Vec<_>>();
    values.sort();
    values
}

#[test]
fn task_from_records_source_provenance_and_want_list_has_no_task_column() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let project_s = project.path().to_str().unwrap();
    let want = send_work(project.path(), "want", "cto", "want: tidy the seam", "body");

    let created = vivi([
        "task",
        "from",
        "--project",
        project_s,
        "--for",
        "cto",
        "--to",
        "ceo",
        "--subject",
        "task: tidy the seam",
        "--body",
        "done_when: the seam is tidy",
        &want,
    ]);
    assert_success(&created);
    assert!(stdout(&created).contains(&want), "{}", stdout(&created));
    let task = handle_after(&stdout(&created), "created ceo");

    // The association is recorded once, on the created task, where the work is.
    let storage = Storage::open_mailspace(&project.path().join(".vivi")).unwrap();
    let metadata = storage.item_metadata(&task).unwrap();
    assert_eq!(
        metadata.get("source_handle").map(String::as_str),
        Some(want.as_str())
    );
    assert_eq!(
        metadata.get("source_kind").map(String::as_str),
        Some("want")
    );
    assert!(metadata.contains_key("source_content_id"), "{metadata:?}");

    // `want list` renders wants. A derived-task column was removed: it could
    // only ever be populated for a want that was never promoted, and computing
    // it read the whole event log once per want.
    let list = vivi(["want", "list", "--project", project_s, "--for", "cto"]);
    assert_success(&list);
    let text = stdout(&list);
    assert!(
        text.contains("handle  status  priority  rank  repo  lane  subject"),
        "{text}"
    );
    assert!(!text.contains("active_tasks"), "{text}");
    assert!(text.contains(&want), "{text}");
}

fn init_roster(project: &std::path::Path) {
    assert_success(&vivi([
        "mailspace",
        "init",
        "--project",
        project.to_str().unwrap(),
    ]));
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

fn send_work(project: &std::path::Path, kind: &str, to: &str, subject: &str, body: &str) -> String {
    let output = vivi([
        kind,
        "send",
        "--project",
        project.to_str().unwrap(),
        "--from",
        "ceo",
        "--to",
        to,
        "--subject",
        subject,
        "--body",
        body,
    ]);
    assert_success(&output);
    handle_after(&stdout(&output), &format!("created {to}"))
}

#[test]
fn role_cadence_and_schedule_status_from_outbound_mail() {
    let project = tempfile::tempdir().unwrap();
    let p = project.path().to_str().unwrap();
    assert_success(&vivi(["mailspace", "init", "--project", p]));
    assert_success(&vivi([
        "role",
        "add",
        "head-ceo",
        "--kind",
        "head",
        "--model",
        "glm-5.2",
        "--thinking",
        "high",
        "--cadence",
        "15m",
        "--project",
        p,
    ]));
    assert_success(&vivi([
        "role",
        "add",
        "mind",
        "--kind",
        "mind",
        "--project",
        p,
    ]));

    // No outbound signal yet → schedule never.
    let status = vivi(["role", "status", "head-ceo", "--json", "--project", p]);
    assert_success(&status);
    let v: Value = serde_json::from_str(&stdout(&status)).unwrap();
    assert_eq!(v["schedule"]["state"], "never");
    assert_eq!(v["schedule"]["cadence"], "15m");
    assert_eq!(v["schedule"]["cadence_seconds"], 900);
    assert!(v["schedule"]["next_due_at"].as_str().is_some(), "{v}");

    // Reject invalid cadence units.
    let bad = vivi(["role", "set", "head-ceo", "--cadence", "1w", "--project", p]);
    assert!(!bad.status.success(), "{}", stderr(&bad));

    // Outbound mail establishes a signal → ok (just now).
    assert_success(&vivi([
        "mail",
        "send",
        "--project",
        p,
        "--from",
        "head-ceo",
        "--to",
        "mind",
        "--subject",
        "cycle report",
        "--body",
        "done",
    ]));
    // `mailspace init` already wrote the `[pump]` table the pump state needs.
    let pump_home = project.path().join(".vivi/pump");
    std::fs::create_dir_all(&pump_home).unwrap();
    let mut pump_state = PumpState::default();
    let mut cadence = CadenceState::default();
    cadence.runs.insert(
        String::from("head-ceo"),
        RoleRun {
            job: String::from("cadence-head-ceo-1"),
            task: String::from("abcd1234"),
            read_only: true,
            started_at: 1_792_425_600,
            ended_at: Some(1_792_425_700),
            outcome: Some(String::from("success")),
            attempt: 0,
            retry_at: None,
        },
    );
    set_state(&mut pump_state, &cadence);
    let mut pump_store = PumpStore::open(&pump_home.join(DB_FILENAME)).unwrap();
    pump_store.save(&pump_state).unwrap();

    let status = vivi(["role", "status", "head-ceo", "--json", "--project", p]);
    assert_success(&status);
    let v: Value = serde_json::from_str(&stdout(&status)).unwrap();
    assert_eq!(v["schedule"]["state"], "ok");
    assert!(
        v["schedule"]["last_signal_handle"].as_str().is_some(),
        "{v}"
    );
    assert!(v["schedule"]["age_seconds"].as_u64().is_some(), "{v}");

    // Board surfaces schedule for roles with cadence.
    let board: Value = serde_json::from_str(&stdout(&vivi([
        "board",
        "--for",
        "head-ceo",
        "--json",
        "--project",
        p,
    ])))
    .unwrap();
    assert_eq!(board["identities"][0]["schedule"]["state"], "ok");
    assert_eq!(board["identities"][0]["schedule"]["cadence"], "15m");
    assert!(
        board["identities"][0]["schedule"]["next_due_at"]
            .as_str()
            .is_some(),
        "{board}"
    );
    assert!(
        board["identities"][0]["last_auto_run"]
            .as_str()
            .is_some_and(|run| run.contains("success")),
        "{board}"
    );
    assert_eq!(board["identities"][0]["model"], "glm-5.2");
    assert_eq!(board["identities"][0]["thinking"], "high");

    let text = stdout(&vivi(["board", "--for", "head-ceo", "--project", p]));
    assert!(text.contains("model: glm-5.2"), "{text}");
    assert!(text.contains("thinking: high"), "{text}");
    assert!(text.contains("schedule: ok"), "{text}");
    assert!(text.contains("cadence 15m"), "{text}");
    assert!(text.contains("next due"), "{text}");
    assert!(text.contains("last auto-run success"), "{text}");

    // Clear cadence → none.
    assert_success(&vivi([
        "role",
        "set",
        "head-ceo",
        "--clear-cadence",
        "--project",
        p,
    ]));
    let shown: Value = serde_json::from_str(&stdout(&vivi([
        "role",
        "show",
        "head-ceo",
        "--json",
        "--project",
        p,
    ])))
    .unwrap();
    assert_eq!(shown["cadence"], Value::Null);
    let status = vivi(["role", "status", "head-ceo", "--json", "--project", p]);
    assert_success(&status);
    let v: Value = serde_json::from_str(&stdout(&status)).unwrap();
    assert_eq!(v["schedule"]["state"], "none");
}

#[test]
fn role_status_reports_pid_binding_and_liveness() {
    let project = tempfile::tempdir().unwrap();
    assert_success(&vivi([
        "mailspace",
        "init",
        "--project",
        project.path().to_str().unwrap(),
    ]));
    assert_success(&vivi([
        "role",
        "add",
        "hand-1",
        "--kind",
        "hand",
        "--project",
        project.path().to_str().unwrap(),
    ]));
    let p = project.path().to_str().unwrap();

    // No binding set yet.
    let out = vivi(["role", "status", "hand-1", "--json", "--project", p]);
    assert_success(&out);
    let v: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(v["name"], "hand-1");
    assert_eq!(v["pid"], Value::Null);
    assert_eq!(v["status"]["state"], "not_set");
    assert_eq!(v["status"]["running"], Value::Null);

    // Self-register this test process's pid; host defaults to the local host.
    let pid = std::process::id();
    assert_success(&vivi([
        "role",
        "set",
        "hand-1",
        "--pid",
        &pid.to_string(),
        "--project",
        p,
    ]));
    let show = vivi(["role", "show", "hand-1", "--json", "--project", p]);
    assert_success(&show);
    let shown: Value = serde_json::from_str(&stdout(&show)).unwrap();
    assert_eq!(shown["pid"].as_u64(), Some(u64::from(pid)));
    assert!(shown["host"].as_str().is_some(), "{shown}");

    let out = vivi(["role", "status", "hand-1", "--json", "--project", p]);
    assert_success(&out);
    let v: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(v["status"]["state"], "alive");
    assert_eq!(v["status"]["running"], true);
    assert!(v["status"]["name"].as_str().is_some(), "{v}");
    assert!(v["status"]["memory_bytes"].as_u64().is_some(), "{v}");
    assert!(v["status"]["uptime_seconds"].as_u64().is_some(), "{v}");

    // A pid that does not exist reports dead. 4_000_000 is outside the macOS
    // pid space; this test runs on the dev host where it is reliably dead.
    assert_success(&vivi([
        "role",
        "set",
        "hand-1",
        "--pid",
        "4000000",
        "--project",
        p,
    ]));
    let out = vivi(["role", "status", "hand-1", "--json", "--project", p]);
    assert_success(&out);
    let v: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(v["status"]["state"], "dead");
    assert_eq!(v["status"]["running"], false);

    // A binding whose stored host differs from the local host is reported remote;
    // the local process table is not probed.
    assert_success(&vivi([
        "role",
        "set",
        "hand-1",
        "--pid",
        &pid.to_string(),
        "--host",
        "zzz-remote-host-not-local-999",
        "--project",
        p,
    ]));
    let out = vivi(["role", "status", "hand-1", "--json", "--project", p]);
    assert_success(&out);
    let v: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(v["status"]["state"], "remote");
    assert_eq!(v["status"]["running"], Value::Null);
    assert_eq!(v["host"], "zzz-remote-host-not-local-999");

    // Clearing the pid also clears the host (no binding without a pid).
    assert_success(&vivi([
        "role",
        "set",
        "hand-1",
        "--clear-pid",
        "--project",
        p,
    ]));
    let shown: Value = serde_json::from_str(&stdout(&vivi([
        "role",
        "show",
        "hand-1",
        "--json",
        "--project",
        p,
    ])))
    .unwrap();
    assert_eq!(shown["pid"], Value::Null);
    assert_eq!(shown["host"], Value::Null);

    // Subagent harness: a stored pid is not treated as a liveness signal.
    assert_success(&vivi([
        "role",
        "add",
        "hand-2",
        "--kind",
        "hand",
        "--harness",
        "subagent",
        "--project",
        p,
    ]));
    assert_success(&vivi([
        "role",
        "set",
        "hand-2",
        "--pid",
        "4000000",
        "--project",
        p,
    ]));
    let out = vivi(["role", "status", "hand-2", "--json", "--project", p]);
    assert_success(&out);
    let v: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(v["status"]["state"], "subagent");
    assert_eq!(v["status"]["running"], Value::Null);
}

#[test]
#[allow(clippy::too_many_lines)]
fn trace_command_reconstructs_reply_chain_and_collapses_copies() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());

    let mail = vivi([
        "mail",
        "send",
        "--project",
        project.path().to_str().unwrap(),
        "--from",
        "ceo",
        "--to",
        "cto",
        "--subject",
        "status: blocker review",
        "--body",
        "The release blocker is now assigned.",
    ]);
    assert_success(&mail);
    let mail_stdout = stdout(&mail);
    let delivered = handle_after(&mail_stdout, "delivered cto");

    let reply = vivi([
        "mail",
        "reply",
        &delivered,
        "--project",
        project.path().to_str().unwrap(),
        "--from",
        "cto",
        "--body",
        "I have reviewed the blocker.",
    ]);
    assert_success(&reply);
    let reply_handle = handle_after(&stdout(&reply), "replied ceo");

    let trace = vivi([
        "trace",
        &reply_handle,
        "--project",
        project.path().to_str().unwrap(),
    ]);
    assert_success(&trace);
    let trace_stdout = stdout(&trace);
    assert!(
        trace_stdout.contains("status: blocker review"),
        "{trace_stdout}"
    );
    assert!(
        trace_stdout.contains("ancestor ->") && trace_stdout.contains("captured"),
        "{trace_stdout}"
    );
    // The sent copy and inbox copy share a content_id and should collapse.
    assert!(trace_stdout.contains("copies:"), "{trace_stdout}");

    let trace_json = vivi([
        "trace",
        &reply_handle,
        "--project",
        project.path().to_str().unwrap(),
        "--json",
    ]);
    assert_success(&trace_json);
    let graph: Value = serde_json::from_str(&stdout(&trace_json)).unwrap();
    let nodes = graph["nodes"].as_array().unwrap();
    assert!(nodes.len() >= 2, "{graph}");
    let message_handles: std::collections::HashSet<&str> = nodes
        .iter()
        .flat_map(|node| node["messages"].as_array().unwrap())
        .filter_map(|message| message["handle"].as_str())
        .collect();
    assert!(message_handles.contains(reply_handle.as_str()), "{graph}");
    assert!(nodes.iter().any(|node| {
        node["edges"].as_array().unwrap().iter().any(|edge| {
            edge["direction"] == "ancestor"
                && edge["source"] == "captured"
                && edge["target"].is_string()
        })
    }));
}

#[test]
fn graph_import_show_idempotent_and_check() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let mermaid = project.path().join("wave.mmd");
    std::fs::write(
        &mermaid,
        r#"
flowchart LR
  subgraph mir["mir-swarm"]
    verify["G-P-10/U1/reverify"]
    accept["G-P-10/U1/accept"]
    u2["G-P-10/U2"]
    u3["G-P-10/U3"]
    verify --> accept
    accept --> u2
    accept --> u3
  end
"#,
    )
    .unwrap();
    let project_s = project.path().to_str().unwrap();
    let mermaid_s = mermaid.to_str().unwrap();

    let check = vivi([
        "graph",
        "import",
        "--code",
        "mir-swarm-wave-2",
        "--file",
        mermaid_s,
        "--check",
        "--json",
        "--project",
        project_s,
    ]);
    assert_success(&check);
    let check_v: Value = serde_json::from_str(&stdout(&check)).unwrap();
    assert_eq!(check_v["check_only"], true);
    assert_eq!(check_v["node_count"], 4);
    assert_eq!(check_v["ready"][0], "verify");
    assert!(
        check_v.get("nodes").is_none(),
        "import receipt is not topology"
    );
    assert!(
        check_v.get("edges").is_none(),
        "import receipt is not topology"
    );

    let missing = vivi(["graph", "show", "mir-swarm-wave-2", "--project", project_s]);
    assert!(!missing.status.success(), "check must not write graph");

    let import = vivi([
        "graph",
        "import",
        "--code",
        "mir-swarm-wave-2",
        "--file",
        mermaid_s,
        "--json",
        "--project",
        project_s,
    ]);
    assert_success(&import);
    let import_v: Value = serde_json::from_str(&stdout(&import)).unwrap();
    assert_eq!(import_v["created"], true);
    assert_eq!(import_v["revision"], 1);
    assert_eq!(import_v["edge_count"], 3);
    let handle = import_v["graph_handle"].as_str().unwrap().to_string();

    let again = vivi([
        "graph",
        "import",
        "--code",
        "mir-swarm-wave-2",
        "--file",
        mermaid_s,
        "--json",
        "--project",
        project_s,
    ]);
    assert_success(&again);
    let again_v: Value = serde_json::from_str(&stdout(&again)).unwrap();
    assert_eq!(again_v["idempotent"], true);
    assert_eq!(again_v["graph_handle"], handle);
    assert_eq!(again_v["revision"], 1);

    // graph show is Mermaid topology only (no --json).
    let show_mermaid = vivi(["graph", "show", "mir-swarm-wave-2", "--project", project_s]);
    assert_success(&show_mermaid);
    let mermaid_out = stdout(&show_mermaid);
    assert!(mermaid_out.contains("flowchart"), "{mermaid_out}");
    assert!(mermaid_out.contains("verify"), "{mermaid_out}");
    assert!(mermaid_out.contains("accept"), "{mermaid_out}");
    assert!(mermaid_out.contains("-->"), "{mermaid_out}");

    // Status loops use graph ready (compact frontier), not full topology JSON.
    let ready = vivi([
        "graph",
        "ready",
        "mir-swarm-wave-2",
        "--json",
        "--project",
        project_s,
    ]);
    assert_success(&ready);
    let ready_v: Value = serde_json::from_str(&stdout(&ready)).unwrap();
    assert_eq!(ready_v["handle"], handle);
    assert_eq!(ready_v["ready"].as_array().unwrap().len(), 1);
    assert_eq!(ready_v["ready"][0], "verify");
    assert_eq!(ready_v["blocked"].as_array().unwrap().len(), 3);
    assert!(ready_v.get("nodes").is_none());
    assert!(ready_v.get("edges").is_none());
}

#[test]
fn task_show_and_activate_pin_task_content() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let project_s = project.path().to_str().unwrap();
    let task = send_work(
        project.path(),
        "task",
        "cto",
        "pinned unit",
        "done_when: lands",
    );

    let show = vivi(["task", "show", &task, "--project", project_s]);
    assert_success(&show);
    let show_out = stdout(&show);
    assert!(show_out.contains("Content: "), "{show_out}");
    let show_pin = show_out
        .lines()
        .find_map(|l| l.strip_prefix("Content: "))
        .expect("content pin line");

    let mermaid = project.path().join("pin.mmd");
    std::fs::write(&mermaid, "flowchart TD\na[\"A\"]\n").unwrap();
    assert_success(&vivi([
        "graph",
        "import",
        "--code",
        "pin",
        "--file",
        mermaid.to_str().unwrap(),
        "--project",
        project_s,
    ]));
    let activate = vivi([
        "graph",
        "activate",
        "a",
        "--graph",
        "pin",
        "--task",
        &task,
        "--json",
        "--project",
        project_s,
    ]);
    assert_success(&activate);
    let activate_v: Value = serde_json::from_str(&stdout(&activate)).unwrap();
    let content = activate_v["content"].as_str().unwrap();
    assert_eq!(content.len(), 64, "{content}");
    assert!(content.chars().all(|c| c.is_ascii_hexdigit()), "{content}");
    assert_eq!(content, show_pin.trim());
}

#[test]
fn graph_import_rejects_cycle_without_writes() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let mermaid = project.path().join("cycle.mmd");
    std::fs::write(&mermaid, "flowchart TD\na --> b\nb --> a\n").unwrap();
    let project_s = project.path().to_str().unwrap();
    let out = vivi([
        "graph",
        "import",
        "--code",
        "bad-cycle",
        "--file",
        mermaid.to_str().unwrap(),
        "--project",
        project_s,
    ]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("cycle"), "{}", stderr(&out));
    let show = vivi(["graph", "show", "bad-cycle", "--project", project_s]);
    assert!(!show.status.success());
}

#[test]
fn graph_import_accepts_planning_vocabulary() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let project_s = project.path().to_str().unwrap();
    let mermaid = project.path().join("planning.mmd");
    std::fs::write(
        &mermaid,
        r#"
flowchart LR
  classDef done fill:#cfc,stroke:#393;
  work["unit"]
  dec40{"operator ruling"}
  stadium(["kick off"])
  res_provision["provision index"]:::stub
  open["not done"]
  stadium --> work
  work --> dec40
  dec40 -.-> res_provision
  open -.-> work
  class dec40 decision
"#,
    )
    .unwrap();
    let import = vivi([
        "graph",
        "import",
        "--code",
        "planning",
        "--file",
        mermaid.to_str().unwrap(),
        "--json",
        "--project",
        project_s,
    ]);
    assert_success(&import);
    let import_v: Value = serde_json::from_str(&stdout(&import)).unwrap();
    assert_eq!(import_v["node_count"], 5);
    assert_eq!(import_v["edge_count"], 4);
    let ready_list = import_v["ready"].as_array().unwrap();
    assert!(
        ready_list.contains(&Value::String("stadium".into())),
        "{ready_list:?}"
    );
    assert!(
        !ready_list.iter().any(|v| v == "res_provision"),
        "{ready_list:?}"
    );
    let gates = import_v["gates"].as_array().unwrap();
    assert_eq!(gates.len(), 1, "{gates:?}");
    assert_eq!(gates[0]["source_id"], "res_provision");
    assert_eq!(gates[0]["kind"], "stub");

    // Dotted couplings never gate readiness: work stays blocked only by the
    // solid stadium edge; the stub gates instead of dispatching; dec40 is
    // blocked by the solid work edge.
    let ready = vivi(["graph", "ready", "planning", "--project", project_s]);
    assert_success(&ready);
    let ready_out = stdout(&ready);
    assert!(ready_out.contains("gates"), "{ready_out}");
    assert!(ready_out.contains("res_provision (stub)"), "{ready_out}");
    assert!(ready_out.contains("dec40"), "{ready_out}");

    // Export round-trips the vocabulary.
    let exported = vivi(["graph", "export", "planning", "--project", project_s]);
    assert_success(&exported);
    let exported_mermaid = stdout(&exported);
    assert!(exported_mermaid.contains("dec40{"), "{exported_mermaid}");
    assert!(exported_mermaid.contains(":::stub"), "{exported_mermaid}");
    assert!(exported_mermaid.contains("-.->"), "{exported_mermaid}");
}

#[test]
fn graph_activate_refuses_decision_nodes() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let project_s = project.path().to_str().unwrap();
    let mermaid = project.path().join("gates.mmd");
    std::fs::write(
        &mermaid,
        "flowchart LR\nwork[\"unit\"]\ndec40{\"operator ruling\"}\n",
    )
    .unwrap();
    assert_success(&vivi([
        "graph",
        "import",
        "--code",
        "gates",
        "--file",
        mermaid.to_str().unwrap(),
        "--project",
        project_s,
    ]));
    let task = send_work(
        project.path(),
        "task",
        "cto",
        "probe",
        "done_when: probe lands",
    );
    let activate = vivi([
        "graph",
        "activate",
        "dec40",
        "--graph",
        "gates",
        "--task",
        &task,
        "--project",
        project_s,
    ]);
    assert!(!activate.status.success());
    let activate_err = stderr(&activate);
    assert!(activate_err.contains("decision"), "{activate_err}");
    assert!(activate_err.contains("graph complete"), "{activate_err}");

    let complete = vivi([
        "graph",
        "complete",
        "dec40",
        "--graph",
        "gates",
        "--note",
        "ruling: proceed",
        "--project",
        project_s,
    ]);
    assert_success(&complete);
}

#[test]
fn graph_complete_ignores_task_flag_with_hint() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let project_s = project.path().to_str().unwrap();
    let mermaid = project.path().join("tiny.mmd");
    std::fs::write(&mermaid, "flowchart TD\na[\"A\"]\nb[\"B\"]\na --> b\n").unwrap();
    assert_success(&vivi([
        "graph",
        "import",
        "--code",
        "tiny",
        "--file",
        mermaid.to_str().unwrap(),
        "--project",
        project_s,
    ]));
    let out = vivi([
        "graph",
        "complete",
        "a",
        "--graph",
        "tiny",
        "--task",
        "whatever",
        "--project",
        project_s,
    ]);
    assert_success(&out);
    let err = stderr(&out);
    assert!(err.contains("--task is ignored"), "{err}");
    assert!(err.contains("graph activate"), "{err}");
}

#[test]
fn task_send_without_done_when_warns_on_stderr() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let p = project.path().to_str().unwrap();
    let vague = vivi([
        "task",
        "send",
        "--project",
        p,
        "--from",
        "ceo",
        "--to",
        "cto",
        "--subject",
        "vague work",
        "--body",
        "just do it",
    ]);
    assert_success(&vague);
    assert!(
        stderr(&vague).contains("no_done_when"),
        "{}",
        stderr(&vague)
    );

    let clear = vivi([
        "task",
        "send",
        "--project",
        p,
        "--from",
        "ceo",
        "--to",
        "cto",
        "--subject",
        "clear work",
        "--body",
        "done_when: it works",
    ]);
    assert_success(&clear);
    assert!(
        !stderr(&clear).contains("no_done_when"),
        "{}",
        stderr(&clear)
    );

    // Wants stay quiet: they are parked by design.
    let want = vivi([
        "want",
        "send",
        "--project",
        p,
        "--from",
        "ceo",
        "--to",
        "ceo",
        "--subject",
        "someday",
        "--body",
        "someday",
    ]);
    assert_success(&want);
    assert!(!stderr(&want).contains("no_done_when"), "{}", stderr(&want));
}

#[test]
fn graph_apply_complete_and_export() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let project_s = project.path().to_str().unwrap();
    let base = project.path().join("base.mmd");
    std::fs::write(
        &base,
        "flowchart LR\nverify[\"v\"]\naccept[\"a\"]\nverify --> accept\n",
    )
    .unwrap();
    assert_success(&vivi([
        "graph",
        "import",
        "--code",
        "wave",
        "--file",
        base.to_str().unwrap(),
        "--project",
        project_s,
    ]));

    let complete = vivi([
        "graph",
        "complete",
        "wave:verify",
        "--json",
        "--project",
        project_s,
    ]);
    assert_success(&complete);
    let completed: Value = serde_json::from_str(&stdout(&complete)).unwrap();
    assert_eq!(completed["action"], "complete");
    assert_eq!(completed["node"], "verify");
    assert_eq!(completed["ready"][0], "accept");
    assert!(completed.get("nodes").is_none());

    let expanded = project.path().join("expanded.mmd");
    std::fs::write(
        &expanded,
        "flowchart LR\nverify[\"v\"]\naccept[\"a\"]\nu2[\"U2\"]\nverify --> accept\naccept --> u2\n",
    )
    .unwrap();
    let apply = vivi([
        "graph",
        "apply",
        "wave",
        "--file",
        expanded.to_str().unwrap(),
        "--json",
        "--project",
        project_s,
    ]);
    assert_success(&apply);
    let applied: Value = serde_json::from_str(&stdout(&apply)).unwrap();
    assert_eq!(applied["revision"], 2);
    assert!(
        applied["nodes_added"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n == "u2"),
        "{applied}"
    );

    let export = vivi([
        "graph",
        "export",
        "wave",
        "--include-state",
        "--project",
        project_s,
    ]);
    assert_success(&export);
    let mermaid = stdout(&export);
    assert!(mermaid.contains("flowchart"), "{mermaid}");
    assert!(mermaid.contains("verify"), "{mermaid}");
    assert!(mermaid.contains("classDef"), "{mermaid}");

    // complete accept so nothing blocked on verify; re-import ready frontier for board
    assert_success(&vivi([
        "graph",
        "complete",
        "wave:accept",
        "--project",
        project_s,
    ]));
    let board = vivi(["board", "--graph", "--json", "--project", project_s]);
    assert_success(&board);
    let board_v: Value = serde_json::from_str(&stdout(&board)).unwrap();
    assert!(board_v["identities"].is_array(), "{board_v}");
    let graphs = board_v["graphs"].as_array().expect("graphs present");
    assert!(!graphs.is_empty(), "{board_v}");
    assert_eq!(graphs[0]["code"], "wave");
    assert!(
        graphs[0]["nodes"].as_array().unwrap().len() >= 2,
        "{board_v}"
    );
    assert!(graphs[0]["nodes"][0]["blocked_by"].is_array());
    assert!(graphs[0]["nodes"][0]["successors"].is_array());
}

#[test]
fn graph_activate_binds_ready_task_and_refuses_blocked() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let project_s = project.path().to_str().unwrap();
    let mermaid = project.path().join("g.mmd");
    std::fs::write(&mermaid, "flowchart LR\na[\"A\"]\nb[\"B\"]\na --> b\n").unwrap();
    assert_success(&vivi([
        "graph",
        "import",
        "--code",
        "lane",
        "--file",
        mermaid.to_str().unwrap(),
        "--project",
        project_s,
    ]));
    let task = vivi([
        "task",
        "send",
        "--project",
        project_s,
        "--from",
        "ceo",
        "--to",
        "cto",
        "--subject",
        "do A",
        "--body",
        "work",
    ]);
    assert_success(&task);
    let task_handle = handle_after(&stdout(&task), "created cto");

    let blocked = vivi([
        "graph",
        "activate",
        "lane:b",
        "--task",
        &task_handle,
        "--project",
        project_s,
    ]);
    assert!(!blocked.status.success());
    assert!(stderr(&blocked).contains("blocked"), "{}", stderr(&blocked));

    let activate = vivi([
        "graph",
        "activate",
        "lane:a",
        "--task",
        &task_handle,
        "--json",
        "--project",
        project_s,
    ]);
    assert_success(&activate);
    let receipt: Value = serde_json::from_str(&stdout(&activate)).unwrap();
    assert_eq!(receipt["action"], "activate");
    assert_eq!(receipt["node"], "a");
    assert_eq!(receipt["task"], task_handle);
    assert!(
        receipt["active"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n == "a"),
        "{receipt}"
    );
    assert!(receipt.get("nodes").is_none());
}

#[test]
fn backlog_citizenship_mints_unlocks_and_never_promotes() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let project_s = project.path().to_str().unwrap();

    let dep_output = vivi([
        "need",
        "send",
        "--project",
        project_s,
        "--from",
        "ceo",
        "--to",
        "cto",
        "--subject",
        "auth fix",
        "--body",
        "must land",
    ]);
    assert_success(&dep_output);
    let dep = handle_after(&stdout(&dep_output), "created cto");

    let want_output = vivi([
        "want",
        "send",
        "--project",
        project_s,
        "--from",
        "ceo",
        "--to",
        "cto",
        "--subject",
        "retry helper",
        "--body",
        "later",
        "--depends-on",
        &dep,
    ]);
    assert_success(&want_output);
    let want = handle_after(&stdout(&want_output), "created cto");

    let node_readiness = |handle: &str| -> String {
        let board = vivi(["board", "--graph", "--json", "--project", project_s]);
        assert_success(&board);
        let board_v: Value = serde_json::from_str(&stdout(&board)).unwrap();
        let graphs = board_v["graphs"].as_array().expect("graphs present");
        let backlog = graphs
            .iter()
            .find(|g| g["code"] == "backlog")
            .unwrap_or_else(|| panic!("no backlog graph: {board_v}"));
        backlog["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["source_id"] == handle)
            .unwrap_or_else(|| panic!("node {handle} missing: {backlog}"))["readiness"]
            .as_str()
            .unwrap()
            .to_string()
    };

    assert_eq!(node_readiness(&want), "blocked");
    assert_eq!(node_readiness(&dep), "ready");

    // The author accepts: an assignee's `done` is only a report.
    let done = vivi(["need", "done", "--project", project_s, "--for", "ceo", &dep]);
    assert_success(&done);

    assert_eq!(node_readiness(&want), "ready");

    // Unlocking never promotes: the item is still listed as a want, not a need.
    let wants = vivi(["want", "list", "--project", project_s, "--for", "cto"]);
    assert_success(&wants);
    assert!(stdout(&wants).contains(&want), "{}", stdout(&wants));
    let needs = vivi(["need", "list", "--project", project_s, "--for", "cto"]);
    assert_success(&needs);
    assert!(!stdout(&needs).contains(&want), "{}", stdout(&needs));
}

#[test]
fn task_depends_on_task_unlocks_via_graph() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let project_s = project.path().to_str().unwrap();

    let first_output = vivi([
        "task",
        "send",
        "--project",
        project_s,
        "--from",
        "ceo",
        "--to",
        "cto",
        "--subject",
        "unit A",
        "--body",
        "work",
    ]);
    assert_success(&first_output);
    let first = handle_after(&stdout(&first_output), "created cto");

    let second_output = vivi([
        "task",
        "send",
        "--project",
        project_s,
        "--from",
        "ceo",
        "--to",
        "cto",
        "--subject",
        "unit B",
        "--body",
        "work",
        "--depends-on",
        &first,
    ]);
    assert_success(&second_output);
    let second = handle_after(&stdout(&second_output), "created cto");

    let readiness = |handle: &str| -> String {
        let board = vivi(["board", "--graph", "--json", "--project", project_s]);
        assert_success(&board);
        let board_v: Value = serde_json::from_str(&stdout(&board)).unwrap();
        board_v["graphs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["code"] == "backlog")
            .unwrap_or_else(|| panic!("no backlog graph: {board_v}"))["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["source_id"] == handle)
            .unwrap_or_else(|| panic!("node {handle} missing: {board_v}"))["readiness"]
            .as_str()
            .unwrap()
            .to_string()
    };

    assert_eq!(readiness(&first), "ready");
    assert_eq!(readiness(&second), "blocked");

    let done = vivi([
        "task",
        "done",
        "--project",
        project_s,
        "--for",
        "ceo",
        &first,
    ]);
    assert_success(&done);
    assert_eq!(readiness(&second), "ready");

    // An unknown dependency handle fails the send without creating anything.
    let bad = vivi([
        "task",
        "send",
        "--project",
        project_s,
        "--from",
        "ceo",
        "--to",
        "cto",
        "--subject",
        "bad dep",
        "--body",
        "work",
        "--depends-on",
        "deadbeef",
    ]);
    assert!(!bad.status.success());
    assert!(stderr(&bad).contains("deadbeef"), "{}", stderr(&bad));
}

#[test]
fn need_bind_joins_completion_via_cli() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let project_s = project.path().to_str().unwrap();

    let send = |kind: &str, subject: &str| -> String {
        let output = vivi([
            kind,
            "send",
            "--project",
            project_s,
            "--from",
            "ceo",
            "--to",
            "cto",
            "--subject",
            subject,
            "--body",
            "work",
        ]);
        assert_success(&output);
        handle_after(&stdout(&output), "created cto")
    };

    let need = send("need", "lower me");
    let first = send("task", "unit 1");
    let second = send("task", "unit 2");

    let bind = vivi([
        "need",
        "bind",
        "--project",
        project_s,
        &need,
        &first,
        &second,
    ]);
    assert_success(&bind);

    let node_state = |handle: &str| -> String {
        let board = vivi(["board", "--graph", "--json", "--project", project_s]);
        assert_success(&board);
        let board_v: Value = serde_json::from_str(&stdout(&board)).unwrap();
        board_v["graphs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["code"] == "backlog")
            .unwrap_or_else(|| panic!("no backlog graph: {board_v}"))["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["source_id"] == handle)
            .unwrap_or_else(|| panic!("node {handle} missing: {board_v}"))["state"]
            .as_str()
            .unwrap()
            .to_string()
    };

    assert_eq!(node_state(&need), "open");

    let done_one = vivi([
        "task",
        "done",
        "--project",
        project_s,
        "--for",
        "ceo",
        &first,
    ]);
    assert_success(&done_one);
    assert_eq!(node_state(&need), "open");

    let done_two = vivi([
        "task",
        "done",
        "--project",
        project_s,
        "--for",
        "ceo",
        &second,
    ]);
    assert_success(&done_two);
    assert_eq!(node_state(&need), "done");
}

#[test]
fn step_manifest_reports_dispatches_and_exceptions() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let project_s = project.path().to_str().unwrap();

    let task_output = vivi([
        "task",
        "send",
        "--project",
        project_s,
        "--from",
        "ceo",
        "--to",
        "cto",
        "--subject",
        "unit A",
        "--body",
        "unit: U-1\ndone_when: malformed input rejected\nwrite_scope: src/validate.rs",
    ]);
    assert_success(&task_output);
    let task = handle_after(&stdout(&task_output), "created cto");

    let want_output = vivi([
        "want",
        "send",
        "--project",
        project_s,
        "--from",
        "ceo",
        "--to",
        "cto",
        "--subject",
        "later work",
        "--body",
        "done_when: exists",
    ]);
    assert_success(&want_output);
    let want = handle_after(&stdout(&want_output), "created cto");

    let step = vivi(["step", "--project", project_s, "--json"]);
    assert_success(&step);
    let manifest: Value = serde_json::from_str(&stdout(&step)).unwrap();
    assert_eq!(manifest["graph"], "backlog");
    assert!(manifest["decisions"].as_array().unwrap().is_empty());

    let dispatches = manifest["dispatches"].as_array().unwrap();
    let dispatch = dispatches
        .iter()
        .find(|d| d["item"] == task)
        .unwrap_or_else(|| panic!("no dispatch for {task}: {manifest}"));
    assert_eq!(dispatch["kind"], "task");
    assert_eq!(dispatch["done_when_clauses"], 1);
    assert_eq!(dispatch["write_scope"], true);

    let exceptions = manifest["exceptions"].as_array().unwrap();
    let exception = exceptions
        .iter()
        .find(|e| e["item"] == want)
        .unwrap_or_else(|| panic!("no exception for {want}: {manifest}"));
    assert_eq!(exception["reason"], "want_requires_promotion");

    let text = vivi(["step", "--project", project_s]);
    assert_success(&text);
    assert!(stdout(&text).contains("unit A"), "{}", stdout(&text));
    assert!(
        stdout(&text).contains("want_requires_promotion"),
        "{}",
        stdout(&text)
    );
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

fn vivi_with_stdin<I, S>(args: I, stdin: &str) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let mut child = Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
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

#[test]
fn graph_activate_accepts_bare_backlog_handles() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let project_s = project.path().to_str().unwrap();
    let task = send_work(project.path(), "task", "cto", "bare handle", "work");

    let activate = vivi([
        "graph",
        "activate",
        &task,
        "--task",
        &task,
        "--project",
        project_s,
    ]);
    assert_success(&activate);
    let receipt: Value = serde_json::from_str(&stdout(&vivi([
        "graph",
        "ready",
        "backlog",
        "--json",
        "--project",
        project_s,
    ])))
    .unwrap();
    assert!(
        receipt["active"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n.as_str() == Some(task.as_str())),
        "{receipt}"
    );

    // An unknown bare handle names the backlog graph in its error.
    let missing = vivi(["graph", "complete", "deadbee0", "--project", project_s]);
    assert!(!missing.status.success());
    assert!(
        stderr(&missing).contains("deadbee0"),
        "{}",
        stderr(&missing)
    );
}

#[test]
fn delivered_work_is_tracked_without_repair() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let project_s = project.path().to_str().unwrap();
    // A tracked send creates the backlog graph.
    send_work(
        project.path(),
        "task",
        "cto",
        "tracked unit",
        "done_when: x",
    );

    // An .eml delivered straight into tasks is tracked like any other work:
    // the backlog node is minted at ingest.
    let identity_list = stdout(&vivi([
        "mailspace",
        "identity",
        "list",
        "--project",
        project_s,
    ]));
    let cto_addr = identity_list
        .lines()
        .find_map(|l| l.trim_start().strip_prefix("cto "))
        .expect("cto address")
        .trim()
        .to_string();
    let ceo_addr = identity_list
        .lines()
        .find_map(|l| l.trim_start().strip_prefix("ceo "))
        .expect("ceo address")
        .trim()
        .to_string();
    let eml_path = project.path().join("untracked.eml");
    std::fs::write(
        &eml_path,
        format!(
            "From: {ceo_addr}\r\nTo: {cto_addr}\r\nSubject: untracked unit\r\nX-Vivi-Kind: \
             task\r\n\r\ndone_when: y\r\n"
        ),
    )
    .unwrap();
    assert_success(&vivi([
        "mail",
        "deliver",
        eml_path.to_str().unwrap(),
        "--folder",
        "tasks",
        "--project",
        project_s,
    ]));
    let task_list = stdout(&vivi([
        "task",
        "list",
        "--for",
        "cto",
        "--project",
        project_s,
    ]));
    let untracked = task_list
        .lines()
        .find(|l| l.contains("untracked unit"))
        .and_then(|l| l.split_whitespace().next())
        .expect("untracked handle")
        .to_string();

    let audit = vivi(["graph", "audit", "--project", project_s]);
    assert_success(&audit);
    let audit_out = stdout(&audit);
    assert!(audit_out.contains("missing=0"), "{audit_out}");
    assert!(!audit_out.contains("missing_node"), "{audit_out}");

    assert_success(&vivi([
        "graph",
        "activate",
        &untracked,
        "--task",
        &untracked,
        "--project",
        project_s,
    ]));

    let after = vivi(["graph", "audit", "--project", project_s]);
    assert_success(&after);
    assert!(stdout(&after).contains("missing=0"), "{}", stdout(&after));
}

#[test]
fn graph_connect_adds_post_hoc_prerequisite() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let project_s = project.path().to_str().unwrap();
    let prereq = send_work(project.path(), "task", "cto", "prereq unit", "done_when: x");
    let dependent = send_work(
        project.path(),
        "task",
        "cto",
        "dependent unit",
        "done_when: y",
    );

    assert_success(&vivi([
        "graph",
        "connect",
        &dependent,
        &prereq,
        "--project",
        project_s,
    ]));

    let ready: Value = serde_json::from_str(&stdout(&vivi([
        "graph",
        "ready",
        "backlog",
        "--json",
        "--project",
        project_s,
    ])))
    .unwrap();
    let ready_list = ready["ready"].as_array().unwrap();
    assert!(
        !ready_list
            .iter()
            .any(|n| n.as_str() == Some(dependent.as_str())),
        "{ready}"
    );

    // Settling the prerequisite unlocks the dependent.
    assert_success(&vivi([
        "task",
        "done",
        "--for",
        "ceo",
        &prereq,
        "--project",
        project_s,
    ]));
    let unlocked: Value = serde_json::from_str(&stdout(&vivi([
        "graph",
        "ready",
        "backlog",
        "--json",
        "--project",
        project_s,
    ])))
    .unwrap();
    assert!(
        unlocked["ready"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n.as_str() == Some(dependent.as_str())),
        "{unlocked}"
    );
}

#[test]
fn need_bind_join_settles_need_item_and_lists_units() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let project_s = project.path().to_str().unwrap();
    let need = send_work(
        project.path(),
        "need",
        "cto",
        "lowered outcome",
        "done_when: z",
    );
    let unit = send_work(
        project.path(),
        "task",
        "cto",
        "unit for need",
        "done_when: w",
    );

    assert_success(&vivi([
        "need",
        "bind",
        &need,
        &unit,
        "--project",
        project_s,
    ]));

    let show = stdout(&vivi(["need", "show", &need, "--project", project_s]));
    assert!(show.contains("units"), "{show}");
    assert!(show.contains(&unit), "{show}");

    // Settling the unit must close the need item itself, not only its node.
    assert_success(&vivi([
        "task",
        "done",
        "--for",
        "ceo",
        &unit,
        "--project",
        project_s,
    ]));
    let open = stdout(&vivi([
        "need",
        "list",
        "--for",
        "cto",
        "--status",
        "open",
        "--project",
        project_s,
    ]));
    assert!(!open.contains("lowered outcome"), "{open}");
    let done = stdout(&vivi([
        "need",
        "list",
        "--for",
        "cto",
        "--status",
        "done",
        "--project",
        project_s,
    ]));
    assert!(done.contains("lowered outcome"), "{done}");
}

#[test]
fn cross_identity_close_names_the_holder() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let project_s = project.path().to_str().unwrap();
    assert_success(&vivi([
        "mailspace",
        "identity",
        "add",
        "cfo",
        "--project",
        project_s,
    ]));
    let task = send_work(project.path(), "task", "cto", "routed unit", "done_when: x");

    // `cfo` neither authored the task nor is a mind: it may only report,
    // and the refusal says through whom and who may close it.
    let out = vivi([
        "task",
        "done",
        "--for",
        "cfo",
        &task,
        "--project",
        project_s,
    ]);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(err.contains("held by identity 'cto'"), "{err}");
    assert!(err.contains("--for cto"), "{err}");
    assert!(err.contains("its author 'ceo'"), "{err}");

    // `ceo` authored it: the delivered handle works for the author.
    let out = vivi([
        "task",
        "done",
        "--for",
        "ceo",
        &task,
        "--project",
        project_s,
    ]);
    assert_success(&out);
    assert!(
        stdout(&out).starts_with(&format!("done {task}; event ")),
        "{}",
        stdout(&out)
    );
}

#[test]
fn duplicate_work_send_warns_on_open_same_subject() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let project_s = project.path().to_str().unwrap();
    send_work(project.path(), "want", "cto", "same want", "note: x");
    let dup = vivi([
        "want",
        "send",
        "--project",
        project_s,
        "--from",
        "ceo",
        "--to",
        "cto",
        "--subject",
        "same want",
        "--body",
        "note: x",
    ]);
    assert_success(&dup);
    assert!(
        stderr(&dup).contains("may be a duplicate"),
        "{}",
        stderr(&dup)
    );
}

#[test]
fn graph_ready_prints_counts_and_filters_by_kind() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let project_s = project.path().to_str().unwrap();
    send_work(project.path(), "task", "cto", "a task", "done_when: x");
    send_work(project.path(), "need", "cto", "a need", "done_when: y");

    let ready = stdout(&vivi(["graph", "ready", "backlog", "--project", project_s]));
    assert!(ready.contains("counts   ready="), "{ready}");

    let tasks_only: Value = serde_json::from_str(&stdout(&vivi([
        "graph",
        "ready",
        "backlog",
        "--kind",
        "need",
        "--json",
        "--project",
        project_s,
    ])))
    .unwrap();
    let subjects = tasks_only["ready"].as_array().unwrap().len();
    assert_eq!(subjects, 1, "{tasks_only}");

    let bad = vivi([
        "graph",
        "ready",
        "backlog",
        "--kind",
        "spam",
        "--project",
        project_s,
    ]);
    assert!(!bad.status.success());
}

#[test]
fn status_reports_vivi_version() {
    let project = tempfile::tempdir().unwrap();
    init_roster(project.path());
    let out = stdout(&vivi([
        "mailspace",
        "status",
        "--project",
        project.path().to_str().unwrap(),
    ]));
    assert!(out.contains("vivi      "), "{out}");
    let json: Value = serde_json::from_str(&stdout(&vivi([
        "mailspace",
        "status",
        "--json",
        "--project",
        project.path().to_str().unwrap(),
    ])))
    .unwrap();
    assert!(!json["vivi"].as_str().unwrap().is_empty());
}
