//! End-to-end smoke test of `vivi pump` through the real binary: one tick of `once` over a temp mailspace, the files the
//! tick writes, the `PAUSE` flag, the missing `[pump]` table error, and the
//! single-instance lock.

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use vivi::mailspace::{Mailspace, PumpAction, PumpActionKind};
use vivi::pump::flow::index::{RunRow, StepState};
use vivi::pump::store::{DB_FILENAME, PumpStore};

#[test]
fn once_runs_one_tick_and_writes_db_status_and_log() {
    let project = temp_mailspace();
    let home = pump_home(&project);
    assert_success(&vivi(["pump", "once", "--project", root(&project)]));
    let status = fs::read_to_string(home.join("status.txt")).unwrap();
    assert!(home.join("pump.db").is_file(), "no pump.db");
    assert!(home.join("log.txt").is_file(), "no log.txt");
    let lines: Vec<&str> = status.lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "status.txt must be the stamp line: {status}"
    );
    assert!(lines[0].ends_with('Z'), "line 1: {status}");
    assert!(
        !status.lines().any(|line| line.starts_with("units")),
        "no unit line: {status}"
    );
}

#[test]
fn replan_posts_an_action_on_the_open_record() {
    let help = stdout(&vivi(["pump", "--help"]));
    assert!(help.contains("replan"), "{help}");

    let project = temp_mailspace();
    let handle = send_need(&project);
    open_pump_db(&project);

    // A replan acts on a dispatch: with no open pump record it is refused.
    let no_record = vivi(["pump", "replan", &handle, "--project", root(&project)]);
    assert!(!no_record.status.success(), "{}", stdout(&no_record));
    let error = stderr(&no_record);
    assert!(error.contains("no open pump record"), "{error}");
    assert!(error.contains("vivi pump send"), "{error}");

    assert_success(&vivi([
        "pump",
        "send",
        "--need",
        &handle,
        "--project",
        root(&project),
    ]));
    let queued = vivi(["pump", "replan", &handle, "--project", root(&project)]);
    assert_success(&queued);
    let actions = posted_actions(&project);
    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0].need, handle);
    assert_eq!(actions[0].request.kind, PumpActionKind::Replan);

    let forced = vivi([
        "pump",
        "replan",
        &handle,
        "--force",
        "--project",
        root(&project),
    ]);
    assert!(!forced.status.success(), "--force is gone");
    assert_eq!(posted_actions(&project).len(), 1);
}

#[test]
fn requeue_and_withdraw_post_actions_on_the_open_record() {
    let project = temp_mailspace();
    let handle = send_need(&project);
    let step = seed_flow_step(&project, &handle);

    let refused = vivi(["pump", "requeue", &step, "--project", root(&project)]);
    assert!(!refused.status.success(), "{}", stdout(&refused));
    assert!(
        stderr(&refused).contains("no open pump record"),
        "{}",
        stderr(&refused)
    );
    let refused = vivi([
        "pump",
        "withdraw",
        &handle,
        "--reason",
        "stop",
        "--project",
        root(&project),
    ]);
    assert!(!refused.status.success(), "{}", stdout(&refused));
    assert!(posted_actions(&project).is_empty());

    assert_success(&vivi([
        "pump",
        "send",
        "--need",
        &handle,
        "--project",
        root(&project),
    ]));
    assert_success(&vivi([
        "pump",
        "requeue",
        &step,
        "--project",
        root(&project),
    ]));
    assert_success(&vivi([
        "pump",
        "withdraw",
        &handle,
        "--reason",
        "stop",
        "--project",
        root(&project),
    ]));

    let actions = posted_actions(&project);
    let kinds: Vec<_> = actions.iter().map(|action| action.request.kind).collect();
    assert_eq!(
        kinds,
        vec![PumpActionKind::Requeue, PumpActionKind::Withdraw]
    );
    assert_eq!(actions[0].request.unit.as_deref(), Some(step.as_str()));
    assert_eq!(actions[1].request.reason, "stop");

    let closed = vivi([
        "need",
        "list",
        "--for",
        "mind",
        "--status",
        "done",
        "--project",
        root(&project),
    ]);
    assert_success(&closed);
    assert!(stdout(&closed).contains(&handle), "{}", stdout(&closed));
}

#[test]
fn pump_send_opens_a_record_for_a_need_addressed_to_a_third_identity() {
    let project = temp_mailspace();
    let _ = send_need(&project);
    let out = vivi([
        "need",
        "send",
        "--from",
        "operator",
        "--to",
        "operator",
        "--subject",
        "not for the pump",
        "--done-when",
        "never.",
        "--body",
        "never.",
        "--project",
        root(&project),
    ]);
    assert_success(&out);
    let stray = stdout(&out)
        .split_whitespace()
        .find(|word| word.len() == 8 && word.chars().all(|c| c.is_ascii_hexdigit()))
        .unwrap()
        .to_owned();

    let sent = vivi([
        "pump",
        "send",
        "--need",
        &stray,
        "--project",
        root(&project),
    ]);

    assert_success(&sent);
    let listed = vivi(["pump", "list", "--json", "--project", root(&project)]);
    assert!(stdout(&listed).contains(&stray), "{}", stdout(&listed));
}

