use std::path::PathBuf;

use clap::Parser;
use vivi::cli::{
    Cli, Command, CycleCommand, DeliveryCommand, MailAbsorbStatus, MailCommand, MailspaceCommand,
    MailspaceIdentityCommand, MemoCommand, NeedCommand, NeedSendCommand, PoolSubcommand,
    SkillsCommand, TaskCommand, TaskDumpStatusArg, TaskSendCommand, TaskStatus, WantCommand,
    WantSendCommand,
};

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
fn parses_doctor_flags_and_global_project() {
    let cli = Cli::try_parse_from([
        "vivi",
        "doctor",
        "--project",
        "/tmp/vivi-project",
        "--json",
        "--fix-hints",
    ])
    .unwrap();
    match cli.command {
        Command::Doctor(command) => {
            assert_eq!(command.project, Some(PathBuf::from("/tmp/vivi-project")));
            assert!(command.json);
            assert!(command.fix_hints);
        }
        other => panic!("unexpected command: {other:?}"),
    }

    let global =
        Cli::try_parse_from(["vivi", "--project", "/tmp/global-project", "doctor"]).unwrap();
    assert_eq!(global.project, Some(PathBuf::from("/tmp/global-project")));
    assert!(matches!(global.command, Command::Doctor(_)));
}

#[test]
fn pool_dispatch_accepts_pump_owned_need_and_kind_metadata() {
    let cli = Cli::try_parse_from([
        "vivi", "pool", "dispatch", "abcd1234", "--need", "need1234", "--kind", "fix",
    ])
    .unwrap();
    let Command::Pool {
        command: PoolSubcommand::Dispatch(command),
    } = cli.command
    else {
        panic!("unexpected command");
    };
    assert_eq!(command.need.as_deref(), Some("need1234"));
    assert_eq!(command.kind.as_deref(), Some("fix"));
}

