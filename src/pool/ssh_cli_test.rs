use super::*;

fn parts(command: &Command) -> Vec<String> {
    command
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
}

fn unshared(host: &str) -> SshCli {
    SshCli {
        host: host.to_string(),
        control_dir: None,
    }
}

#[test]
fn a_call_never_prompts_and_gives_up_connecting() {
    let args = parts(&unshared("pharos").command("echo hi"));
    for needed in ["BatchMode=yes", "ConnectTimeout=10", "RequestTTY=no"] {
        assert!(args.iter().any(|a| a == needed), "{needed}: {args:?}");
    }
    assert!(args.contains(&"-T".to_string()));
}

#[test]
fn the_host_follows_a_double_dash_and_the_remote_command_is_one_word() {
    let args = parts(&unshared("-oProxyCommand=x").command("bash -c 'a b'"));
    let n = args.len();
    assert_eq!(&args[n - 3..], ["--", "-oProxyCommand=x", "bash -c 'a b'"]);
}

#[test]
fn forwards_and_configured_remote_commands_cannot_change_the_call() {
    let args = parts(&unshared("h").command("true"));
    assert!(args.contains(&"ClearAllForwardings=yes".to_string()));
    assert!(args.contains(&"RemoteCommand=none".to_string()));
}

#[test]
fn calls_share_one_connection_through_a_private_socket_that_ends_with_the_runner() {
    let cli = SshCli::new("pharos");
    let dir = cli.control_dir.clone().expect("a private directory");
    let args = cli.options();
    assert!(args.contains(&"ControlMaster=auto".to_string()));
    let path = format!("ControlPath={}", dir.join("m").display());
    assert!(args.contains(&path), "{path}: {args:?}");
    let mode = std::fs::metadata(&dir).unwrap();
    assert_eq!(
        std::os::unix::fs::PermissionsExt::mode(&mode.permissions()) & 0o777,
        0o700
    );
    assert!(dir.as_os_str().len() < 80, "short enough for a socket path");
    drop(cli);
    assert!(!dir.exists(), "the socket directory is removed");
}

#[test]
fn an_unshared_runner_sets_no_control_options() {
    let args = unshared("h").options();
    assert!(args.iter().all(|a| !a.starts_with("Control")));
}

#[test]
fn a_missing_ssh_binary_is_named() {
    let cli = unshared("h");
    // The real `ssh` may or may not exist; only check the error text shape
    // when it does not.
    let result = cli.run(&["true".to_string()], None, Duration::from_millis(1));
    if let Err(e) = result {
        let said = e.to_string();
        assert!(said.contains("ssh"), "an error always names ssh: {said}");
    }
}

#[test]
fn two_runners_in_one_process_each_get_a_connection_of_their_own() {
    let (a, b) = (SshCli::new("h"), SshCli::new("h"));
    let (da, db) = (
        a.control_dir.clone().unwrap(),
        b.control_dir.clone().unwrap(),
    );
    assert_ne!(da, db);
    drop(a);
    assert!(db.exists(), "ending one leaves the other's socket");
}
