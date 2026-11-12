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

const MUX_REFUSAL: &str =
    "mux_client_request_session: session request failed: Session open refused by peer";

#[derive(Debug)]
struct Invocation {
    args: Vec<String>,
    stdin: Option<Vec<u8>>,
    timeout: Duration,
}

/// Run the SSH command through a stand-in without changing this process's
/// PATH or touching a real master. It refuses sessions before reading stdin.
struct RefusingSsh {
    cli: SshCli,
    root: tempfile::TempDir,
    refusals: usize,
    close_code: i32,
    calls: std::sync::Mutex<Vec<Invocation>>,
}

impl RefusingSsh {
    fn new(refusals: usize) -> Self {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("ssh.sh"),
            r#"
case " $* " in
    *" -O exit "*) exit "$CLOSE_CODE" ;;
esac
n=0
if [ -f "$COUNT" ]; then read -r n < "$COUNT"; fi
n=$((n + 1))
printf '%s\n' "$n" > "$COUNT"
if [ "$n" -le "$REFUSALS" ]; then
    printf '%s\n' "$REFUSAL" >&2
    exit 255
fi
cat
"#,
        )
        .unwrap();
        Self {
            cli: SshCli {
                host: "worker".into(),
                extra: Vec::new(),
                control_dir: Some(root.path().to_path_buf()),
                owns_control_dir: false,
            },
            root,
            refusals,
            close_code: 0,
            calls: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn invoke(
        &self,
        command: &Command,
        input: Option<&[u8]>,
        limit: Duration,
    ) -> Result<RunOutput, VivariumError> {
        self.calls.lock().unwrap().push(Invocation {
            args: parts(command),
            stdin: input.map(<[u8]>::to_vec),
            timeout: limit,
        });
        let mut fake = Command::new("sh");
        fake.arg(self.root.path().join("ssh.sh"))
            .args(command.get_args())
            .env("COUNT", self.root.path().join("count"))
            .env("REFUSALS", self.refusals.to_string())
            .env("REFUSAL", MUX_REFUSAL)
            .env("CLOSE_CODE", self.close_code.to_string());
        execute(fake, input, limit, "fake worker")
    }
}

impl Runner for RefusingSsh {
    fn run(
        &self,
        args: &[String],
        stdin: Option<&[u8]>,
        timeout: Duration,
    ) -> Result<RunOutput, VivariumError> {
        self.cli
            .run_with(&args[0], stdin, timeout, |command, input, limit| {
                self.invoke(&command, input, limit)
            })
    }
}

/// Count this recovery warning, not warnings from other tests or threads.
#[derive(Clone, Default)]
struct RecoveryLog(std::sync::Arc<AtomicUsize>);

impl tracing::Subscriber for RecoveryLog {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut message = RecoveryMessage(false);
        event.record(&mut message);
        if *event.metadata().level() == tracing::Level::WARN && message.0 {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
}

struct RecoveryMessage(bool);

impl tracing::field::Visit for RecoveryMessage {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}").contains("SSH master refused a session");
        }
    }
}

#[test]
fn a_refused_mux_session_closes_the_master_and_retries_once_with_the_same_input() {
    let ssh = RefusingSsh::new(1);
    let log = RecoveryLog::default();
    // Larger than a pipe buffer: the refused session never reads the input.
    let input = vec![0x7f; 128 * 1024];
    let timeout = Duration::from_secs(3);
    let output = tracing::subscriber::with_default(log.clone(), || {
        ssh.run(&["put a file".into()], Some(&input), timeout)
            .unwrap()
    });
    assert!(output.succeeded(), "{output:?}");
    assert_eq!(output.stdout, input);
    assert_eq!(log.0.load(Ordering::Relaxed), 1);
    let calls = ssh.calls.lock().unwrap();
    assert_eq!(calls.len(), 3, "session, master exit, fresh session");
    assert_eq!(calls[0].args.last().unwrap(), "put a file");
    assert_eq!(
        &calls[1].args[calls[1].args.len() - 4..],
        ["-O", "exit", "--", "worker"]
    );
    assert_eq!(calls[1].stdin, None);
    assert_eq!(calls[1].timeout, MASTER_EXIT_TIMEOUT);
    assert_eq!(
        &calls[2].args[..6],
        [
            "-S",
            "none",
            "-o",
            "ControlMaster=no",
            "-o",
            "ControlPersist=no"
        ]
    );
    assert_eq!(&calls[2].args[6..], calls[0].args);
    for call in [&calls[0], &calls[2]] {
        assert_eq!(call.stdin.as_deref(), Some(input.as_slice()));
        assert_eq!(call.timeout, timeout);
    }
    let path = format!("ControlPath={}", ssh.root.path().join("%C").display());
    assert!(calls[1].args.contains(&path));
}

