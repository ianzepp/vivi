//! End-to-end tests for `vivi boot`.
//!
//! Boot is the project frame in one read: read-only, stateless, and idempotent.
//! These tests drive the real binary against a scratch mailspace and prove the
//! digest renders every section, that a probe contributes and can be skipped,
//! that a cap is reported, and that boot changes nothing.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

fn path_str(path: &Path) -> &str {
    path.to_str().unwrap_or_default()
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

fn boot(project: &Path) -> String {
    let output = vivi(["boot", "--project", path_str(project)]);
    assert_success(&output);
    stdout(&output)
}

fn init_mailspace(root: &Path) {
    assert_success(&vivi(["mailspace", "init", "--project", path_str(root)]));
}

fn add_role(root: &Path, name: &str, extra: &[&str]) {
    let mut args = vec!["role", "add", name, "--project", path_str(root)];
    args.extend_from_slice(extra);
    assert_success(&vivi(args));
}

fn send(project: &Path, kind: &str, from: &str, to: &str, subject: &str) -> String {
    let output = vivi([
        kind,
        "send",
        "--project",
        path_str(project),
        "--from",
        from,
        "--to",
        to,
        "--subject",
        subject,
        "--body",
        "body",
    ]);
    assert_success(&output);
    let text = stdout(&output);
    // Local delivery prints `created <to> <handle>` for work kinds and
    // `delivered <to> <handle>` for plain mail.
    ["created", "delivered"]
        .iter()
        .find_map(|verb| {
            text.lines().find_map(|line| {
                let rest = line.strip_prefix(&format!("{verb} {to}"))?;
                rest.split_whitespace().next().map(str::to_string)
            })
        })
        .unwrap_or_else(|| panic!("missing handle for {to}:\n{text}"))
}

/// A goal document whose Status line over-claims against its own register.
fn write_goal(root: &Path) {
    let dir = root.join("docs/factory/demo");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("goal.md"),
        "# GOAL: demo\n\n**Status**: active — 2/3 delivered\n\n## Ledger\n\n\
         | Unit | Need | Status | Notes |\n\
         | --- | --- | --- | --- |\n\
         | DM-1 | `a1` | done | landed |\n\
         | DM-2 | `a2` | pending | next |\n\
         | DM-3 | `a3` | pending | after |\n",
    )
    .unwrap();
    assert_success(&vivi([
        "goal",
        "add",
        "--path",
        "docs/factory/demo/goal.md",
        "--project",
        path_str(root),
    ]));
}

/// Append a probe declaration to the mailspace config.
fn declare_probe(root: &Path, name: &str, command: &str) {
    let config = root.join(".vivi/mailspace.toml");
    let mut text = fs::read_to_string(&config).unwrap();
    text.push_str(&format!(
        "\n[[probes]]\nname = \"{name}\"\ncommand = \"{command}\"\n"
    ));
    fs::write(&config, text).unwrap();
}

/// Write an executable probe that prints `body` on stdout.
fn write_probe(root: &Path, file: &str, body: &str) {
    let script = root.join(file);
    fs::write(&script, format!("#!/bin/sh\ncat <<'JSON'\n{body}\nJSON\n")).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
}

fn rosters(root: &Path) {
    init_mailspace(root);
    add_role(root, "mind", &["--kind", "mind"]);
    add_role(root, "hand", &["--kind", "hand", "--harness", "subagent"]);
    add_role(root, "cadence", &["--kind", "steward", "--cadence", "30m"]);
}