/// The pump actions posted in the project and not yet applied.
fn posted_actions(project: &tempfile::TempDir) -> Vec<PumpAction> {
    Mailspace::discover(Some(project.path()))
        .unwrap()
        .pump_pending_actions(&BTreeSet::new())
        .unwrap()
}

/// Send one need and return its handle.
fn send_need(project: &tempfile::TempDir) -> String {
    // `mailspace init` creates no roster; the sender and holder must exist.
    for (role, kind) in [("operator", "operator"), ("mind", "mind")] {
        let _ = vivi([
            "role",
            "add",
            role,
            "--kind",
            kind,
            "--project",
            root(project),
        ]);
    }
    let out = vivi([
        "need",
        "send",
        "--from",
        "operator",
        "--to",
        "mind",
        "--subject",
        "pump visibility",
        "--done-when",
        "the pump block renders.",
        "--body",
        "the pump block renders.",
        "--project",
        root(project),
    ]);
    assert_success(&out);
    stdout(&out)
        .lines()
        .find_map(|line| line.strip_prefix("created mind ").map(str::to_owned))
        .unwrap_or_else(|| panic!("no handle in {}", stdout(&out)))
}

/// Create the project's `pump.db`.
fn open_pump_db(project: &tempfile::TempDir) {
    PumpStore::open(&pump_home(project).join(DB_FILENAME)).unwrap();
}

/// Seed a `live` flow run of `need` holding one step; returns the step task.
fn seed_flow_step(project: &tempfile::TempDir, need: &str) -> String {
    let task = format!("{need}-step");
    let store = PumpStore::open(&pump_home(project).join(DB_FILENAME)).unwrap();
    let run = RunRow {
        need: need.to_owned(),
        run: 1,
        flow: String::from("build"),
        flow_version: 2,
        kind: String::from("build"),
        entry: String::from("plan"),
        state: String::from("live"),
    };
    store.insert_run(&run).unwrap();
    store
        .insert_run_step(&run, &run.step("plan", &task, StepState::Pending))
        .unwrap();
    task
}

#[test]
fn requeue_help_names_steps_and_job_only() {
    let help = stdout(&vivi(["pump", "requeue", "--help"]));
    assert!(help.contains("STEP"), "{help}");
    assert!(help.contains("--job"), "{help}");
    for gone in [
        "--attempts",
        "--fresh",
        "--refund-host-failures",
        "--stuck-since",
        "--why",
    ] {
        assert!(!help.contains(gone), "{gone} is retired: {help}");
    }
}

#[test]
fn a_config_that_still_sets_the_retired_board_mark_does_not_start_the_pump() {
    let project = temp_mailspace();
    let config = project.path().join(".vivi").join("mailspace.toml");
    let text = fs::read_to_string(&config).unwrap();
    fs::write(
        &config,
        text.replace("[pump.board]\n", "[pump.board]\nmark = \"pump\"\n"),
    )
    .unwrap();
    let out = vivi(["pump", "once", "--project", root(&project)]);
    assert!(
        !out.status.success(),
        "a retired key must be an error, stdout: {}",
        stdout(&out)
    );
    let all = format!("{}{}", stdout(&out), stderr(&out));
    assert!(all.contains("[pump.board] mark is retired"), "{all}");
    assert!(all.contains("vivi pump send --need <handle>"), "{all}");
}

#[test]
fn a_pause_flag_still_writes_status_and_exits_zero() {
    let project = temp_mailspace();
    let home = pump_home(&project);
    fs::create_dir_all(&home).unwrap();
    fs::write(home.join("PAUSE"), "").unwrap();
    assert_success(&vivi(["pump", "once", "--project", root(&project)]));
    assert!(
        !home.join("PAUSE").exists(),
        "legacy pause file was not migrated"
    );
    let status = fs::read_to_string(home.join("status.txt")).unwrap();
    assert!(
        status
            .lines()
            .next()
            .is_some_and(|line| line.ends_with('Z')),
        "{status}"
    );
    assert!(
        !status.lines().any(|line| line.starts_with("merges ")),
        "{status}"
    );
}

