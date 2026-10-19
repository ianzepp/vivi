//! Tests for [`super::host`].

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use tempfile::tempdir;

use crate::pump::config::{PumpConfig, PumpTarget};
use crate::pump::host::*;

#[test]
fn shell_quote_wraps_and_escapes_single_quotes() {
    assert_eq!(shell_quote("two words"), "'two words'");
    assert_eq!(shell_quote("it's"), "'it'\\''s'");
    assert_eq!(shell_quote("a'b'c"), "'a'\\''b'\\''c'");
    assert_eq!(shell_quote(""), "''");
}

#[test]
fn shell_quote_leaves_a_safe_word_alone() {
    assert_eq!(shell_quote("abc-XYZ_1.2/3"), "abc-XYZ_1.2/3");
    assert_eq!(shell_quote("a@b%c+d=e:f,g.h"), "a@b%c+d=e:f,g.h");
}

#[test]
fn shell_split_handles_quotes_and_spaces() {
    assert_eq!(
        shell_split("say 'hello world'  \"two  words\" end"),
        vec!["say", "hello world", "two  words", "end"]
    );
    assert_eq!(shell_split("  spaced   out  "), vec!["spaced", "out"]);
    assert_eq!(shell_split(""), Vec::<String>::new());
}

#[test]
fn shell_split_falls_back_to_whitespace_on_unbalanced_quotes() {
    assert_eq!(shell_split("a 'b c"), vec!["a", "'b", "c"]);
    assert_eq!(shell_split("x \"y"), vec!["x", "\"y"]);
}

#[test]
fn real_run_returns_stdout_then_stderr_and_the_exit_code() {
    let dir = tempdir().unwrap();
    let host = RealHost::new(dir.path());
    let out = host.run("echo out; echo err 1>&2; exit 3", None, 10);
    assert_eq!(out.code, 3);
    assert_eq!(out.text, "out\nerr\n");
}

#[test]
fn real_run_times_out_with_code_124_and_the_marker_text() {
    let dir = tempdir().unwrap();
    let host = RealHost::new(dir.path());
    let started = Instant::now();
    let out = host.run("sleep 5", None, 1);
    let elapsed = started.elapsed();
    assert!(elapsed < Duration::from_secs(4), "took {elapsed:?}");
    assert_eq!(out.code, 124);
    assert_eq!(out.text, "\n[timeout after 1s]");
}

#[test]
fn real_run_applies_cwd() {
    let dir = tempdir().unwrap();
    let work = dir.path().join("work");
    fs::create_dir_all(&work).unwrap();
    let host = RealHost::new(dir.path());
    let out = host.run("pwd", Some(&work), 10);
    let expected = fs::canonicalize(&work).unwrap();
    assert_eq!(out.text.trim(), expected.to_string_lossy().as_ref());
    assert_eq!(out.code, 0);
}

