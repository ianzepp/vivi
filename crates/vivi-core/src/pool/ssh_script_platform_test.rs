//! Exercise the SSH script's platform branches without a macOS host.
use std::process::Command;

use super::SLOT_SCRIPT;

fn run(system: &str, setup: &str, body: &str) -> String {
    let root = tempfile::tempdir().unwrap();
    let functions = SLOT_SCRIPT.split("case \"$VERB\" in").next().unwrap();
    let text = format!(
        "set -e\nuname() {{ if [ \"$1\" = -m ]; then echo arm64; else echo {system}; fi; }}\nROOT=$TEST_ROOT; UMASK=077\n{setup}\n{functions}\n{body}"
    );
    let out = Command::new("/bin/bash")
        .args(["-c", &text])
        .env("TEST_ROOT", root.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn darwin_finds_only_exact_slot_tokens_and_checks_the_boot_pid_with_ps() {
    let output = run(
        "Darwin",
        r#"
ps() {
  echo "$*" >> "$ROOT/ps-calls"
  case "$*" in
    '-A -ww -E -o pid=,command=')
      printf '  12 bash VIVI_SLOT_ROOT=%s OTHER=x\n' "$ROOT"
      printf '  13 bash VIVI_SLOT_ROOT=%s/other\n' "$ROOT"
      printf '  14 bash NOT_VIVI_SLOT_ROOT=%s\n' "$ROOT"
      printf '  15 bash X=VIVI_SLOT_ROOT=%s\n' "$ROOT"
      printf '  16 compiler VIVI_SLOT_ROOT=%s\n' "$ROOT"
      ;;
    '-ww -E -o command= -p 12') printf 'bash OTHER=x VIVI_SLOT_ROOT=%s\n' "$ROOT" ;;
    '-ww -E -o command= -p 13') printf 'bash VIVI_SLOT_ROOT=%s/other\n' "$ROOT" ;;
    *) echo 'unexpected ps arguments' >&2; return 99 ;;
  esac
}
"#,
        r#"
test "$SYSTEM" = Darwin
test "$(tagged "$ROOT")" = $'12\n16'
echo 12 > "$ROOT/boot.pid"
alive "$ROOT"
test "$(slot_pids "$ROOT")" = '12 16 '
echo 13 > "$ROOT/boot.pid"
! alive "$ROOT"
echo invalid > "$ROOT/boot.pid"
! alive "$ROOT"
rm "$ROOT/boot.pid"
! alive "$ROOT"
cat "$ROOT/ps-calls"
"#,
    );
    assert!(output.contains("-A -ww -E -o pid=,command="));
    assert!(output.contains("-ww -E -o command= -p 12"));
    assert!(
        !output.contains("sid="),
        "Darwin never uses the Linux session query"
    );
}

const NO_SETSID: &str = r#"
command() {
  if [ "$*" = '-v setsid' ]; then return 1; fi
  builtin command "$@"
}
"#;

const NO_PROC: &str = r#"
[() {
  if [[ $1 == -r && $2 == /proc/self/environ ]]; then return 1; fi
  builtin [ "$@"
}
"#;

#[test]
fn darwin_detaches_with_perl_when_setsid_is_absent_and_probe_has_no_proc_requirement() {
    let output = run(
        "Darwin",
        &format!("{NO_SETSID}\n{NO_PROC}"),
        r#"
nohup() { printf '<%s>\n' "$@"; }
detach bash -l -c 'echo boot'
probe
"#,
    );
    assert!(
        output.starts_with("<perl>\n<-MPOSIX>\n<-e>\n<POSIX::setsid()"),
        "{output}"
    );
    assert!(output.contains("exec @ARGV"));
    assert!(output.contains("<bash>\n<-l>\n<-c>\n<echo boot>"));
    assert!(output.contains("system=Darwin arm64"));
    assert!(!output.contains("missing=/proc"));
    assert!(!output.contains("missing=setsid"));
    let linux = run("Linux", &format!("{NO_SETSID}\n{NO_PROC}"), "probe");
    assert!(linux.contains("missing=setsid"));
    assert!(linux.contains("missing=/proc"));
}

#[test]
fn an_available_setsid_is_still_preferred() {
    for system in ["Darwin", "Linux"] {
        let output = run(
            system,
            r#"
command() { if [ "$*" = '-v setsid' ]; then return 0; fi; builtin command "$@"; }
"#,
            r#"
nohup() { printf '<%s>\n' "$@"; }
detach bash -l -c 'echo boot'
"#,
        );
        assert_eq!(output, "<setsid>\n<bash>\n<-l>\n<-c>\n<echo boot>\n");
    }
}

#[test]
fn the_perl_fallback_really_executes_in_a_new_session() {
    run(
        "Darwin",
        NO_SETSID,
        r"
detach perl -e 'exit(getpgrp(0) == $$ ? 0 : 1)'
",
    );
}

#[cfg(target_os = "linux")]
#[test]
fn linux_still_finds_tagged_environments_in_proc_and_rejects_reused_pids() {
    run(
        "Linux",
        "ps() { echo 'Linux tagged/alive must not invoke ps' >&2; return 99; }",
        r#"
env VIVI_SLOT_ROOT="$ROOT" sleep 30 &
p=$!
trap 'kill "$p" 2>/dev/null || true' EXIT
echo "$p" > "$ROOT/boot.pid"
for i in $(seq 100); do alive "$ROOT" && break; sleep 0.01; done
alive "$ROOT"
tagged "$ROOT" | grep -qx "$p"
echo $$ > "$ROOT/boot.pid"
! alive "$ROOT"
"#,
    );
}

#[test]
fn the_runner_also_uses_darwin_environment_tags_to_stop_leftover_agent_processes() {
    let runner = crate::pool::job::RUNNER;
    let start = runner.find("  agent_job_pids() {").unwrap();
    let end = start + runner[start..].find("\n  }\n").unwrap() + "\n  }".len();
    let body = format!(
        r#"
{}
JOB=example
test "$(agent_job_pids)" = '42'
"#,
        &runner[start..end]
    );
    run(
        "Darwin",
        r#"
ps() {
  test "$*" = '-A -ww -E -o pid=,command=' || return 99
  echo '42 compiler VIVI_AGENT_JOB=example'
  echo '43 compiler VIVI_AGENT_JOB=example-other'
  echo '44 compiler NOT_VIVI_AGENT_JOB=example'
}
"#,
        &body,
    );
}