#[test]
fn boot_renders_every_section_in_one_read() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    rosters(root);

    let task = send(root, "task", "mind", "hand", "hand: implement the unit");
    let need = send(root, "need", "hand", "mind", "need: repair the parser");
    let want = send(root, "want", "mind", "mind", "want: someday tidy this");
    let mail = send(root, "mail", "hand", "mind", "status report");
    assert_success(&vivi([
        "memo",
        "save",
        "--for",
        "mind",
        "--subject",
        "POSTURE: hold the line",
        "--body",
        "standing posture",
        "--project",
        path_str(root),
    ]));
    assert_success(&vivi([
        "role",
        "charter",
        "set",
        "hand",
        "--body",
        "You are a product Hand. Implement one logical change.",
        "--project",
        path_str(root),
    ]));
    write_goal(root);

    let out = boot(root);

    assert!(out.contains("boot    "), "{out}");
    assert!(out.contains("totals  "), "{out}");
    assert!(out.contains("6 roles"), "{out}");

    // Seats: the role holding open work is shown with its binding verdict.
    assert!(out.contains("-- seats:"), "{out}");
    assert!(out.contains("hand") && out.contains("unverified"), "{out}");

    // Loops: the declared cadence with no signal yet.
    assert!(out.contains("-- loops:"), "{out}");
    assert!(out.contains("cadence") && out.contains("never"), "{out}");

    // Unabsorbed mail.
    assert!(out.contains("-- unabsorbed mail:"), "{out}");
    assert!(out.contains(&mail), "{out}");

    // Handles: all three kinds with verdicts.
    assert!(out.contains("-- handles:"), "{out}");
    for handle in [&task, &need, &want] {
        assert!(
            out.contains(handle.as_str()),
            "handle {handle} missing:\n{out}"
        );
    }
    assert!(
        out.contains("task") && out.contains("need") && out.contains("want"),
        "{out}"
    );

    // Goals: Status, register tally, the contradiction, and the frontier.
    assert!(out.contains("-- goals:"), "{out}");
    assert!(out.contains("status: active — 2/3 delivered"), "{out}");
    assert!(out.contains("register: 3 rows: pending=2 done=1"), "{out}");
    assert!(
        out.contains("MISMATCH: Status claims 2; register holds 1 done"),
        "{out}"
    );
    assert!(
        out.contains("frontier: DM-2 pending | DM-3 pending"),
        "{out}"
    );

    // Memos.
    assert!(out.contains("-- memos:"), "{out}");
    assert!(out.contains("POSTURE: hold the line"), "{out}");

    // Charter heads for roles holding open work.
    assert!(out.contains("-- charters:"), "{out}");
    assert!(out.contains("You are a product Hand"), "{out}");

    // Triage slices carry the backlog handles.
    assert!(out.contains("-- triage:"), "{out}");
    assert!(out.contains("slice 1"), "{out}");
    assert!(out.contains(&need), "{out}");
}

#[test]
fn boot_is_read_only_and_repeatable() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    rosters(root);
    let need = send(root, "need", "hand", "mind", "need: repair the parser");
    send(root, "mail", "hand", "mind", "status report");

    let config = root.join(".vivi/mailspace.toml");
    let config_before = fs::read_to_string(&config).unwrap();
    let needs_before = stdout(&vivi([
        "need",
        "list",
        "--for",
        "mind",
        "--project",
        path_str(root),
    ]));
    let mail_before = stdout(&vivi([
        "mail",
        "list",
        "--for",
        "mind",
        "--project",
        path_str(root),
    ]));

    let first = boot(root);
    let second = boot(root);

    assert_eq!(config_before, fs::read_to_string(&config).unwrap());
    assert_eq!(
        needs_before,
        stdout(&vivi([
            "need",
            "list",
            "--for",
            "mind",
            "--project",
            path_str(root)
        ]))
    );
    assert_eq!(
        mail_before,
        stdout(&vivi([
            "mail",
            "list",
            "--for",
            "mind",
            "--project",
            path_str(root)
        ]))
    );
    // Unabsorbed mail stays unabsorbed: boot never seals a record.
    assert!(second.contains("status report"), "{second}");

    // Stateless: the two runs agree on everything but the timestamp.
    let strip = |text: &str| {
        text.lines()
            .filter(|line| !line.starts_with("at      "))
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(strip(&first), strip(&second));
    assert!(first.contains(&need));
}

#[test]
fn boot_merges_probe_facts_sections_and_verdicts() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    rosters(root);
    let need = send(root, "need", "hand", "mind", "need: repair the parser");

    write_probe(
        root,
        "probe.sh",
        &format!(
            r#"{{
  "facts": ["main is at deadbeef"],
  "sections": [{{"title": "world", "lines": ["radix main deadbeef"]}}],
  "verdicts": [{{"handle": "{need}", "verdict": "stale", "detail": "commit already on main"}}]
}}"#
        ),
    );
    declare_probe(root, "demo", "probe.sh");

    let out = boot(root);

    assert!(out.contains("probe   main is at deadbeef"), "{out}");
    assert!(out.contains("-- probe: world"), "{out}");
    assert!(out.contains("radix main deadbeef"), "{out}");
    // The probe's verdict lands on the handle row.
    assert!(out.contains("stale"), "{out}");
    assert!(out.contains("commit already on main"), "{out}");
    assert!(!out.contains("stale 0"), "{out}");
}