#[test]
fn a_missing_pump_table_is_a_clear_error() {
    let project = tempfile::tempdir().unwrap();
    assert_success(&vivi(["mailspace", "init", "--project", root(&project)]));
    // `mailspace init` writes a starter [pump]; take it out again.
    let config = project.path().join(".vivi").join("mailspace.toml");
    let text = fs::read_to_string(&config).unwrap();
    fs::write(&config, text.replace(vivi::mailspace::STARTER_PUMP, "")).unwrap();
    let out = vivi(["pump", "once", "--project", root(&project)]);
    assert!(!out.status.success(), "{}", stdout(&out));
    assert!(stderr(&out).contains("[pump]"), "{}", stderr(&out));
}

#[test]
fn a_second_instance_is_refused() {
    let project = temp_mailspace();
    let home = pump_home(&project);
    fs::create_dir_all(&home).unwrap();
    let held = File::create(home.join("lock")).unwrap();
    held.lock().unwrap();
    let out = vivi(["pump", "once", "--project", root(&project)]);
    assert!(!out.status.success(), "{}", stdout(&out));
    assert!(
        stderr(&out).contains("another vivi pump is running"),
        "{}",
        stderr(&out)
    );
    drop(held);
    assert_success(&vivi(["pump", "once", "--project", root(&project)]));
}

#[test]
fn a_service_child_pump_refreshes_status_every_tick() {
    let project = temp_mailspace();
    let status = pump_home(&project).join("status.txt");
    let mut child = Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args([
            "pump",
            "run",
            "--service-child",
            "--project",
            root(&project),
        ])
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    // The first tick writes status.txt; wait for it before taking its mtime.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !status.exists() {
        assert!(
            Instant::now() < deadline,
            "the service child wrote no status.txt within 10s"
        );
        thread::sleep(Duration::from_millis(200));
    }
    let first = fs::metadata(&status).unwrap().modified().unwrap();
    // Every tick rewrites the file, so the mtime must advance within a few
    // one-second ticks.
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut next = first;
    while next <= first {
        assert!(
            Instant::now() < deadline,
            "status.txt was not refreshed in 30s: first {first:?}, latest {next:?}"
        );
        thread::sleep(Duration::from_millis(200));
        next = fs::metadata(&status).unwrap().modified().unwrap();
    }
    let refreshed = next > first;
    // Stop the child: `pump stop`, then wait; kill it if either step stalls.
    let stopped = vivi(["pump", "stop", "--project", root(&project)]);
    let mut exited = stopped.status.success();
    if exited {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if Instant::now() >= deadline {
                exited = false;
                break;
            }
            thread::sleep(Duration::from_millis(200));
        }
    }
    if !exited {
        child.kill().unwrap();
    }
    let _ = child.wait().unwrap();
    assert!(
        refreshed,
        "status.txt mtime did not advance: first {first:?}, latest {next:?}"
    );
}