#[test]
fn report_records_rejects_view_only_grouping_and_top_flags() {
    assert!(
        Cli::try_parse_from([
            "vivi", "report", "records", "--format", "csv", "--by", "day",
        ])
        .is_err()
    );
    assert!(
        Cli::try_parse_from([
            "vivi", "report", "records", "--format", "jsonl", "--top", "10",
        ])
        .is_err()
    );
    assert!(
        Cli::try_parse_from([
            "vivi",
            "report",
            "throughput",
            "--by",
            "hour",
            "--top",
            "10",
        ])
        .is_ok()
    );
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
                    branch,
                    commit,
                    project,
                },
        } => {
            assert_eq!(handle, "abc123");
            assert_eq!(for_identity, "cto");
            assert_eq!(note, None);
            assert_eq!(verdict, None);
            assert!(repo.is_empty());
            assert!(tip.is_empty());
            assert_eq!(branch, None);
            assert_eq!(commit, None);
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
                    branch,
                    commit,
                    project,
                },
        } => {
            assert_eq!(handle, "abc123");
            assert_eq!(for_identity, "auditor-1");
            assert_eq!(note.as_deref(), Some("P2: minor lint"));
            assert_eq!(verdict.as_deref(), Some("clean_pass"));
            assert_eq!(repo, vec!["examples", "hosts"]);
            assert_eq!(tip, vec!["e968cc3", "0de5c36"]);
            assert_eq!(branch, None);
            assert_eq!(commit, None);
            assert_eq!(project, None);
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn parses_task_done_with_integration_location() {
    let cli = Cli::try_parse_from([
        "vivi",
        "task",
        "done",
        "abc123",
        "--for",
        "mind",
        "--branch",
        "mind/review",
        "--commit",
        "deadbeef",
    ])
    .unwrap();

    let Command::Task {
        command: TaskCommand::Done { branch, commit, .. },
    } = cli.command
    else {
        panic!("unexpected command");
    };
    assert_eq!(branch.as_deref(), Some("mind/review"));
    assert_eq!(commit.as_deref(), Some("deadbeef"));
}

#[test]
fn task_done_requires_branch_and_commit_together() {
    assert!(
        Cli::try_parse_from([
            "vivi",
            "task",
            "done",
            "abc123",
            "--for",
            "mind",
            "--branch",
            "mind/review",
        ])
        .is_err()
    );
    assert!(
        Cli::try_parse_from([
            "vivi", "task", "done", "abc123", "--for", "mind", "--commit", "deadbeef",
        ])
        .is_err()
    );
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
                    held: _,
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
                    branch,
                    commit,
                    project,
                },
        } => {
            assert_eq!(handle, "abc123");
            assert_eq!(for_identity, "ceo");
            assert_eq!(note.as_deref(), Some("tasks completed"));
            assert_eq!(branch, None);
            assert_eq!(commit, None);
            assert_eq!(project, None);
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn parses_need_done_with_integration_location() {
    let cli = Cli::try_parse_from([
        "vivi",
        "need",
        "done",
        "abc123",
        "--for",
        "mind",
        "--branch",
        "mind/review",
        "--commit",
        "deadbeef",
    ])
    .unwrap();

    let Command::Need {
        command: NeedCommand::Done { branch, commit, .. },
    } = cli.command
    else {
        panic!("unexpected command");
    };
    assert_eq!(branch.as_deref(), Some("mind/review"));
    assert_eq!(commit.as_deref(), Some("deadbeef"));
}

#[test]
fn parses_delivery_send_fields() {
    let cli = Cli::try_parse_from([
        "vivi",
        "delivery",
        "send",
        "--from",
        "mind",
        "--to",
        "merge@1234abcd.local",
        "--branch",
        "review",
        "--tip",
        "deadbeef",
        "--base",
        "feedface",
        "--gate",
        "cargo test -p vivi",
        "--gate-result",
        "passed",
        "--closes",
        "aabbccdd",
        "--blocked",
        "eeff0011=waiting on review",
        "--filed",
        "33445566",
    ])
    .unwrap();

    let Command::Delivery {
        command:
            DeliveryCommand::Send {
                branch,
                tip,
                base,
                gate_result,
                closes,
                blocked,
                filed,
                ..
            },
    } = cli.command
    else {
        panic!("unexpected command");
    };
    assert_eq!(branch, "review");
    assert_eq!(tip, "deadbeef");
    assert_eq!(base, "feedface");
    assert_eq!(gate_result, "passed");
    assert_eq!(closes, vec!["aabbccdd".to_string()]);
    assert_eq!(blocked, vec!["eeff0011=waiting on review".to_string()]);
    assert_eq!(filed, vec!["33445566".to_string()]);
}

#[test]
fn parses_delivery_close_merge_commit() {
    let cli = Cli::try_parse_from([
        "vivi",
        "delivery",
        "close",
        "aabbccdd",
        "--for",
        "merge",
        "--merge-commit",
        "deadbeef",
    ])
    .unwrap();

    let Command::Delivery {
        command:
            DeliveryCommand::Close {
                handle,
                for_identity,
                merge_commit,
                ..
            },
    } = cli.command
    else {
        panic!("unexpected command");
    };
    assert_eq!(handle, "aabbccdd");
    assert_eq!(for_identity, "merge");
    assert_eq!(merge_commit, "deadbeef");
}

#[test]
fn want_promote_no_longer_parses() {
    assert!(Cli::try_parse_from(["vivi", "want", "promote", "abc123", "--for", "ceo"]).is_err());
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

#[test]
fn parses_pool_dispatch_default_branch_override() {
    use vivi::cli::PoolSubcommand;
    let parse = |extra: &[&str]| {
        let mut args = vec!["vivi", "pool", "dispatch", "abcd1234", "--model", "m/x"];
        args.extend(extra);
        let Command::Pool {
            command: PoolSubcommand::Dispatch(d),
        } = Cli::try_parse_from(args).unwrap().command
        else {
            panic!("expected pool dispatch");
        };
        d.allow_default_branch
    };
    assert!(!parse(&[]));
    assert!(parse(&["--allow-default-branch"]));
}

#[test]
fn parses_pool_dispatch_read_only_and_refuses_a_branch_with_it() {
    use vivi::cli::PoolSubcommand;
    let parse = |extra: &[&str]| {
        let mut args = vec!["vivi", "pool", "dispatch", "abcd1234", "--model", "m/x"];
        args.extend(extra);
        Cli::try_parse_from(args).map(|cli| match cli.command {
            Command::Pool {
                command: PoolSubcommand::Dispatch(d),
            } => (d.read_only, d.from, d.branch),
            _ => panic!("expected pool dispatch"),
        })
    };
    assert!(!parse(&["--branch", "b"]).unwrap().0);
    let (read_only, from, branch) = parse(&["--read-only", "--from", "main"]).unwrap();
    assert!(read_only);
    assert_eq!((from.as_deref(), branch), (Some("main"), None));
    assert!(
        parse(&["--read-only"]).unwrap().0,
        "from may come from the body"
    );
    assert!(parse(&["--read-only", "--branch", "b"]).is_err());
    assert!(parse(&["--read-only", "--allow-default-branch"]).is_err());
}

#[test]
fn parses_pool_dispatch_with_refs() {
    use vivi::cli::PoolSubcommand;
    let parse = |extra: &[&str]| {
        let mut args = vec!["vivi", "pool", "dispatch", "abcd1234", "--model", "m/x"];
        args.extend(extra);
        Cli::try_parse_from(args).map(|cli| match cli.command {
            Command::Pool {
                command: PoolSubcommand::Dispatch(d),
            } => d.with,
            _ => panic!("expected pool dispatch"),
        })
    };
    assert!(parse(&[]).unwrap().is_empty());
    assert_eq!(
        parse(&["--with", "review/aaa", "--with", "review/bbb"]).unwrap(),
        ["review/aaa", "review/bbb"]
    );
    assert_eq!(
        parse(&["--read-only", "--with", "review/aaa"]).unwrap(),
        ["review/aaa"],
        "a read-only job may name extra refs"
    );
}

#[test]
fn parses_pool_dispatch_repository_target() {
    use vivi::cli::PoolSubcommand;
    let cli = Cli::try_parse_from([
        "vivi", "pool", "dispatch", "abcd1234", "--target", "radix", "--model", "m/x",
    ])
    .unwrap();
    let Command::Pool {
        command: PoolSubcommand::Dispatch(dispatch),
    } = cli.command
    else {
        panic!("expected pool dispatch");
    };
    assert_eq!(dispatch.target.as_deref(), Some("radix"));
}

#[test]
fn parses_pool_doctor_test_and_long() {
    use vivi::cli::PoolSubcommand;
    let parse = |extra: &[&str]| {
        let mut args = vec!["vivi", "pool", "doctor"];
        args.extend(extra);
        Cli::try_parse_from(args).map(|cli| {
            let Command::Pool {
                command: PoolSubcommand::Doctor(d),
            } = cli.command
            else {
                panic!("expected pool doctor");
            };
            (d.test, d.long, d.json)
        })
    };
    assert_eq!(parse(&[]).unwrap(), (false, false, false));
    assert_eq!(parse(&["--test"]).unwrap(), (true, false, false));
    assert_eq!(
        parse(&["--test", "--long", "--json"]).unwrap(),
        (true, true, true)
    );
    assert!(parse(&["--long"]).is_err(), "--long needs --test");
}

#[test]
fn parses_skills_list_show_and_install() {
    let list = Cli::try_parse_from(["vivi", "skills", "list", "--json"]).unwrap();
    match list.command {
        Command::Skills {
            command: SkillsCommand::List { json },
        } => assert!(json),
        other => panic!("unexpected command: {other:?}"),
    }

    let show = Cli::try_parse_from([
        "vivi",
        "skills",
        "show",
        "vivi",
        "--file",
        "references/work-graph-flow.md",
    ])
    .unwrap();
    match show.command {
        Command::Skills {
            command: SkillsCommand::Show { name, file },
        } => {
            assert_eq!(name, "vivi");
            assert_eq!(file.as_deref(), Some("references/work-graph-flow.md"));
        }
        other => panic!("unexpected command: {other:?}"),
    }

    let install =
        Cli::try_parse_from(["vivi", "skills", "install", "/tmp/skills", "--force"]).unwrap();
    match install.command {
        Command::Skills {
            command: SkillsCommand::Install { dir, force },
        } => {
            assert_eq!(dir.as_os_str(), "/tmp/skills");
            assert!(force);
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn parses_config_and_role_pool_helpers() {
    use vivi::cli::{ConfigCommand, PoolSubcommand, RoleCommand};

    let show = Cli::try_parse_from(["vivi", "config", "show", "--resolved", "--json"]).unwrap();
    match show.command {
        Command::Config {
            command: ConfigCommand::Show { resolved, json, .. },
        } => {
            assert!(resolved);
            assert!(json);
        }
        other => panic!("unexpected command: {other:?}"),
    }

    let migrate = Cli::try_parse_from(["vivi", "config", "migrate", "--write"]).unwrap();
    match migrate.command {
        Command::Config {
            command: ConfigCommand::Migrate { write, .. },
        } => assert!(write),
        other => panic!("unexpected command: {other:?}"),
    }

    let pool = Cli::try_parse_from(["vivi", "role", "pool", "hand", "glm"]).unwrap();
    match pool.command {
        Command::Role {
            command: RoleCommand::Pool { name, pool, .. },
        } => {
            assert_eq!(name, "hand");
            assert_eq!(pool, "glm");
        }
        other => panic!("unexpected command: {other:?}"),
    }

    let add = Cli::try_parse_from([
        "vivi",
        "pool",
        "add",
        "host",
        "--provider",
        "local",
        "--slots",
        "4",
    ])
    .unwrap();
    match add.command {
        Command::Pool {
            command: PoolSubcommand::Add(cmd),
        } => {
            assert_eq!(cmd.name, "host");
            assert_eq!(cmd.provider, "local");
            assert_eq!(cmd.slots, 4);
        }
        other => panic!("unexpected command: {other:?}"),
    }
}
