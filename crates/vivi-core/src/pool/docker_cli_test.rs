use super::*;

fn sh() -> DockerCli {
    DockerCli {
        bin: "sh".to_string(),
        host: None,
    }
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(ToString::to_string).collect()
}

#[test]
fn a_local_daemon_sets_no_host() {
    let command = DockerCli::new(None).command(&args(&["ps", "-a"]));
    assert_eq!(command.get_program(), "docker");
    let given: Vec<_> = command.get_args().collect();
    assert_eq!(given, ["ps", "-a"]);
    assert!(command.get_envs().all(|(k, _)| k != "DOCKER_HOST"));
}

#[test]
fn a_remote_daemon_goes_through_docker_host_and_never_through_an_argument() {
    let command = DockerCli::new(Some("ssh://ci@build-box")).command(&args(&["ps"]));
    let host = command
        .get_envs()
        .find(|(k, _)| *k == "DOCKER_HOST")
        .and_then(|(_, v)| v)
        .unwrap();
    assert_eq!(host, "ssh://ci@build-box");
    assert!(
        command
            .get_args()
            .all(|a| a != "-H" && !a.to_string_lossy().contains("ssh://"))
    );
}

#[test]
fn output_exit_code_and_stderr_are_returned_not_raised() {
    let out = sh()
        .run(
            &args(&["-c", "echo out; echo err >&2; exit 3"]),
            None,
            CALL_TIMEOUT,
        )
        .unwrap();
    assert_eq!(
        (out.code, out.stdout_text(), out.stderr.trim()),
        (3, "out".into(), "err")
    );
    assert!(!out.succeeded());
}

#[test]
fn standard_input_reaches_the_program() {
    let out = sh()
        .run(
            &args(&["-c", "cat"]),
            Some(b"fed through stdin"),
            CALL_TIMEOUT,
        )
        .unwrap();
    assert_eq!(out.stdout_text(), "fed through stdin");
}

#[test]
fn a_call_past_its_limit_is_killed_and_reported() {
    let err = sh()
        .run(&args(&["-c", "sleep 5"]), None, Duration::from_millis(150))
        .unwrap_err()
        .to_string();
    assert!(err.contains("did not finish within"), "{err}");
}

#[test]
fn a_missing_program_names_docker_and_the_path() {
    let cli = DockerCli {
        bin: "docker-that-does-not-exist".to_string(),
        host: None,
    };
    let err = cli
        .run(&args(&["ps"]), None, CALL_TIMEOUT)
        .unwrap_err()
        .to_string();
    assert!(err.contains("is Docker installed and on PATH"), "{err}");
}