/// A temp project mailspace with the minimal `[pump]` table appended.
fn temp_mailspace() -> tempfile::TempDir {
    let project = tempfile::tempdir().unwrap();
    assert_success(&vivi(["mailspace", "init", "--project", root(&project)]));
    let config = project.path().join(".vivi").join("mailspace.toml");
    let root = project.path().to_str().unwrap();
    let table = format!(
        "\n[pump]\nhome = \"{root}/.vivi/pump-home\"\ntick = 1\n\
         committer_name = \"Test\"\ncommitter_email = \"test@example.com\"\n\n\
         [pump.board]\nfrom = []\npull = false\n\n\
         [pump.dispatch]\nlanes = [{{ model = \"test/model\", cap = 1 }}]\n\n\
         [pools.test]\nprovider = \"local\"\nslots = 1\n\n\
         [pools.test.harness]\nname = \"test\"\nrun = \"test --model {{model}} @{{brief}}\"\n\
         models = [\"test/model\"]\n\n\
         [pools.test.bands]\nP2 = [{{ model = \"test/model\", reasoning = \"low\" }}]\n"
    );
    let mut parsed: toml::Value = toml::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
    // `mailspace init` writes a starter [pump]; this test supplies its own.
    parsed.as_table_mut().unwrap().remove("pump");
    for (name, kind) in [
        ("hand", "hand"),
        ("mind", "mind"),
        ("pump-planner", "planner"),
        ("merge", "merge"),
        ("auditor", "auditor"),
    ] {
        ensure_role(&mut parsed, name, kind, "test", "P2");
    }
    let mut text = toml::to_string(&parsed).unwrap();
    text.push_str(&table);
    fs::write(config, text).unwrap();
    // The pump refuses to start without its integration clone; an empty repository stands in.
    let repo = pump_home(&project).join("repo");
    fs::create_dir_all(&repo).unwrap();
    let init = Command::new("git")
        .args(["init", "-q"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert_success(&init);
    assert_success(&vivi([
        "pump",
        "start",
        "--project",
        project.path().to_str().unwrap(),
    ]));
    project
}

fn ensure_role(config: &mut toml::Value, name: &str, kind: &str, pool: &str, model: &str) {
    let identities = config
        .get_mut("identities")
        .and_then(toml::Value::as_array_mut)
        .expect("mailspace init omitted identities array");
    let index = identities
        .iter()
        .position(|identity| identity.get("name").and_then(toml::Value::as_str) == Some(name));
    let index = if let Some(index) = index {
        index
    } else {
        identities.push(toml::Value::Table(toml::map::Map::new()));
        identities.len() - 1
    };
    let row = identities[index]
        .as_table_mut()
        .expect("identity row is not a table");
    row.insert("name".to_owned(), toml::Value::String(name.to_owned()));
    row.insert("kind".to_owned(), toml::Value::String(kind.to_owned()));
    row.insert("pool".to_owned(), toml::Value::String(pool.to_owned()));
    row.insert("model".to_owned(), toml::Value::String(model.to_owned()));
}

fn pump_home(project: &tempfile::TempDir) -> std::path::PathBuf {
    project.path().join(".vivi").join("pump-home")
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

#[test]
fn log_prints_the_last_lines_and_applies_filters() {
    let project = temp_mailspace();
    let home = pump_home(&project);
    fs::write(
        home.join("log.txt"),
        "10:00:00 one\n10:00:01 lane default/2: two u1\n10:00:02 three u12\n10:00:03 four\n",
    )
    .unwrap();
    fs::write(
        home.join("exceptions.txt"),
        "10:00:09 stuck u1 needs a person\n",
    )
    .unwrap();
    let tail = vivi(["pump", "log", "--project", root(&project), "-n", "2"]);
    assert_success(&tail);
    assert_eq!(stdout(&tail), "10:00:02 three u12\n10:00:03 four\n");
    let unit = vivi(["pump", "log", "--project", root(&project), "--unit", "u1"]);
    assert_eq!(stdout(&unit), "10:00:01 lane default/2: two u1\n");
    let grep = vivi(["pump", "log", "--project", root(&project), "--grep", "four"]);
    assert_eq!(stdout(&grep), "10:00:03 four\n");
    let exceptions = vivi(["pump", "log", "--project", root(&project), "--exceptions"]);
    assert_success(&exceptions);
    assert_eq!(stdout(&exceptions), "10:00:09 stuck u1 needs a person\n");
}

#[test]
fn log_without_a_file_exits_one_naming_the_path() {
    let project = temp_mailspace();
    let out = vivi(["pump", "log", "--project", root(&project)]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("no pump log at"), "{}", stderr(&out));
    assert!(stderr(&out).contains("log.txt"), "{}", stderr(&out));
}

#[test]
fn pump_once_on_a_freshly_initialised_project_holds_instead_of_failing() {
    let project = tempfile::tempdir().unwrap();
    assert_success(&vivi(["mailspace", "init", "--project", root(&project)]));
    assert_success(&vivi(["pump", "start", "--project", root(&project)]));
    let out = vivi(["pump", "once", "--project", root(&project)]);
    assert_success(&out);
    let text = fs::read_to_string(project.path().join(".vivi").join("pump").join("status.txt"))
        .unwrap_or_default();
    assert!(text.contains("not ready:"), "{text}");
    assert!(text.contains("pump.committer_name"), "{text}");
    let health = vivi(["pump", "health", "--json", "--project", root(&project)]);
    assert_success(&health);
    let body = stdout(&health);
    assert!(body.contains("not ready: "), "{body}");
    assert!(body.contains("\"verdict\": \"unknown\""), "{body}");
}

#[test]
fn a_service_child_on_a_not_ready_project_announces_ready_and_keeps_running() {
    use std::io::{BufRead, BufReader};
    let project = tempfile::tempdir().unwrap();
    assert_success(&vivi(["mailspace", "init", "--project", root(&project)]));
    assert_success(&vivi(["pump", "start", "--project", root(&project)]));
    let mut child = Command::new(env!("CARGO_BIN_EXE_vivi"))
        .args([
            "pump",
            "run",
            "--service-child",
            "--project",
            root(&project),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert_eq!(line.trim(), "VIVI_PUMP_SERVICE_READY");
    thread::sleep(Duration::from_millis(1500));
    assert!(
        child.try_wait().unwrap().is_none(),
        "a held pump must stay alive, or the supervisor restarts it in a loop"
    );
    let stopped = vivi(["pump", "stop", "--project", root(&project)]);
    assert_success(&stopped);
    let deadline = Instant::now() + Duration::from_secs(15);
    let exited = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() >= deadline {
            break None;
        }
        thread::sleep(Duration::from_millis(200));
    };
    if exited.is_none() {
        child.kill().unwrap();
        let _ = child.wait();
    }
    assert!(
        exited.is_some_and(|status| status.success()),
        "the held pump did not exit cleanly after pump stop"
    );
}