#[test]
fn real_spawn_writes_the_output_file_without_waiting() {
    let dir = tempdir().unwrap();
    let host = RealHost::new(dir.path());
    let out_file = dir.path().join("detached.out");
    let started = Instant::now();
    host.spawn("echo started; sleep 5", None, None, &out_file);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "spawn must return before the command finishes"
    );
    let mut saw = false;
    while started.elapsed() < Duration::from_secs(4) {
        if fs::read_to_string(&out_file).is_ok_and(|text| text.contains("started")) {
            saw = true;
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert!(saw, "output file should gain 'started' while sleep 5 runs");
}

#[test]
fn real_spawn_writes_an_exit_receipt_for_a_failing_command() {
    let dir = tempdir().unwrap();
    let host = RealHost::new(dir.path());
    let out_file = dir.path().join("detached.out");
    let pid = host
        .spawn("exit 127", None, None, &out_file)
        .expect("spawn should report the child pid");
    assert_ne!(pid, 0);
    let receipt = dir.path().join("detached.out.rc");
    assert_eq!(exit_receipt_path(&out_file), receipt);
    let started = Instant::now();
    let mut code = String::new();
    while started.elapsed() < Duration::from_secs(4) {
        if let Ok(text) = fs::read_to_string(&receipt) {
            code = text.trim().to_string();
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(code, "127", "exit receipt at {receipt:?}");
}

#[test]
fn fake_spawn_returns_distinct_pids() {
    let dir = tempdir().unwrap();
    let host = FakeHost::new();
    let first = host
        .spawn("echo a", None, None, &dir.path().join("a.out"))
        .expect("first fake spawn should report a pid");
    let second = host
        .spawn("echo b", None, None, &dir.path().join("b.out"))
        .expect("second fake spawn should report a pid");
    assert!(first >= 1000, "first pid {first} should start at 1000");
    assert!(second >= 1000, "second pid {second} should start at 1000");
    assert_ne!(first, second);
}

#[test]
fn ctx_sh_prepends_the_shell_prefix() {
    let dir = tempdir().unwrap();
    let host = FakeHost::new();
    let ctx = Ctx::with_parts(
        PumpConfig::default(),
        host.clone(),
        dir.path().join("project"),
        String::from("vivi"),
        String::from("cd /somewhere"),
        dir.path().join("home"),
        dir.path().join("home").join("repo"),
    );
    drop(ctx.sh("echo hi", None, 5));
    assert_eq!(host.commands(), vec!["cd /somewhere; echo hi"]);
    let (plain, plain_host) = test_ctx(dir.path(), PumpConfig::default());
    drop(plain.sh("echo bare", None, 5));
    assert_eq!(plain_host.commands(), vec!["echo bare"]);
}

#[test]
fn pool_sources_travel_to_derived_contexts() {
    let dir = tempdir().unwrap();
    let mut cfg = PumpConfig::default();
    cfg.targets.insert(
        String::from("tela"),
        PumpTarget {
            path: String::from("tela"),
            branch: String::from("main"),
            ..PumpTarget::default()
        },
    );
    let (ctx, _host) = test_ctx(dir.path(), cfg);
    let ctx = ctx.with_pool_sources(BTreeMap::from([(
        String::from("glm"),
        PathBuf::from("/wtree/repo"),
    )]));
    let target = ctx.for_target("tela").unwrap();
    assert_eq!(
        target.pool_sources.get("glm"),
        Some(&PathBuf::from("/wtree/repo"))
    );
    let other = ctx.with_repo(PathBuf::from("/other"));
    assert_eq!(
        other.pool_sources.get("glm"),
        Some(&PathBuf::from("/wtree/repo"))
    );
}

#[test]
fn mailspace_travels_to_derived_target_contexts() {
    let dir = tempdir().unwrap();
    let mut cfg = PumpConfig::default();
    cfg.targets.insert(
        String::from("tela"),
        PumpTarget {
            path: String::from("tela"),
            branch: String::from("main"),
            ..PumpTarget::default()
        },
    );
    let (ctx, _host) = test_ctx(dir.path(), cfg);
    let ctx = ctx.with_mailspace(String::from("abc123"));
    assert_eq!(ctx.mailspace, "abc123");
    let target = ctx.for_target("tela").unwrap();
    assert_eq!(target.mailspace, "abc123");
    let (bare, _bare_host) = test_ctx(dir.path(), PumpConfig::default());
    assert_eq!(bare.mailspace, "");
}

#[test]
fn ctx_vivi_appends_the_quoted_project() {
    let dir = tempdir().unwrap();
    let host = FakeHost::new();
    let project = dir.path().join("my project");
    let ctx = Ctx::with_parts(
        PumpConfig::default(),
        host.clone(),
        project.clone(),
        String::from("vivi"),
        String::new(),
        dir.path().join("home"),
        dir.path().join("home").join("repo"),
    );
    let out = ctx.vivi("task list", 5);
    assert_eq!(out.code, 0);
    let expected = format!("vivi task list --project '{}'", project.display());
    assert_eq!(host.commands(), vec![expected]);
}

#[test]
fn ctx_exception_appends_one_line_with_newlines_folded_and_the_text_cut() {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), PumpConfig::default());
    let tail: String = "x".repeat(700);
    ctx.exception("gate", "u1", &format!("line one\nline two\n{tail}"));
    let path = dir.path().join("home").join("exceptions.txt");
    let content = fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 1, "exactly one line, got: {content:?}");
    assert!(lines[0].starts_with("13:46:40 gate u1 "));
    assert!(lines[0].contains("line one | line two"));
    let xs = lines[0].matches('x').count();
    assert!(xs > 500, "kept the head of the text: {xs} chars");
    assert!(xs <= 600, "text is cut at 600 chars: {xs} chars");
    assert_eq!(host.logs(), vec!["EXCEPTION gate u1"]);
}

#[test]
fn fake_host_replies_by_fragment_in_order_and_records_commands() {
    let host = FakeHost::new();
    host.reply("git push", 0, "pushed");
    host.reply("git status", 1, "dirty");
    host.set_now(42);
    assert_eq!(host.now(), 42);
    let a = host.run("git status --short", None, 1);
    assert_eq!((a.code, a.text.as_str()), (1, "dirty"));
    let b = host.run("git push origin main", None, 1);
    assert_eq!((b.code, b.text.as_str()), (0, "pushed"));
    let c = host.run("vivi board", None, 1);
    assert_eq!((c.code, c.text.as_str()), (0, ""));
    assert_eq!(
        host.commands(),
        vec!["git status --short", "git push origin main", "vivi board"]
    );
    host.advance(3);
    assert_eq!(host.now(), 45);
}

#[test]
fn fake_host_reply_once_is_consumed_before_persistent_rules() {
    let host = FakeHost::new();
    host.reply("build", 0, "persistent");
    host.reply_once("build", 9, "once");
    let first = host.run("make build", None, 1);
    assert_eq!((first.code, first.text.as_str()), (9, "once"));
    let second = host.run("make build", None, 1);
    assert_eq!((second.code, second.text.as_str()), (0, "persistent"));
}

#[test]
fn fake_host_treats_an_unscripted_done_need_list_as_empty() {
    let host = FakeHost::new();
    let output = host.run("vivi need list --status done --to mind --json", None, 1);
    assert_eq!((output.code, output.text.as_str()), (0, "[]"));
}
