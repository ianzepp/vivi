//! The PATH of a slot's boot: a login shell may reorder PATH, so both slot
//! drivers put the invoking session's order back in front of it.

use std::process::Command;

use super::{SLOT_SCRIPT, WORKER_MACOS_SCRIPT};

/// The PATH-restoring prelude of the boot command, exactly as the script
/// spells it: from the assignment to the `unset` that ends it.
fn prelude(script: &str) -> &str {
    let start = script
        .find(r#"PATH="$VIVI_SESSION_PATH:$PATH";"#)
        .expect("the boot command restores the session PATH");
    let end = script[start..]
        .find("unset VIVI_SESSION_PATH d q;")
        .expect("the prelude ends by unsetting its names")
        + "unset VIVI_SESSION_PATH d q;".len();
    &script[start..start + end]
}

/// Runs `prelude` under stock `/bin/bash` (3.2 on macOS) as the `-c` text of a
/// login shell would see it: `login` is the PATH after the login profile
/// reordered it, `session` the one the invoking session exported.
fn restored(script: &str, session: &str, login: &str) -> String {
    let text = format!(
        "{}\necho \"$PATH\"; echo \"${{VIVI_SESSION_PATH-unset}}${{d-}}${{q-}}\"",
        prelude(script)
    );
    let out = Command::new("/bin/bash")
        .arg("-c")
        .arg(text)
        .env("PATH", login)
        .env("VIVI_SESSION_PATH", session)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    let mut lines = text.lines();
    let path = lines.next().unwrap().to_string();
    assert_eq!(
        lines.next(),
        Some("unset"),
        "the prelude leaves no names behind"
    );
    path
}

const SCRIPTS: [(&str, &str); 2] = [
    ("ssh-slot.sh", SLOT_SCRIPT),
    ("worker-macos.sh", WORKER_MACOS_SCRIPT),
];

#[test]
fn the_session_order_comes_back_in_front_of_a_reordered_login_path() {
    for (name, script) in SCRIPTS {
        let got = restored(
            script,
            "/s/one:/s/two:/usr/local/bin:/s/three",
            "/usr/local/bin:/usr/bin:/bin:/s/three:/s/two:/s/one:/extra",
        );
        assert_eq!(
            got, "/s/one:/s/two:/usr/local/bin:/s/three:/usr/bin:/bin:/extra",
            "{name}"
        );
    }
}

#[test]
fn repeated_and_empty_entries_are_dropped_keeping_the_first_of_each() {
    for (name, script) in SCRIPTS {
        let got = restored(script, "/a:/b:/a::/c:", "/c:/b::/d:/d");
        assert_eq!(got, "/a:/b:/c:/d", "{name}");
    }
}

#[test]
fn entries_with_spaces_or_globs_pass_through_untouched() {
    for (name, script) in SCRIPTS {
        let got = restored(script, "/with space/bin:/g*/bin", "/usr/bin");
        assert_eq!(got, "/with space/bin:/g*/bin:/usr/bin", "{name}");
    }
}

#[test]
fn both_boot_commands_export_the_session_path_for_the_prelude() {
    for (name, script) in SCRIPTS {
        assert!(
            script.contains(r#"VIVI_SESSION_PATH="$PATH""#),
            "{name} exports VIVI_SESSION_PATH before the login shell starts"
        );
        assert!(
            script.contains("bash -l -c 'PATH=\"$VIVI_SESSION_PATH:$PATH\";"),
            "{name} still runs a login shell"
        );
    }
}
