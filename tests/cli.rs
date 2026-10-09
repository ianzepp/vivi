use std::path::PathBuf;

use clap::Parser;
use vivi::cli::{
    ChatSubcommand, Cli, Command, CycleCommand, MailAbsorbStatus, MailCommand, MailspaceCommand,
    MailspaceIdentityCommand, MemoCommand, NeedCommand, NeedSendCommand, TaskCommand,
    TaskDumpStatusArg, TaskSendCommand, TaskStatus, WantCommand, WantSendCommand,
};

#[test]
fn parses_chat_status() {
    let cli = Cli::try_parse_from(["vivi", "chat", "status", "--project", "/tmp/demo"]).unwrap();
    let Command::Chat {
        command: ChatSubcommand::Status { project },
    } = cli.command
    else {
        panic!("expected chat status");
    };
    assert_eq!(project, Some(PathBuf::from("/tmp/demo")));
}

#[test]
fn parses_mailspace_identity_add() {
    let cli = Cli::try_parse_from(["vivi", "mailspace", "identity", "add", "cto"]).unwrap();

    match cli.command {
        Command::Mailspace {
            command:
                MailspaceCommand::Identity {
                    command: MailspaceIdentityCommand::Add { identity, project },
                },
        } => {
            assert_eq!(identity, "cto");
            assert_eq!(project, None);
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn parses_mailspace_archive_set_and_clear() {
    let set = Cli::try_parse_from(["vivi", "mailspace", "archive", "--set", "../vivi"]).unwrap();
    match set.command {
        Command::Mailspace {
            command: MailspaceCommand::Archive { set, clear, .. },
        } => {
            assert_eq!(set.as_deref(), Some("../vivi"));
            assert!(!clear);
        }
        other => panic!("unexpected command: {other:?}"),
    }

    let clear = Cli::try_parse_from(["vivi", "mailspace", "archive", "--clear"]).unwrap();
    match clear.command {
        Command::Mailspace {
            command: MailspaceCommand::Archive { set, clear, .. },
        } => {
            assert!(set.is_none());
            assert!(clear);
        }
        other => panic!("unexpected command: {other:?}"),
    }

    let export = Cli::try_parse_from(["vivi", "mailspace", "archive", "export"]).unwrap();
    match export.command {
        Command::Mailspace {
            command:
                MailspaceCommand::Archive {
                    command: Some(vivi::cli::MailspaceArchiveCommand::Export { json, .. }),
                    ..
                },
        } => {
            assert!(!json);
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn parses_role_add_set_and_charter() {
    use vivi::cli::{RoleCharterCommand, RoleCommand};

    let add = Cli::try_parse_from([
        "vivi",
        "role",
        "add",
        "head-ceo",
        "--kind",
        "head",
        "--harness",
        "subagent",
        "--label",
        "executive",
    ])
    .unwrap();
    match add.command {
        Command::Role {
            command:
                RoleCommand::Add {
                    name,
                    kind,
                    harness,
                    labels,
                    ..
                },
        } => {
            assert_eq!(name, "head-ceo");
            assert_eq!(kind.as_deref(), Some("head"));
            assert_eq!(harness.as_deref(), Some("subagent"));
            assert_eq!(labels, vec!["executive".to_string()]);
        }
        other => panic!("unexpected command: {other:?}"),
    }

    let set = Cli::try_parse_from([
        "vivi",
        "role",
        "set",
        "hand-1",
        "--provider",
        "zai",
        "--model",
        "glm-5.2",
        "--thinking",
        "low",
    ])
    .unwrap();
    match set.command {
        Command::Role {
            command:
                RoleCommand::Set {
                    name,
                    provider,
                    model,
                    thinking,
                    ..
                },
        } => {
            assert_eq!(name, "hand-1");
            assert_eq!(provider.as_deref(), Some("zai"));
            assert_eq!(model.as_deref(), Some("glm-5.2"));
            assert_eq!(thinking.as_deref(), Some("low"));
        }
        other => panic!("unexpected command: {other:?}"),
    }

    let charter = Cli::try_parse_from([
        "vivi",
        "role",
        "charter",
        "set",
        "head-ceo",
        "--file",
        "personas/ceo.md",
    ])
    .unwrap();
    match charter.command {
        Command::Role {
            command:
                RoleCommand::Charter {
                    command: RoleCharterCommand::Set { name, file, .. },
                },
        } => {
            assert_eq!(name, "head-ceo");
            assert_eq!(
                file.as_ref().map(|p| p.to_string_lossy().into_owned()),
                Some("personas/ceo.md".into())
            );
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn parses_role_set_pid_host_and_status() {
    use vivi::cli::RoleCommand;

    let set = Cli::try_parse_from([
        "vivi", "role", "set", "hand-1", "--pid", "12345", "--host", "pharos",
    ])
    .unwrap();
    match set.command {
        Command::Role {
            command:
                RoleCommand::Set {
                    name,
                    pid,
                    host,
                    clear_pid,
                    clear_host,
                    ..
                },
        } => {
            assert_eq!(name, "hand-1");
            assert_eq!(pid, Some(12_345));
            assert_eq!(host.as_deref(), Some("pharos"));
            assert!(!clear_pid);
            assert!(!clear_host);
        }
        other => panic!("unexpected command: {other:?}"),
    }

    let clear = Cli::try_parse_from(["vivi", "role", "set", "hand-1", "--clear-pid"]).unwrap();
    match clear.command {
        Command::Role {
            command: RoleCommand::Set {
                name, clear_pid, ..
            },
        } => {
            assert_eq!(name, "hand-1");
            assert!(clear_pid);
        }
        other => panic!("unexpected command: {other:?}"),
    }

    let status = Cli::try_parse_from(["vivi", "role", "status", "hand-1", "--json"]).unwrap();
    match status.command {
        Command::Role {
            command: RoleCommand::Status { name, json, .. },
        } => {
            assert_eq!(name, "hand-1");
            assert!(json);
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn parses_mailspace_import_dry_run() {
    let cli = Cli::try_parse_from([
        "vivi",
        "mailspace",
        "import",
        "--project",
        "/tmp/target",
        "--from",
        "/tmp/source",
        "--dry-run",
        "--json",
    ])
    .unwrap();

    match cli.command {
        Command::Mailspace {
            command: MailspaceCommand::Import(command),
        } => {
            assert_eq!(
                command.project.unwrap(),
                std::path::PathBuf::from("/tmp/target")
            );
            assert_eq!(command.from, std::path::PathBuf::from("/tmp/source"));
            assert!(command.dry_run);
            assert!(command.json);
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn parses_mailspace_merge_compatibility_alias() {
    let cli = Cli::try_parse_from([
        "vivi",
        "mailspace",
        "merge",
        "--from",
        "/tmp/source",
        "--dry-run",
    ])
    .unwrap();

    match cli.command {
        Command::Mailspace {
            command: MailspaceCommand::Merge(command),
        } => {
            assert_eq!(command.from, std::path::PathBuf::from("/tmp/source"));
            assert!(command.dry_run);
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn parses_board_command() {
    let cli = Cli::try_parse_from([
        "vivi",
        "board",
        "--for",
        "cto",
        "--wants",
        "3",
        "--since",
        "1h",
        "--watermark-file",
        "/tmp/board.watermark",
        "--write-watermark",
        "--json",
        "--project",
        "/tmp/project",
    ])
    .unwrap();

    match cli.command {
        Command::Board(command) => {
            assert_eq!(command.for_identity.as_deref(), Some("cto"));
            assert_eq!(command.wants, 3);
            assert_eq!(command.since.as_deref(), Some("1h"));
            assert_eq!(
                command.watermark_file,
                Some(PathBuf::from("/tmp/board.watermark"))
            );
            assert!(command.write_watermark);
            assert!(command.json);
            assert_eq!(command.project, Some(PathBuf::from("/tmp/project")));
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn parses_board_with_global_project_before_subcommand() {
    let cli = Cli::try_parse_from([
        "vivi",
        "--project",
        "/tmp/project",
        "board",
        "--for",
        "mind",
        "--json",
    ])
    .unwrap();

    assert_eq!(cli.project, Some(PathBuf::from("/tmp/project")));
    match cli.command {
        Command::Board(command) => {
            assert_eq!(command.for_identity.as_deref(), Some("mind"));
            assert!(command.json);
            // clap global --project also fills the board-local project field
            assert_eq!(command.project, Some(PathBuf::from("/tmp/project")));
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn parses_task_list_with_global_project_before_subcommand() {
    let cli = Cli::try_parse_from([
        "vivi",
        "--project",
        "/tmp/project",
        "task",
        "list",
        "--for",
        "hand-1",
        "--status",
        "open",
    ])
    .unwrap();

    assert_eq!(cli.project, Some(PathBuf::from("/tmp/project")));
    match cli.command {
        Command::Task {
            command:
                TaskCommand::List {
                    for_identity,
                    status,
                    project,
                    ..
                },
        } => {
            assert_eq!(for_identity.as_deref(), Some("hand-1"));
            assert!(matches!(status, TaskStatus::Open));
            assert_eq!(project, Some(PathBuf::from("/tmp/project")));
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn parses_local_mail_send() {
    let cli = Cli::try_parse_from([
        "vivi",
        "mail",
        "send",
        "--from",
        "ceo",
        "--to",
        "cto",
        "--subject",
        "review",
        "--body",
        "please review",
    ])
    .unwrap();

    match cli.command {
        Command::Mail {
            command: MailCommand::Send(command),
        } => {
            assert_eq!(command.from, "ceo");
            assert_eq!(command.to, vec!["cto"]);
            assert_eq!(command.subject, "review");
            assert_eq!(command.body.as_deref(), Some("please review"));
            assert_eq!(command.body_file, None);
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn parses_local_mail_send_body_file() {
    let cli = Cli::try_parse_from([
        "vivi",
        "mail",
        "send",
        "--from",
        "ceo",
        "--to",
        "cto",
        "--subject",
        "review",
        "--body-file",
        "body.md",
    ])
    .unwrap();

    match cli.command {
        Command::Mail {
            command: MailCommand::Send(command),
        } => {
            assert_eq!(command.body, None);
            assert_eq!(command.body_file, Some(PathBuf::from("body.md")));
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn parses_local_mail_list_with_json_and_project() {
    let cli = Cli::try_parse_from([
        "vivi",
        "mail",
        "list",
        "--for",
        "mind",
        "--folder",
        "inbox",
        "--json",
        "--project",
        "/tmp/project",
    ])
    .unwrap();

    match cli.command {
        Command::Mail {
            command: MailCommand::List(command),
        } => {
            assert_eq!(command.for_identity.as_deref(), Some("mind"));
            assert_eq!(command.from, None);
            assert_eq!(command.to, None);
            assert_eq!(command.folder, "inbox");
            assert!(matches!(command.status, MailAbsorbStatus::All));
            assert_eq!(command.absorbed_by, None);
            assert!(command.json);
            assert_eq!(command.project, Some(PathBuf::from("/tmp/project")));
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn parses_local_mail_list_from_and_to_without_for() {
    let from_only = Cli::try_parse_from(["vivi", "mail", "list", "--from", "mind"]).unwrap();
    match from_only.command {
        Command::Mail {
            command: MailCommand::List(command),
        } => {
            assert_eq!(command.for_identity, None);
            assert_eq!(command.from.as_deref(), Some("mind"));
            assert_eq!(command.to, None);
        }
        other => panic!("unexpected command: {other:?}"),
    }

    let to_only = Cli::try_parse_from(["vivi", "mail", "list", "--to", "hand"]).unwrap();
    match to_only.command {
        Command::Mail {
            command: MailCommand::List(command),
        } => {
            assert_eq!(command.to.as_deref(), Some("hand"));
        }
        other => panic!("unexpected command: {other:?}"),
    }

    let both = Cli::try_parse_from([
        "vivi", "mail", "list", "--for", "hand", "--from", "mind", "--to", "hand",
    ])
    .unwrap();
    match both.command {
        Command::Mail {
            command: MailCommand::List(command),
        } => {
            assert_eq!(command.for_identity.as_deref(), Some("hand"));
            assert_eq!(command.from.as_deref(), Some("mind"));
            assert_eq!(command.to.as_deref(), Some("hand"));
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn rejects_local_mail_list_without_scope() {
    let err = Cli::try_parse_from(["vivi", "mail", "list"]).unwrap_err();
    let message = err.to_string();
    assert!(message.contains("--for"), "{message}");
    assert!(message.contains("--from"), "{message}");
    assert!(message.contains("--to"), "{message}");
}

#[test]
fn parses_local_mail_show() {
    let cli = Cli::try_parse_from(["vivi", "mail", "show", "abc123", "--json"]).unwrap();

    match cli.command {
        Command::Mail {
            command:
                MailCommand::Show {
                    handles,
                    json,
                    project,
                },
        } => {
            assert_eq!(handles, vec!["abc123"]);
            assert!(json);
            assert_eq!(project, None);
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn parses_trace_command() {
    let cli = Cli::try_parse_from([
        "vivi",
        "trace",
        "abc123",
        "--json",
        "--max-depth",
        "5",
        "--limit",
        "100",
        "--project",
        "/tmp/project",
    ])
    .unwrap();

    match cli.command {
        Command::Trace(command) => {
            assert_eq!(command.handle, "abc123");
            assert!(command.json);
            assert_eq!(command.max_depth, 5);
            assert_eq!(command.limit, 100);
            assert_eq!(command.project, Some(PathBuf::from("/tmp/project")));
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn parses_local_mail_dump_filters() {
    let cli = Cli::try_parse_from([
        "vivi",
        "mail",
        "dump",
        "--participant",
        "cto",
        "--from",
        "ceo",
        "--subject",
        "review",
        "--body",
        "blocker",
        "--since",
        "24h",
        "--json",
    ])
    .unwrap();

    match cli.command {
        Command::Mail {
            command: MailCommand::Dump(command),
        } => {
            assert_eq!(command.participant.as_deref(), Some("cto"));
            assert_eq!(command.from.as_deref(), Some("ceo"));
            assert_eq!(command.subject.as_deref(), Some("review"));
            assert_eq!(command.body.as_deref(), Some("blocker"));
            assert_eq!(command.since.as_deref(), Some("24h"));
            assert!(command.json);
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn parses_task_done() {
    let cli = Cli::try_parse_from(["vivi", "task", "done", "abc123", "--for", "cto"]).unwrap();

    match cli.command {
        Command::Task {
            command:
                TaskCommand::Done {
                    handle,
                    for_identity,
                    note,
                    verdict,
                    repo,
                    tip,
                    project,
                },
        } => {
            assert_eq!(handle, "abc123");
            assert_eq!(for_identity, "cto");
            assert_eq!(note, None);
            assert_eq!(verdict, None);
            assert!(repo.is_empty());
            assert!(tip.is_empty());
            assert_eq!(project, None);
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn parses_task_done_with_verdict_and_tips() {
    let cli = Cli::try_parse_from([
        "vivi",
        "task",
        "done",
        "abc123",
        "--for",
        "auditor-1",
        "--verdict",
        "clean_pass",
        "--repo",
        "examples",
        "--tip",
        "e968cc3",
        "--repo",
        "hosts",
        "--tip",
        "0de5c36",
        "--note",
        "P2: minor lint",
    ])
    .unwrap();

    match cli.command {
        Command::Task {
            command:
                TaskCommand::Done {
                    handle,
                    for_identity,
                    note,
                    verdict,
                    repo,
                    tip,
                    project,
                },
        } => {
            assert_eq!(handle, "abc123");
            assert_eq!(for_identity, "auditor-1");
            assert_eq!(note.as_deref(), Some("P2: minor lint"));
            assert_eq!(verdict.as_deref(), Some("clean_pass"));
            assert_eq!(repo, vec!["examples", "hosts"]);
            assert_eq!(tip, vec!["e968cc3", "0de5c36"]);
            assert_eq!(project, None);
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn parses_task_list_done_status() {
    let cli =
        Cli::try_parse_from(["vivi", "task", "list", "--for", "cto", "--status", "done"]).unwrap();

    match cli.command {
        Command::Task {
            command:
                TaskCommand::List {
                    for_identity,
                    from,
                    to,
                    status,
                    json,
                    project,
                    blocked: _,
                    blocking: _,
                },
        } => {
            assert_eq!(for_identity.as_deref(), Some("cto"));
            assert_eq!(from, None);
            assert_eq!(to, None);
            assert!(matches!(status, TaskStatus::Done));
            assert!(!json);
            assert_eq!(project, None);
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn parses_task_list_json() {
    let cli = Cli::try_parse_from(["vivi", "task", "list", "--for", "cto", "--json"]).unwrap();

    match cli.command {
        Command::Task {
            command: TaskCommand::List { json, .. },
        } => assert!(json),
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn task_dump_defaults_to_open_status() {
    let cli = Cli::try_parse_from(["vivi", "task", "dump", "--for", "cto"]).unwrap();

    match cli.command {
        Command::Task {
            command: TaskCommand::Dump(command),
        } => assert!(matches!(command.status, TaskDumpStatusArg::Open)),
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn parses_task_dump_status_all() {
    let cli = Cli::try_parse_from([
        "vivi", "task", "dump", "--for", "cto", "--status", "all", "--output", "tasks.md",
    ])
    .unwrap();

    match cli.command {
        Command::Task {
            command: TaskCommand::Dump(command),
        } => {
            assert_eq!(command.for_identity.as_deref(), Some("cto"));
            assert!(matches!(command.status, TaskDumpStatusArg::All));
            assert_eq!(command.output, Some(PathBuf::from("tasks.md")));
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn parses_need_done_with_note() {
    let cli = Cli::try_parse_from([
        "vivi",
        "need",
        "done",
        "abc123",
        "--for",
        "ceo",
        "--note",
        "tasks completed",
    ])
    .unwrap();

    match cli.command {
        Command::Need {
            command:
                NeedCommand::Done {
                    handle,
                    for_identity,
                    note,
                    project,
                },
        } => {
            assert_eq!(handle, "abc123");
            assert_eq!(for_identity, "ceo");
            assert_eq!(note.as_deref(), Some("tasks completed"));
            assert_eq!(project, None);
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn parses_want_promote() {
    let cli = Cli::try_parse_from(["vivi", "want", "promote", "abc123", "--for", "ceo"]).unwrap();

    match cli.command {
        Command::Want {
            command:
                WantCommand::Promote {
                    handle,
                    for_identity,
                    note,
                    project,
                },
        } => {
            assert_eq!(handle, "abc123");
            assert_eq!(for_identity, "ceo");
            assert_eq!(note, None);
            assert_eq!(project, None);
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn parses_want_list_status_all() {
    let cli =
        Cli::try_parse_from(["vivi", "want", "list", "--for", "ceo", "--status", "all"]).unwrap();

    match cli.command {
        Command::Want {
            command: WantCommand::List { status, .. },
        } => assert!(matches!(status, vivi::cli::WantStatus::All)),
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn parses_want_done_and_drop() {
    let done = Cli::try_parse_from(["vivi", "want", "done", "abc123", "--for", "ceo"]).unwrap();
    match done.command {
        Command::Want {
            command:
                WantCommand::Done {
                    handle,
                    for_identity,
                    ..
                },
        } => {
            assert_eq!(handle, "abc123");
            assert_eq!(for_identity, "ceo");
        }
        other => panic!("unexpected command: {other:?}"),
    }

    let drop = Cli::try_parse_from(["vivi", "want", "drop", "abc123", "--for", "ceo"]).unwrap();
    match drop.command {
        Command::Want {
            command:
                WantCommand::Drop {
                    handle,
                    for_identity,
                    ..
                },
        } => {
            assert_eq!(handle, "abc123");
            assert_eq!(for_identity, "ceo");
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn parses_mail_absorb() {
    let cli = Cli::try_parse_from([
        "vivi", "mail", "absorb", "abc123", "--for", "mind", "--note", "handled",
    ])
    .unwrap();

    match cli.command {
        Command::Mail {
            command: MailCommand::Absorb(absorb),
        } => {
            assert_eq!(absorb.handle, "abc123");
            assert_eq!(absorb.for_identity, "mind");
            assert_eq!(absorb.note.as_deref(), Some("handled"));
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn parses_absorb_for_each_record_kind() {
    let mail = Cli::try_parse_from(["vivi", "mail", "absorb", "abc123", "--for", "mind"]).unwrap();
    assert!(matches!(
        mail.command,
        Command::Mail {
            command: MailCommand::Absorb(ref absorb)
        } if absorb.handle == "abc123" && absorb.for_identity == "mind"
    ));
    let task = Cli::try_parse_from(["vivi", "task", "absorb", "abc123", "--for", "mind"]).unwrap();
    assert!(matches!(
        task.command,
        Command::Task {
            command: TaskCommand::Absorb(ref absorb)
        } if absorb.handle == "abc123"
    ));
    let need = Cli::try_parse_from(["vivi", "need", "absorb", "abc123", "--for", "mind"]).unwrap();
    assert!(matches!(
        need.command,
        Command::Need {
            command: NeedCommand::Absorb(ref absorb)
        } if absorb.handle == "abc123"
    ));
    let want = Cli::try_parse_from(["vivi", "want", "absorb", "abc123", "--for", "mind"]).unwrap();
    assert!(matches!(
        want.command,
        Command::Want {
            command: WantCommand::Absorb(ref absorb)
        } if absorb.handle == "abc123"
    ));
    let memo = Cli::try_parse_from(["vivi", "memo", "absorb", "abc123", "--for", "mind"]).unwrap();
    assert!(matches!(
        memo.command,
        Command::Memo {
            command: MemoCommand::Absorb(ref absorb)
        } if absorb.handle == "abc123"
    ));
}

#[test]
fn parses_task_from_source_handle() {
    let cli = Cli::try_parse_from([
        "vivi",
        "task",
        "from",
        "abc123",
        "--for",
        "mind",
        "--to",
        "hand-2",
        "--subject",
        "Do work",
        "--body",
        "body",
    ])
    .unwrap();

    match cli.command {
        Command::Task {
            command: TaskCommand::From(command),
        } => {
            assert_eq!(command.handle, "abc123");
            assert_eq!(command.for_identity, "mind");
            assert_eq!(command.to, vec!["hand-2"]);
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn parses_want_set_priority() {
    let cli = Cli::try_parse_from([
        "vivi",
        "want",
        "set-priority",
        "abc123",
        "--for",
        "mind",
        "--priority",
        "P1",
        "--rank",
        "20",
        "--repo",
        "faber-runtime",
        "--lane",
        "correctness",
    ])
    .unwrap();

    match cli.command {
        Command::Want {
            command:
                WantCommand::SetPriority {
                    handle,
                    priority,
                    rank,
                    repo,
                    lane,
                    ..
                },
        } => {
            assert_eq!(handle, "abc123");
            assert_eq!(priority, "P1");
            assert_eq!(rank, Some(20));
            assert_eq!(repo.as_deref(), Some("faber-runtime"));
            assert_eq!(lane.as_deref(), Some("correctness"));
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn parses_cycle_intake() {
    let cli = Cli::try_parse_from([
        "vivi",
        "cycle",
        "intake",
        "--for",
        "mind",
        "--cursor-file",
        ".vivi/mind.cursor",
        "--write-cursor",
        "--json",
    ])
    .unwrap();

    match cli.command {
        Command::Cycle {
            command:
                CycleCommand::Intake {
                    for_identity,
                    cursor_file,
                    write_cursor,
                    json,
                    ..
                },
        } => {
            assert_eq!(for_identity, "mind");
            assert_eq!(cursor_file, Some(PathBuf::from(".vivi/mind.cursor")));
            assert!(write_cursor);
            assert!(json);
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn parses_memo_search_command() {
    let cli =
        Cli::try_parse_from(["vivi", "memo", "search", "railway deploy", "--for", "mind"]).unwrap();

    match cli.command {
        Command::Memo {
            command:
                MemoCommand::Search {
                    query,
                    for_identity,
                    subject,
                    json,
                    ..
                },
        } => {
            assert_eq!(query, "railway deploy");
            assert_eq!(for_identity, "mind");
            assert!(!subject);
            assert!(!json);
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn parses_memo_search_subject_only() {
    let cli = Cli::try_parse_from([
        "vivi",
        "memo",
        "search",
        "ACCEPT*",
        "--for",
        "mind",
        "--subject",
        "--json",
    ])
    .unwrap();

    match cli.command {
        Command::Memo {
            command:
                MemoCommand::Search {
                    query,
                    for_identity,
                    subject,
                    json,
                    ..
                },
        } => {
            assert_eq!(query, "ACCEPT*");
            assert_eq!(for_identity, "mind");
            assert!(subject);
            assert!(json);
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn parses_task_send_with_depends_on() {
    let cli = Cli::try_parse_from([
        "vivi",
        "task",
        "send",
        "--from",
        "ceo",
        "--to",
        "hand-1",
        "--subject",
        "do this after that",
        "--body",
        "depends on prior work",
        "--depends-on",
        "abc123",
        "--depends-on",
        "def456",
    ])
    .unwrap();

    match cli.command {
        Command::Task {
            command:
                TaskCommand::Send(TaskSendCommand {
                    send, depends_on, ..
                }),
        } => {
            assert_eq!(send.from, "ceo");
            assert_eq!(send.to, vec!["hand-1"]);
            assert_eq!(send.subject, "do this after that");
            assert_eq!(depends_on, vec!["abc123", "def456"]);
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn parses_need_and_want_send_with_depends_on() {
    let need = Cli::try_parse_from([
        "vivi",
        "need",
        "send",
        "--from",
        "mind",
        "--to",
        "cto",
        "--subject",
        "audit wave",
        "--body",
        "work",
        "--depends-on",
        "abc123",
    ])
    .unwrap();
    match need.command {
        Command::Need {
            command:
                NeedCommand::Send(NeedSendCommand {
                    send, depends_on, ..
                }),
        } => {
            assert_eq!(send.from, "mind");
            assert_eq!(send.subject, "audit wave");
            assert_eq!(depends_on, vec!["abc123"]);
        }
        other => panic!("unexpected command: {other:?}"),
    }

    let want = Cli::try_parse_from([
        "vivi",
        "want",
        "send",
        "--from",
        "mind",
        "--to",
        "cto",
        "--subject",
        "retry helper",
        "--body",
        "later",
        "--depends-on",
        "abc123",
        "--depends-on",
        "def456",
    ])
    .unwrap();
    match want.command {
        Command::Want {
            command:
                WantCommand::Send(WantSendCommand {
                    send, depends_on, ..
                }),
        } => {
            assert_eq!(send.to, vec!["cto"]);
            assert_eq!(depends_on, vec!["abc123", "def456"]);
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn parses_task_list_from_to_and_status_all() {
    let cli = Cli::try_parse_from([
        "vivi", "task", "list", "--from", "mind", "--to", "hand", "--status", "all",
    ])
    .unwrap();
    match cli.command {
        Command::Task {
            command:
                TaskCommand::List {
                    for_identity,
                    from,
                    to,
                    status,
                    ..
                },
        } => {
            assert_eq!(for_identity, None);
            assert_eq!(from.as_deref(), Some("mind"));
            assert_eq!(to.as_deref(), Some("hand"));
            assert!(matches!(status, TaskStatus::All));
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn rejects_task_list_without_scope() {
    let err = Cli::try_parse_from(["vivi", "task", "list"]).unwrap_err();
    let message = err.to_string();
    assert!(message.contains("--for"), "{message}");
    assert!(message.contains("--from"), "{message}");
}

#[test]
fn rejects_mail_watch_kinds() {
    let err = Cli::try_parse_from(["vivi", "mail", "watch", "--for", "mind", "--kinds", "task"])
        .unwrap_err();
    assert!(err.to_string().contains("--kinds"), "{err}");
}

#[test]
fn parses_want_dump_as_task_dump_status() {
    let cli =
        Cli::try_parse_from(["vivi", "want", "dump", "--from", "ceo", "--status", "all"]).unwrap();
    match cli.command {
        Command::Want {
            command: WantCommand::Dump(command),
        } => {
            assert_eq!(command.from.as_deref(), Some("ceo"));
            assert!(matches!(command.status, TaskDumpStatusArg::All));
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn rejects_want_dump_mail_folder_flag() {
    let err = Cli::try_parse_from(["vivi", "want", "dump", "--folder", "inbox"]).unwrap_err();
    assert!(err.to_string().contains("--folder"), "{err}");
}

#[test]
fn parses_task_list_blocked_and_blocking() {
    let cli = Cli::try_parse_from([
        "vivi",
        "task",
        "list",
        "--for",
        "hand-1",
        "--blocked",
        "--blocking",
        "abc123",
    ])
    .unwrap();

    match cli.command {
        Command::Task {
            command:
                TaskCommand::List {
                    for_identity,
                    blocked,
                    blocking,
                    ..
                },
        } => {
            assert_eq!(for_identity.as_deref(), Some("hand-1"));
            assert!(blocked);
            assert_eq!(blocking.as_deref(), Some("abc123"));
        }
        other => panic!("unexpected command: {other:?}"),
    }
}