#[test]
fn boot_skips_a_missing_probe_without_failing() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    rosters(root);
    declare_probe(root, "absent", "no-such-probe.sh");

    let out = boot(root);
    assert!(out.contains("-- probes skipped"), "{out}");
    assert!(out.contains("absent"), "{out}");
    assert!(out.contains("missing"), "{out}");
}

#[test]
fn boot_records_every_cap_that_bit() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    rosters(root);
    for index in 0..13 {
        send(root, "mail", "hand", "mind", &format!("report {index}"));
    }

    let out = boot(root);
    assert!(out.contains("-- truncated:"), "{out}");
    assert!(out.contains("unabsorbed mail: showing 12 of 13"), "{out}");
}

#[test]
fn boot_renders_a_bare_mailspace_without_failing() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    init_mailspace(root);

    let out = boot(root);
    assert!(out.contains("boot    "), "{out}");
    assert!(out.contains("3 roles"), "{out}");
    // Absent sections stay absent rather than printing empty headings.
    assert!(!out.contains("-- handles:"), "{out}");
    assert!(!out.contains("-- goals:"), "{out}");
}

#[test]
fn boot_reports_the_path_of_an_unreadable_goal() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    rosters(root);
    write_goal(root);
    fs::remove_file(root.join("docs/factory/demo/goal.md")).unwrap();

    let out = boot(root);
    assert!(out.contains("docs/factory/demo/goal.md"), "{out}");
    assert!(out.contains("(none)"), "{out}");
}

fn append_pump_and_pool(root: &Path, pool_root: &Path) {
    let config = root.join(".vivi/mailspace.toml");
    // `mailspace init` writes a starter [pump]; this test supplies its own.
    let mut text = fs::read_to_string(&config)
        .unwrap()
        .replace(vivi::mailspace::STARTER_PUMP, "");
    text.push_str(&format!(
        "\n[pump]\nbranch = \"next\"\ncommitter_name = \"Test\"\ncommitter_email = \"test@example.com\"\n\
         [pump.dispatch]\nlanes = [{{ model = \"test/model\", cap = 1 }}]\n\
         [pools.lanes]\nprovider = \"local\"\nslots = 2\nbuild_dir = \"build\"\n\
         [pools.lanes.local]\nroot = \"{}\"\nworktree_of = \"{}\"\n",
        pool_root.display(),
        root.display()
    ));
    fs::write(config, text).unwrap();
}

fn write_boot_pump(root: &Path) {
    let home = root.join(".vivi/pump");
    fs::create_dir_all(&home).unwrap();
    let now = u64::try_from(chrono::Utc::now().timestamp()).unwrap_or(1_700_000_000);
    let mut state = vivi::pump::state::State::default();
    state.needs.insert(
        "need1".into(),
        vivi::pump::state::NeedRecord {
            verdict: "units".into(),
            units: vec!["u-inflight".into(), "u-delivered".into()],
            revision: 1,
            ..vivi::pump::state::NeedRecord::default()
        },
    );
    state.units.insert(
        "u-inflight".into(),
        vivi::pump::state::Unit {
            need: "need1".into(),
            status: vivi::pump::state::Status::Inflight,
            task: Some("taskhand1".into()),
            pool: Some("lanes".into()),
            branch: Some("unit/u-inflight-a1".into()),
            model: Some("glm-4.6".into()),
            since: now.saturating_sub(600),
            since_dispatch: Some(now.saturating_sub(240)),
            ..vivi::pump::state::Unit::default()
        },
    );
    state.units.insert(
        "u-delivered".into(),
        vivi::pump::state::Unit {
            need: "need1".into(),
            status: vivi::pump::state::Status::Delivered,
            pool: Some("lanes".into()),
            branch: Some("unit/u-delivered-a1".into()),
            since: now.saturating_sub(3600),
            ..vivi::pump::state::Unit::default()
        },
    );
    state.order = vec!["u-inflight".into(), "u-delivered".into()];
    let mut store =
        vivi::pump::store::PumpStore::open(&home.join(vivi::pump::store::DB_FILENAME)).unwrap();
    store.save(&state).unwrap();
    fs::write(
        home.join("status.txt"),
        "last promote 1h ago (deadbeef)\nlane default/0 idle\n",
    )
    .unwrap();
}

