use std::os::unix::fs::PermissionsExt;

use super::*;

const SECRET: &str = "rpa_TESTKEYVALUE0123456789";

fn key_file(dir: &Path, mode: u32, text: &str) -> PathBuf {
    let path = dir.join("runpod.key");
    std::fs::write(&path, text).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
    path
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|a| (*a).to_string()).collect()
}

#[test]
fn a_key_file_only_its_owner_can_read_is_accepted() {
    let dir = tempfile::tempdir().unwrap();
    for mode in [0o600, 0o400] {
        check_key_file(&key_file(dir.path(), mode, SECRET)).unwrap();
    }
}

#[test]
fn a_key_file_open_to_group_or_others_is_refused_with_the_fix() {
    let dir = tempfile::tempdir().unwrap();
    for mode in [0o640, 0o604, 0o644, 0o660, 0o666, 0o700 | 0o010] {
        let path = key_file(dir.path(), mode, SECRET);
        let said = check_key_file(&path).unwrap_err().to_string();
        assert!(
            said.contains("readable by group or others"),
            "{mode:o}: {said}"
        );
        assert!(said.contains("chmod 600"), "{said}");
        assert!(!said.contains(SECRET), "the refusal never carries the key");
    }
}

#[test]
fn a_missing_or_non_file_key_is_refused_without_reading_anything() {
    let dir = tempfile::tempdir().unwrap();
    let said = check_key_file(&dir.path().join("none.key"))
        .unwrap_err()
        .to_string();
    assert!(
        said.contains("cannot be read") && said.contains("api_key_file"),
        "{said}"
    );
    let said = check_key_file(dir.path()).unwrap_err().to_string();
    assert!(said.contains("not a regular file"), "{said}");
}

#[test]
fn redaction_removes_every_shape_a_key_travels_in() {
    for (input, secret) in [
        ("auth failed for Bearer rpa_ABC123 today", "rpa_ABC123"),
        ("Authorization: Bearer abc.def.ghi", "abc.def.ghi"),
        ("RUNPOD_API_KEY=rpa_ABC123 was rejected", "rpa_ABC123"),
        ("api_key: hunter2 is wrong", "hunter2"),
        ("apikey=hunter2", "hunter2"),
        ("the key rpa_QWERTY999 is bad", "rpa_QWERTY999"),
        ("quoted \"rpa_QWERTY999\" key", "QWERTY999"),
        ("RUNPOD_API_KEY hunter2", "hunter2"),
    ] {
        let out = redact(input);
        assert!(!out.contains(secret), "{input:?} -> {out:?}");
        assert!(out.contains("<redacted>"), "{out:?}");
    }
    assert_eq!(redact("pod abc is running"), "pod abc is running");
}

#[test]
fn the_wrapper_gives_the_key_to_the_child_through_the_environment_only() {
    let dir = tempfile::tempdir().unwrap();
    let path = key_file(dir.path(), 0o600, &format!("{SECRET}\n"));
    let out = Command::new("sh")
        .args(["-c", WRAPPER, "w"])
        .arg(&path)
        .args(["sh", "-c", "printf %s \"$RUNPOD_API_KEY\""])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        SECRET,
        "the newline is trimmed"
    );
}

#[test]
fn the_wrapper_reports_an_unreadable_or_empty_key_by_exit_code() {
    let dir = tempfile::tempdir().unwrap();
    let missing = Command::new("sh")
        .args(["-c", WRAPPER, "w"])
        .arg(dir.path().join("none"))
        .args(["true"])
        .output()
        .unwrap();
    assert_eq!(missing.status.code(), Some(READ_FAILED));
    let empty = key_file(dir.path(), 0o600, "\n");
    let out = Command::new("sh")
        .args(["-c", WRAPPER, "w"])
        .arg(&empty)
        .args(["true"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(EMPTY_KEY));
    assert_eq!(explain(READ_FAILED, ""), "cannot read the API key file");
    assert_eq!(explain(EMPTY_KEY, ""), "the API key file is empty");
    assert!(explain(127, "").contains("not installed"));
}

#[test]
fn the_process_carries_the_key_file_path_and_never_a_key() {
    let config = PoolRunpod {
        api_key_file: Some("/keys/runpod.key".into()),
        ..PoolRunpod::default()
    };
    let cli = CtlCli::new(&config);
    let command = cli.command(&args(&["pod", "list", "--all"]));
    assert_eq!(command.get_program(), "sh");
    let all: Vec<String> = command
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(all[0], "-c");
    assert_eq!(all[1], WRAPPER);
    assert_eq!(all[3], "/keys/runpod.key");
    assert_eq!(
        &all[4..],
        ["runpodctl", "pod", "list", "--all", "--output", "json"]
    );
    assert!(
        command.get_envs().next().is_none(),
        "the key is not placed in the environment by Vivi"
    );
}

#[test]
fn a_call_with_an_open_key_file_never_starts_the_cli() {
    let dir = tempfile::tempdir().unwrap();
    let path = key_file(dir.path(), 0o644, SECRET);
    let config = PoolRunpod {
        api_key_file: Some(path.display().to_string()),
        ..PoolRunpod::default()
    };
    let said = CtlCli::new(&config)
        .run(&args(&["pod", "create"]), None, Duration::from_secs(5))
        .unwrap_err()
        .to_string();
    assert!(said.contains("readable by group or others"), "{said}");
    assert!(!said.contains(SECRET));
}
