//! Flow intake through the real binary: a need starts a run exactly when it
//! has an open pump record. A mark, a sender and a closed record are not
//! intake.

use std::fs;
use std::process::{Command, Output};
use std::sync::Arc;

use vivi::mailspace::Mailspace;
use vivi::pump::config::{BoardConfig, PumpConfig};
use vivi::pump::flow::tick::tick;
use vivi::pump::host::{Ctx, RealHost};
use vivi::pump::state::State;
use vivi::pump::store::{DB_FILENAME, PumpStore};
use vivi::routing::{RoleRouting, RoutedPool, RoutedRole};

#[test]
fn flow_intake_starts_runs_only_for_needs_with_an_open_pump_record() {
    let project = temp_mailspace();
    let recorded = send_need(&project, "has a record");
    let marked = send_need(&project, "only marked");
    let closed = send_need(&project, "record closed");
    assert_success(&vivi([
        "need",
        "mark",
        &marked,
        "pump",
        "--for",
        "mind",
        "--project",
        root(&project),
    ]));
    for need in [&recorded, &closed] {
        assert_success(&vivi([
            "pump",
            "send",
            "--need",
            need,
            "--project",
            root(&project),
        ]));
    }
    let history = vivi([
        "pump",
        "history",
        &closed,
        "--json",
        "--project",
        root(&project),
    ]);
    let record = serde_json::from_str::<serde_json::Value>(&stdout(&history)).unwrap()[0]["handle"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_success(&vivi([
        "pump",
        "done",
        &record,
        "--note",
        "gave-up",
        "--project",
        root(&project),
    ]));

    let ctx = real_ctx(&project);
    let mut state = State::default();
    tick(&ctx, &mut state).unwrap();

    let store = PumpStore::open(&ctx.home.join(DB_FILENAME)).unwrap();
    let runs = |need: &str| store.runs_of(need).unwrap();
    assert_eq!(
        runs(&recorded).len(),
        1,
        "the recorded need starts one run: {:?}",
        runs(&recorded)
    );
    assert!(
        runs(&marked).is_empty(),
        "a mark without a record is not intake: {:?}",
        runs(&marked)
    );
    assert!(
        runs(&closed).is_empty(),
        "a closed record is not intake: {:?}",
        runs(&closed)
    );
}

fn real_ctx(project: &tempfile::TempDir) -> Ctx {
    let home = project.path().join(".vivi/pump");
    fs::create_dir_all(home.join("plans")).unwrap();
    let cfg = PumpConfig {
        board: BoardConfig {
            pull: false,
            ..BoardConfig::default()
        },
        ..PumpConfig::default()
    };
    let mailspace = Mailspace::discover(Some(project.path())).unwrap();
    Ctx::new(
        cfg,
        Arc::new(RealHost::new(&home)),
        project.path().to_path_buf(),
        env!("CARGO_BIN_EXE_vivi").to_string(),
    )
    .with_mailspace(mailspace.mailspace_id().to_string())
    .with_routing(planner_routing())
}

fn planner_routing() -> RoleRouting {
    RoleRouting {
        roles: vec![RoutedRole {
            name: "pump-planner".into(),
            kind: Some("planner".into()),
            status: "active".into(),
            pool: Some("here".into()),
            read_only: None,
            model: None,
            models: Vec::new(),
        }],
        pools: std::collections::BTreeMap::from([(
            "here".into(),
            RoutedPool {
                slots: 1,
                provider: "local".into(),
                fallback: None,
                caps: std::collections::BTreeMap::new(),
                models: Vec::new(),
                has_bands: false,
            },
        )]),
    }
}

fn temp_mailspace() -> tempfile::TempDir {
    let project = tempfile::tempdir().unwrap();
    assert_success(&vivi(["mailspace", "init", "--project", root(&project)]));
    for (role, kind) in [("operator", "operator"), ("mind", "mind")] {
        let _ = vivi([
            "role",
            "add",
            role,
            "--kind",
            kind,
            "--project",
            root(&project),
        ]);
    }
    project
}

fn send_need(project: &tempfile::TempDir, subject: &str) -> String {
    let out = vivi([
        "need",
        "send",
        "--from",
        "operator",
        "--to",
        "mind",
        "--subject",
        subject,
        "--body",
        "done_when: it is planned.",
        "--project",
        root(project),
    ]);
    assert_success(&out);
    stdout(&out)
        .lines()
        .find_map(|line| line.strip_prefix("created mind ").map(str::to_owned))
        .unwrap_or_else(|| panic!("no handle in {}", stdout(&out)))
}

fn root(project: &tempfile::TempDir) -> &str {
    project.path().to_str().unwrap()
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
