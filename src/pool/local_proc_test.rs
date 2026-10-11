//! The slot's processes, for real: bash scripts as boot scripts, on this
//! platform. Anything that only one platform can do is gated, not skipped.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::*;

/// A slot directory holding the given boot script.
fn slot(boot: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().canonicalize().unwrap().join("slot-1");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("slot.env"), "").unwrap();
    std::fs::write(work.join("boot.sh"), boot).unwrap();
    (dir, work)
}

fn until(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn pid_in(work: &Path, name: &str) -> u32 {
    until(name, || work.join(name).exists());
    until("the pid to be complete", || {
        std::fs::read_to_string(work.join(name)).is_ok_and(|t| t.trim().parse::<u32>().is_ok())
    });
    std::fs::read_to_string(work.join(name))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

fn running(pid: u32) -> bool {
    identity(pid).is_some()
}

#[test]
fn the_listing_of_ps_is_read_with_its_start_time_and_zombies() {
    let text = "    1     0     1 Ss   Sat Oct 10 13:17:08 2026\n 4227  4226  4226 S    Sat Oct 10 13:25:41 2026\n 99 1 99 Z+ Sun Oct 11 07:30:33 2026\nnot a line\n";
    let list = parse_listing(text);
    assert_eq!(list.len(), 3);
    assert_eq!(
        (list[1].pid, list[1].ppid, list[1].pgid),
        (4227, 4226, 4226)
    );
    assert_eq!(list[1].started, "Sat Oct 10 13:25:41 2026");
    assert!(!list[0].zombie && list[2].zombie);
}

#[test]
fn descendants_follow_parents_all_the_way_down() {
    let p = |pid, ppid| Proc {
        pid,
        ppid,
        pgid: pid,
        zombie: false,
        started: String::new(),
    };
    let list = vec![p(1, 0), p(10, 1), p(11, 10), p(12, 11), p(20, 1), p(30, 99)];
    let found = descendants(10, &list);
    assert_eq!(found.into_iter().collect::<Vec<_>>(), vec![11, 12]);
}

#[test]
fn a_started_slot_runs_detached_and_a_stop_ends_it() {
    let (_dir, work) = slot("echo $$ > boot-script.pid\nsleep 60\n");
    start(&work).unwrap();
    assert!(is_alive(&work));
    let boot = boot_pid(&work).unwrap();
    assert!(
        std::fs::read_to_string(work.join(BOOT_START))
            .unwrap()
            .trim()
            .len()
            > 10
    );
    let script = pid_in(&work, "boot-script.pid");
    assert!(running(script));
    // A second start while it runs does nothing.
    start(&work).unwrap();
    assert_eq!(boot_pid(&work), Some(boot));
    stop(&work).unwrap();
    assert!(!is_alive(&work));
    until("the processes to end", || {
        !running(boot) && !running(script)
    });
    for name in [BOOT_PID, BOOT_START, EXIT_CODE] {
        assert!(!work.join(name).exists(), "{name} is forgotten");
    }
}

#[test]
fn a_boot_that_ends_leaves_its_exit_code_and_nothing_running() {
    let (_dir, work) = slot("sleep 40 &\necho $! > left.pid\nexit 7\n");
    start(&work).unwrap();
    until("the exit code", || work.join(EXIT_CODE).exists());
    assert_eq!(
        std::fs::read_to_string(work.join(EXIT_CODE))
            .unwrap()
            .trim(),
        "7"
    );
    until("the slot to read stopped", || !is_alive(&work));
    // Nothing the boot started outlives it.
    let left = pid_in(&work, "left.pid");
    until("the leftover child to be killed", || !running(left));
    reap();
}

#[test]
fn a_stop_reaches_a_child_in_a_group_of_its_own() {
    let (_dir, work) = slot("set -m\nsleep 60 &\necho $! > child.pid\nwait\n");
    start(&work).unwrap();
    let child = pid_in(&work, "child.pid");
    let listing = listing().unwrap();
    let mine = listing.iter().find(|p| p.pid == child).unwrap();
    assert_eq!(mine.pgid, child, "the child leads a group of its own");
    assert!(
        slot_pids(&work, &listing).contains(&child),
        "found by parentage"
    );
    stop(&work).unwrap();
    until("the child to end", || !running(child));
}

#[test]
fn a_stop_leaves_no_child_born_while_it_runs() {
    // Boots that keep starting children, each in a group of its own (the way
    // the job runner's `timeout` does), under parents that are ended too:
    // stopping must not orphan the newest child.
    let looper =
        "set -m; for n in 1 2 3 4 5 6 7 8 9 10 11 12; do sleep 61.9 & sleep 0.02; done; wait";
    let boot = format!(
        "set -m\nfor n in 1 2 3 4; do\n  bash -c '{looper}' &\n  bash -c 'sleep 61.7 & bash -c \"sleep 61.8 & wait\" & wait' &\ndone\necho up > up\nwait\n"
    );
    let (_tmp, work) = slot(&boot);
    start(&work).unwrap();
    until("the boot to be up", || work.join("up").exists());
    std::thread::sleep(Duration::from_millis(100));
    stop(&work).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let out = Command::new("ps")
        .args(["-A", "-o", "command="])
        .output()
        .unwrap();
    let left: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.starts_with("sleep 61.") || l.starts_with("bash -c sleep 61."))
        .map(str::to_string)
        .collect();
    assert!(left.is_empty(), "left behind: {left:?}");
}

#[test]
fn a_reused_process_id_is_not_taken_for_the_slots_boot() {
    // The slot's boot is gone and its pid now belongs to a stranger: a process
    // that is no part of the slot and carries no slot tag.
    let (_dir, work) = slot("exit 0\n");
    let mut stranger = Command::new("sleep").arg("60").spawn().unwrap();
    std::fs::write(work.join(BOOT_PID), format!("{}\n", stranger.id())).unwrap();
    std::fs::write(work.join(BOOT_START), "Mon Jan  1 00:00:00 2001\n").unwrap();
    assert!(!is_alive(&work), "the start time differs: another process");
    assert!(
        slot_pids(&work, &listing().unwrap()).is_empty(),
        "nothing of a stranger is touched"
    );
    stop(&work).unwrap();
    assert!(
        stranger.try_wait().unwrap().is_none(),
        "a stop did not signal the stranger"
    );
    stranger.kill().unwrap();
    stranger.wait().unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn on_linux_the_slots_own_tagged_processes_are_stopped_even_when_the_recorded_identity_is_stale() {
    // They are the slot's, not a stranger's: they carry its tag from the
    // wrapper down. A stale boot.start only means the wrapper itself is not
    // trusted by pid; the tag still names what belongs to the slot.
    let (_dir, work) = slot("sleep 61.3\n");
    start(&work).unwrap();
    let boot = boot_pid(&work).unwrap();
    std::fs::write(work.join(BOOT_START), "Mon Jan  1 00:00:00 2001\n").unwrap();
    assert!(!is_alive(&work));
    let found = slot_pids(&work, &listing().unwrap());
    assert!(!found.is_empty(), "the tag finds the slot's own processes");
    stop(&work).unwrap();
    until("the slot's processes to end", || {
        !running(boot) && slot_pids(&work, &listing().unwrap()).is_empty()
    });
}

#[test]
fn this_process_is_never_among_the_pids_of_a_slot() {
    let (_dir, work) = slot("sleep 60\n");
    start(&work).unwrap();
    let boot = boot_pid(&work).unwrap();
    let real_start = identity(boot).unwrap();
    // Point the slot's files at this test process.
    let me = std::process::id();
    std::fs::write(work.join(BOOT_PID), format!("{me}\n")).unwrap();
    std::fs::write(
        work.join(BOOT_START),
        format!("{}\n", identity(me).unwrap()),
    )
    .unwrap();
    assert!(!slot_pids(&work, &listing().unwrap()).contains(&me));
    // Put the real boot back so that it can be stopped.
    std::fs::write(work.join(BOOT_PID), format!("{boot}\n")).unwrap();
    std::fs::write(work.join(BOOT_START), format!("{real_start}\n")).unwrap();
    stop(&work).unwrap();
    until("the boot to end", || !running(boot));
}

#[test]
fn a_dead_slot_has_nothing_to_stop() {
    let (_dir, work) = slot("exit 0\n");
    stop(&work).unwrap();
    assert!(!is_alive(&work));
    assert!(slot_pids(&work, &listing().unwrap()).is_empty());
}

#[cfg(target_os = "linux")]
#[test]
fn on_linux_a_process_that_left_the_group_and_the_tree_is_found_by_its_tag() {
    // A double fork with a session of its own: neither group nor parentage
    // reach it; only the slot's tag in its environment does.
    let (_dir, work) = slot("setsid bash -c 'echo $$ > escaped.pid; exec sleep 60' &\nwait\n");
    start(&work).unwrap();
    let escaped = pid_in(&work, "escaped.pid");
    let boot = boot_pid(&work).unwrap();
    assert!(slot_pids(&work, &listing().unwrap()).contains(&escaped));
    // Kill only the boot: the escapee is an orphan now, and the tag still finds it.
    super::signal("KILL", &std::collections::BTreeSet::from([boot]));
    until("the boot to end", || !is_alive(&work));
    assert!(running(escaped));
    stop(&work).unwrap();
    until("the escaped process to end", || !running(escaped));
}

#[cfg(not(target_os = "linux"))]
#[test]
fn without_proc_there_is_no_tag_scan_and_the_limit_is_the_documented_one() {
    // macOS has no readable /proc and hides the environment of protected
    // programs, so a process that left both the group and the tree is not
    // found there. What is found is the group, the tree and the identity.
    let (_dir, work) = slot("sleep 60\n");
    start(&work).unwrap();
    assert!(tagged(&work).is_empty());
    stop(&work).unwrap();
}