#[test]
fn a_second_mux_refusal_is_reported_by_the_provider_without_another_retry() {
    use crate::pool::{config::PoolConfig, provider::PoolProvider, ssh::SshProvider};
    // Keep inspection access after moving the runner into the provider.
    struct Shared(std::sync::Arc<RefusingSsh>);
    impl Runner for Shared {
        fn run(
            &self,
            args: &[String],
            stdin: Option<&[u8]>,
            timeout: Duration,
        ) -> Result<RunOutput, VivariumError> {
            self.0.run(args, stdin, timeout)
        }
    }
    let ssh = std::sync::Arc::new(RefusingSsh::new(2));
    let config: PoolConfig =
        toml::from_str("provider = 'ssh'\nslots = 1\n[ssh]\nhost = 'worker'\n").unwrap();
    let provider = SshProvider::with_runner(&config, Box::new(Shared(ssh.clone())));
    let log = RecoveryLog::default();
    let error = tracing::subscriber::with_default(log.clone(), || provider.list()).unwrap_err();
    let said = error.to_string();
    assert!(said.contains("cannot reach worker over ssh"), "{said}");
    assert!(said.contains(MUX_REFUSAL), "{said}");
    assert_eq!(
        ssh.calls.lock().unwrap().len(),
        3,
        "no second master exit or third session"
    );
    assert_eq!(log.0.load(Ordering::Relaxed), 1);
}

#[test]
fn a_failed_master_exit_still_retries_on_a_fresh_connection() {
    let mut ssh = RefusingSsh::new(1);
    ssh.close_code = 255;
    let out = ssh
        .run(&["list".into()], None, Duration::from_secs(3))
        .unwrap();
    assert!(out.succeeded());
    let calls = ssh.calls.lock().unwrap();
    assert_eq!(calls.len(), 3);
    assert_eq!(&calls[2].args[..2], ["-S", "none"]);
}

#[test]
fn unrelated_failures_and_success_are_not_retried() {
    for (code, stderr) in [
        (
            255,
            "ssh: connect to host worker port 22: Connection refused",
        ),
        (255, "Permission denied (publickey)."),
        (255, "Connection to worker closed by remote host."),
        (1, "remote command failed"),
        (1, "Session open refused by peer"),
        (0, MUX_REFUSAL),
    ] {
        let expected = RunOutput {
            code,
            stdout: b"output".to_vec(),
            stderr: stderr.into(),
        };
        let mut calls = 0;
        let actual = unshared("worker")
            .run_with("exec", None, Duration::from_secs(1), |_, _, _| {
                calls += 1;
                Ok(expected.clone())
            })
            .unwrap();
        assert_eq!(actual, expected);
        assert_eq!(calls, 1, "{stderr}");
    }
}

#[test]
fn spawn_and_timeout_errors_are_not_retried() {
    let mut calls = 0;
    let error = unshared("worker")
        .run_with("exec", None, Duration::from_secs(1), |_, _, _| {
            calls += 1;
            Err(VivariumError::Message(
                "ssh did not finish within 1s".into(),
            ))
        })
        .unwrap_err();
    assert_eq!(calls, 1);
    assert!(error.to_string().contains("did not finish"));
}

#[test]
fn recovering_a_pinned_connection_uses_its_private_socket_and_port() {
    let cli = SshCli::pinned("worker", 2222, std::path::Path::new("/dev/null"));
    let args = parts(&cli.master_exit_command());
    let path = cli.control_path(cli.control_dir.as_ref().unwrap());
    assert!(args.contains(&format!("ControlPath={}", path.display())));
    assert!(args.windows(2).any(|pair| pair == ["-p", "2222"]));
    assert!(args.contains(&"StrictHostKeyChecking=yes".into()));
    let fresh = parts(&cli.session_command("true", false));
    assert!(fresh.windows(2).any(|pair| pair == ["-p", "2222"]));
    assert!(fresh.contains(&"StrictHostKeyChecking=yes".into()));
}
