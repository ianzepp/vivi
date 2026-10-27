//! The server's command line, as `vivi server run`. The server boots on any argument it does not
//! understand, so each case here runs the real binary under a deadline: a
//! binary that started serving would never exit and fails the test.

use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Run the binary with `VIVI_HOME` set to `home`, under a deadline.
fn wait(args: &[&str], home: &std::path::Path) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_vivi"));
    command
        .args(["server", "run"])
        .args(args)
        .env("VIVI_HOME", home)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("spawn vivi-server");
    let deadline = Instant::now() + Duration::from_secs(10);
    while child.try_wait().expect("poll vivi-server").is_none() {
        if Instant::now() >= deadline {
            child.kill().expect("kill vivi-server");
            panic!("vivi-server {args:?} did not exit: it started serving");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    child
        .wait_with_output()
        .expect("collect vivi-server output")
}

/// A fresh server home whose `config.toml` holds `config`.
fn scratch_home(tag: &str, config: &str) -> std::path::PathBuf {
    let home = std::env::temp_dir().join(format!("vivi-server-cli-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("server")).expect("create server home");
    std::fs::write(home.join("server/config.toml"), config).expect("write config.toml");
    home
}

fn run(args: &[&str]) -> Output {
    // Tests run in parallel: each call needs its own home.
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let home = scratch_home(&format!("args{n}"), "port = 0\n");
    let out = wait(args, &home);
    assert!(
        !home.join("server/data").exists(),
        "vivi-server {args:?} created its data directory"
    );
    let _ = std::fs::remove_dir_all(&home);
    out
}

#[test]
fn version_prints_name_and_crate_version() {
    for flag in ["--version", "-V"] {
        let out = run(&[flag]);
        assert!(out.status.success(), "{flag}: {:?}", out.status);
        let line = String::from_utf8_lossy(&out.stdout);
        assert_eq!(
            line.trim_end(),
            format!("vivi-server {}", env!("CARGO_PKG_VERSION"))
        );
    }
}

#[test]
fn help_prints_usage_and_config() {
    for flag in ["--help", "-h"] {
        let out = run(&[flag]);
        assert!(out.status.success(), "{flag}: {:?}", out.status);
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("Usage: vivi server run"), "{text}");
        assert!(text.contains("config.toml"), "{text}");
        assert!(text.contains("STORE_ENCRYPTION_KEY"), "{text}");
    }
}

#[test]
fn an_unknown_argument_exits_2_without_serving() {
    let out = run(&["--halp"]);
    assert_eq!(out.status.code(), Some(2));
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(text.contains("unrecognized argument: --halp"), "{text}");
    assert!(text.contains("--help"), "{text}");
    assert!(out.stdout.is_empty());
}

#[test]
fn an_address_already_in_use_exits_1_before_the_data_dir_is_touched() {
    let taken = std::net::TcpListener::bind("127.0.0.1:0").expect("hold a port");
    let port = taken.local_addr().expect("local addr").port().to_string();
    let home = scratch_home("taken", &format!("bind = \"127.0.0.1\"\nport = {port}\n"));
    let out = wait(&[], &home);
    assert_eq!(out.status.code(), Some(1));
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(text.contains("cannot bind 127.0.0.1"), "{text}");
    assert!(
        !text.contains("panicked"),
        "a bind failure must be an error exit, not a panic: {text}"
    );
    assert!(
        !home.join("server/data").exists(),
        "the data directory was created before bind"
    );
    let _ = std::fs::remove_dir_all(&home);
}
