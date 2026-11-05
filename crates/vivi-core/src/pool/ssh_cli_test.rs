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
        extra: Vec::new(),
        control_dir: None,
        owns_control_dir: true,
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

/// The pump runs `vivi pool ...` as one subprocess per call, so a connection
/// shared only inside one process was never reused: every call logged in
/// again (the lab stress run: about 57 SSH logins per pump cycle). Runners in
/// every process share one connection per destination.
#[test]
fn calls_share_one_connection_per_destination_with_every_vivi_process_of_this_user() {
    let cli = SshCli::new("pharos");
    let dir = cli.control_dir.clone().expect("the shared directory");
    assert_eq!(Some(dir.clone()), shared_control_dir());
    let args = cli.options();
    assert!(args.contains(&"ControlMaster=auto".to_string()));
    let path = format!("ControlPath={}", dir.join("%C").display());
    assert!(args.contains(&path), "{path}: {args:?}");
    assert!(args.contains(&format!("ControlPersist={PERSIST_SECS}")));
    let mode = std::fs::metadata(&dir).unwrap();
    assert_eq!(
        std::os::unix::fs::PermissionsExt::mode(&mode.permissions()) & 0o777,
        0o700
    );
    assert!(dir.as_os_str().len() < 80, "short enough for a socket path");
    drop(cli);
    assert!(dir.exists(), "the shared directory outlives one runner");
}

#[test]
fn a_pinned_runner_keeps_a_private_socket_that_ends_with_it() {
    let cli = SshCli::pinned("h", 2222, std::path::Path::new("/dev/null"));
    let dir = cli.control_dir.clone().expect("a private directory");
    assert_ne!(Some(dir.clone()), shared_control_dir());
    let path = format!("ControlPath={}", dir.join("m").display());
    assert!(cli.options().contains(&path));
    drop(cli);
    assert!(!dir.exists(), "the private directory is removed");
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
fn two_runners_for_one_host_share_its_connection() {
    let (a, b) = (SshCli::new("h"), SshCli::new("h"));
    assert_eq!(a.options(), b.options());
}

#[test]
fn a_socket_directory_others_can_enter_or_a_symlink_is_never_shared() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let root = tempfile::tempdir().unwrap();
    let uid = std::fs::metadata(root.path()).unwrap().uid();
    let private = root.path().join("private");
    std::fs::create_dir(&private).unwrap();
    std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(private_to(&private, uid));
    assert!(!private_to(&private, uid + 1), "someone else's directory");
    let open = root.path().join("open");
    std::fs::create_dir(&open).unwrap();
    std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!private_to(&open, uid), "others can enter it");
    let link = root.path().join("link");
    std::os::unix::fs::symlink(&private, &link).unwrap();
    assert!(!private_to(&link, uid), "a symlink");
}

/// A Unix socket path is limited to about 104 bytes, and ssh adds a random
/// suffix of about 17 while it binds. On macOS the per-user temporary
/// directory is long enough that a `%C` socket there did not fit, and ssh
/// connected without sharing (seen on the laptop's local pump).
#[test]
fn a_shared_socket_path_fits_the_unix_limit() {
    let dir = shared_control_dir().expect("the shared directory");
    let socket = dir.join("0123456789abcdef0123456789abcdef01234567");
    assert!(
        socket.as_os_str().len() + 17 < 104,
        "{} is too long for a socket",
        socket.display()
    );
}
