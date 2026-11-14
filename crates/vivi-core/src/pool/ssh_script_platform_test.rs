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

/// What a Darwin `ps` prints for the group searches: a slot's boot group (12),
/// an agent group (20), a recorded id now led by another process (30), an
/// orphaned agent group whose leader is gone (41) and an unrelated group (50).
const DARWIN_PS: &str = r#"
ps() {
  echo "$*" >> "$ROOT/ps-calls"
  case "$*" in
    '-A -o pid=,pgid=,lstart=')
      echo '   12    12 Fri Nov 13 10:00:00 2026   '
      echo '   13    12 Fri Nov 13 10:00:01 2026   '
      echo '   20    20 Fri Nov 13 10:05:00 2026   '
      echo '   21    20 Fri Nov 13 10:05:01 2026   '
      echo '   30    30 Sat Nov 14 09:00:00 2026   '
      echo '   31    30 Sat Nov 14 09:00:01 2026   '
      echo '   40    41 Fri Nov 13 10:06:01 2026   '
      echo '   50    50 Fri Nov 13 10:07:00 2026   '
      ;;
    '-o lstart= -p 12') echo 'Fri Nov 13 10:00:00 2026   ' ;;
    '-o pgid= -p 12') echo '   12' ;;
    '-o lstart= -p 99' | '-o pgid= -p 99') ;;
    *) echo "unexpected ps arguments: $*" >&2; return 99 ;;
  esac
}
"#;

#[test]
fn darwin_finds_a_slot_by_its_process_groups_and_rejects_reused_ids() {
    let output = run(
        "Darwin",
        DARWIN_PS,
        r#"
test "$SYSTEM" = Darwin
mkdir -p "$ROOT/job"
echo 12 > "$ROOT/boot.pid"
echo 'Fri Nov 13 10:00:00 2026' > "$ROOT/boot.start"
printf '%s\n' '20|Fri Nov 13 10:05:00 2026' '30|Fri Nov 13 10:30:00 2026' '41|Fri Nov 13 10:06:00 2026' > "$ROOT/job/agent.groups"
test "$(tagged "$ROOT" | tr '\n' ' ')" = '12 13 20 21 40 '
alive "$ROOT"
test "$(slot_pids "$ROOT")" = '12 13 20 21 40 '
echo 'Thu Nov 12 08:00:00 2026' > "$ROOT/boot.start"
! alive "$ROOT"
test "$(tagged "$ROOT" | tr '\n' ' ')" = '20 21 40 '
rm "$ROOT/boot.start"
alive "$ROOT"
echo 99 > "$ROOT/boot.pid"
! alive "$ROOT"
echo invalid > "$ROOT/boot.pid"
! alive "$ROOT"
rm "$ROOT/boot.pid" "$ROOT/job/agent.groups"
! alive "$ROOT"
test -z "$(tagged "$ROOT")"
cat "$ROOT/ps-calls"
"#,
    );
    assert!(
        !output.contains("-E"),
        "macOS shows no environments: never search them"
    );
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
fn the_runner_finds_leftover_agent_processes_by_their_groups_on_darwin() {
    let runner = crate::pool::job::RUNNER;
    let start = runner.find("  agent_job_pids() {").unwrap();
    let end = start + runner[start..].find("\n  }\n").unwrap() + "\n  }".len();
    let body = format!(
        r#"
{}
AGENT_TRACKING=groups
AGENT_GROUPS=$ROOT/agent.groups
printf '%s\n' '20|Fri Nov 13 10:05:00 2026' '30|Fri Nov 13 10:30:00 2026' > "$AGENT_GROUPS"
test "$(agent_job_pids | sort -n | tr '\n' ' ')" = '20 21 '
test -s "$AGENT_GROUPS"
printf '%s\n' '30|Fri Nov 13 10:30:00 2026' '60|Fri Nov 13 10:40:00 2026' > "$AGENT_GROUPS"
test -z "$(agent_job_pids)"
test ! -s "$AGENT_GROUPS"
! grep -q -- -E "$ROOT/ps-calls"
"#,
        &runner[start..end]
    );
    run("Darwin", DARWIN_PS, &body);
}
