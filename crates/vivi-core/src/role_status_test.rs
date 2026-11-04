use super::*;

#[test]
fn no_pid_is_not_set() {
    assert!(matches!(
        probe(None, None, None).state,
        ProcessState::NotSet
    ));
}

#[test]
fn subagent_harness_ignores_pid() {
    let report = probe(Some(1), Some("localhost"), Some("subagent"));
    assert!(matches!(report.state, ProcessState::Subagent));
    assert!(report.running.is_none());
}

#[test]
fn non_subagent_harness_probes_a_dead_pid() {
    // Use a PID that is extremely unlikely to exist. The harness is not
    // subagent, so the probe must reach the OS and report Dead. Host is
    // unset so the probe assumes local rather than classifying remote.
    let report = probe(Some(999_999), None, Some("tmux"));
    assert!(matches!(report.state, ProcessState::Dead));
    assert_eq!(report.running, Some(false));
}

#[test]
fn host_is_remote_classifies_cross_host_correctly() {
    // Stored host differs from local -> remote (never a false dead).
    assert!(host_is_remote(Some("pharos"), Some("burgus")));
    // Same host -> local probe.
    assert!(!host_is_remote(Some("burgus"), Some("burgus")));
    // No stored host -> assume local (cannot disagree).
    assert!(!host_is_remote(None, Some("burgus")));
    // Local hostname unreadable -> assume local rather than guessing remote.
    assert!(!host_is_remote(Some("pharos"), None));
}

#[test]
fn classify_maps_process_status() {
    assert!(matches!(
        classify(ProcessStatus::Zombie),
        ProcessState::Zombie
    ));
    assert!(matches!(classify(ProcessStatus::Dead), ProcessState::Dead));
    assert!(matches!(classify(ProcessStatus::Run), ProcessState::Alive));
    assert!(matches!(
        classify(ProcessStatus::Sleep),
        ProcessState::Alive
    ));
    assert!(matches!(
        classify(ProcessStatus::Unknown(99)),
        ProcessState::Unknown
    ));
}

#[test]
fn not_set_and_dead_reports_carry_no_resource_detail() {
    let not_set = not_set();
    assert!(matches!(not_set.state, ProcessState::NotSet));
    assert!(not_set.name.is_none() && not_set.running.is_none());

    let dead = dead();
    assert!(matches!(dead.state, ProcessState::Dead));
    assert_eq!(dead.running, Some(false));
    assert!(dead.name.is_none());
}