fn mailspace_key(root: &Path) -> String {
    let text = fs::read_to_string(root.join(".vivi/mailspace.toml")).unwrap();
    if let Some(id) = text
        .lines()
        .find_map(|line| line.strip_prefix("id = "))
        .map(|value| value.trim().trim_matches('"').to_string())
    {
        return id;
    }
    let name = text
        .lines()
        .find_map(|line| line.strip_prefix("name = "))
        .map(|value| value.trim().trim_matches('"').to_string())
        .unwrap_or_else(|| "project".to_string());
    vivi::pool::slot::space_slug(&name)
}

fn write_claimed_slot(pool_root: &Path, space: &str) {
    let slot = pool_root.join(space).join("lanes").join("slot-1");
    fs::create_dir_all(slot.join("marks")).unwrap();
    fs::create_dir_all(slot.join("repo")).unwrap();
    fs::write(slot.join("marks/vivi_pool"), "lanes\n").unwrap();
    fs::write(slot.join("marks/vivi_pool_mailspace"), format!("{space}\n")).unwrap();
    fs::write(slot.join("marks/vivi_pool_slot"), "1\n").unwrap();
    fs::write(slot.join("marks/vivi_pool_state"), "ready\n").unwrap();
    fs::write(slot.join("marks/base_ref"), "next\n").unwrap();
    fs::write(slot.join("marks/base_sha"), "abc1234\n").unwrap();
    fs::write(
        slot.join("claim"),
        "task=taskhand1\nclaimed=400\nnonce=aa\n",
    )
    .unwrap();
}

#[test]
fn boot_omits_the_pump_section_without_a_db_or_pools() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    rosters(root);
    let out = boot(root);
    assert!(!out.contains("pump: local execution state"), "{out}");
}

#[test]
fn boot_pump_full_and_own_and_none() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    let pool = tempfile::tempdir().unwrap();
    rosters(root);
    assert_success(&vivi([
        "role",
        "set",
        "hand",
        "--pool",
        "lanes",
        "--project",
        path_str(root),
    ]));
    append_pump_and_pool(root, pool.path());
    write_boot_pump(root);
    write_claimed_slot(pool.path(), &mailspace_key(root));

    let full = stdout(&vivi([
        "boot",
        "--project",
        path_str(root),
        "--role",
        "mind",
        "--pump",
        "full",
    ]));
    assert!(full.contains("pump: local execution state"), "{full}");
    assert!(full.contains("units    "), "{full}");
    assert!(full.contains("inflight"), "{full}");
    assert!(full.contains("u-inflight"), "{full}");
    assert!(full.contains("queue"), "{full}");
    assert!(full.contains("u-delivered"), "{full}");
    assert!(full.contains("lanes"), "{full}");
    assert!(full.contains("promote"), "{full}");
    assert!(full.contains("view     full"), "{full}");

    let own = stdout(&vivi([
        "boot",
        "--project",
        path_str(root),
        "--role",
        "hand",
        "--pump",
        "own",
    ]));
    assert!(own.contains("pump: local execution state"), "{own}");
    assert!(own.contains("u-inflight"), "{own}");
    assert!(own.contains("view     own"), "{own}");
    assert!(!own.contains("u-delivered"), "{own}");
    assert!(!own.contains("oldest wait"), "{own}");

    let none = stdout(&vivi([
        "boot",
        "--project",
        path_str(root),
        "--pump",
        "none",
    ]));
    assert!(none.contains("units    "), "{none}");
    assert!(none.contains("view     none"), "{none}");
    assert!(!none.contains("u-inflight"), "{none}");
    assert!(!none.contains("u-delivered"), "{none}");

    let json = stdout(&vivi([
        "boot",
        "--project",
        path_str(root),
        "--json",
        "--pump",
        "full",
    ]));
    assert!(json.contains("\"view\": \"full\""), "{json}");
    assert!(json.contains("u-inflight"), "{json}");
}

#[test]
fn boot_unknown_role_is_an_error() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    rosters(root);
    let out = vivi(["boot", "--project", path_str(root), "--role", "ghost"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("unknown local role"),
        "{}",
        stderr(&out)
    );
}
