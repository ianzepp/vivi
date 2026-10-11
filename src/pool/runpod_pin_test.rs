use std::cell::RefCell;
use std::os::unix::fs::PermissionsExt;

use super::*;

const ED: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIIBo";
const ECDSA: &str = "ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAA";
const RSA: &str = "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABgQC1";

fn scan_of(keys: &[&str]) -> String {
    let mut text = String::from("# 185.1.2.3:22850 SSH-2.0-OpenSSH_9.6p1\n");
    for key in keys {
        text.push_str(&format!("[185.1.2.3]:22850 {key}\n"));
    }
    text
}

/// Answers with the next scan of a script, then the last one again.
struct Scripted(RefCell<Vec<String>>);

impl KeyScanner for Scripted {
    fn scan(&self, _host: &str, _port: u16) -> Result<String, VivariumError> {
        let mut left = self.0.borrow_mut();
        Ok(if left.len() > 1 {
            left.remove(0)
        } else {
            left[0].clone()
        })
    }
}

fn scripted(scans: &[String]) -> Scripted {
    Scripted(RefCell::new(scans.to_vec()))
}

#[test]
fn a_scan_is_a_sorted_distinct_set_of_supported_keys() {
    let text = scan_of(&[RSA, ED, ED, ECDSA]);
    assert_eq!(parse_scan(&text), [ECDSA, ED, RSA].map(str::to_string));
    let odd = "host ssh-dss AAAA\nhost ssh-ed25519 not*base64\nhost ssh-ed25519\n\n";
    assert!(parse_scan(odd).is_empty());
}

#[test]
fn the_order_a_scan_prints_keys_in_does_not_matter() {
    let a = scan_of(&[ED, RSA, ECDSA]);
    let b = scan_of(&[ECDSA, RSA, ED]);
    assert_eq!(pin_from(&a, &b, "h", 1).unwrap().len(), 3);
}

#[test]
fn two_matching_scans_pin_their_keys() {
    let both = scan_of(&[ED, RSA]);
    let keys = pin_from(&both, &both, "h", 22).unwrap();
    assert_eq!(keys, [ED, RSA].map(str::to_string));
}

#[test]
fn scans_that_differ_fail_closed() {
    let first = scan_of(&[ED]);
    let second = scan_of(&[ECDSA]);
    let err = pin_from(&first, &second, "h", 22).unwrap_err().to_string();
    assert!(
        err.contains("changed between two scans") && err.contains("failing closed"),
        "{err}"
    );
    // A key added or dropped between scans is a change too.
    assert!(pin_from(&scan_of(&[ED]), &scan_of(&[ED, RSA]), "h", 22).is_err());
}

#[test]
fn a_scan_with_no_usable_key_pins_nothing() {
    let none = "# nothing here\n";
    let err = pin_from(none, none, "h", 22).unwrap_err().to_string();
    assert!(err.contains("presented no SSH host key"), "{err}");
    assert!(pin_from(&scan_of(&[ED]), none, "h", 22).is_err());
}

#[test]
fn the_known_hosts_file_names_the_exact_address() {
    let text = known_hosts("185.1.2.3", 22850, &[ED.to_string(), RSA.to_string()]);
    assert_eq!(
        text,
        format!("[185.1.2.3]:22850 {ED}\n[185.1.2.3]:22850 {RSA}\n")
    );
}

#[test]
fn capture_writes_a_private_pin_only_after_two_matching_scans() {
    let dir = tempfile::tempdir().unwrap();
    let path = pin_path(dir.path(), "abc123def");
    let same = scan_of(&[ED, RSA]);
    let scanner = scripted(&[same.clone(), same]);
    let keys = capture(&scanner, ("185.1.2.3", 22850), &path, Duration::ZERO).unwrap();
    assert_eq!(keys.len(), 2);
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(text, known_hosts("185.1.2.3", 22850, &keys));
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn a_changing_host_key_writes_no_pin_and_leaves_an_old_one_alone() {
    let dir = tempfile::tempdir().unwrap();
    let path = pin_path(dir.path(), "abc123def");
    let scanner = scripted(&[scan_of(&[ED]), scan_of(&[RSA])]);
    assert!(capture(&scanner, ("185.1.2.3", 22850), &path, Duration::ZERO).is_err());
    assert!(!path.exists(), "nothing is trusted after a mismatch");
}

#[test]
fn a_hostile_address_is_refused_before_any_scan() {
    let scanner = scripted(&[scan_of(&[ED])]);
    let dir = tempfile::tempdir().unwrap();
    let path = pin_path(dir.path(), "x");
    for (host, port) in [
        ("-oProxyCommand=x", 22),
        ("a b", 22),
        ("", 22),
        ("h;rm", 22),
        ("h", 0),
    ] {
        assert!(
            capture(&scanner, (host, port), &path, Duration::ZERO).is_err(),
            "{host}"
        );
    }
}

#[test]
fn forgetting_a_pin_that_is_not_there_is_fine() {
    let dir = tempfile::tempdir().unwrap();
    let path = pin_path(dir.path(), "gone");
    forget(&path);
    std::fs::write(&path, "x").unwrap();
    forget(&path);
    assert!(!path.exists());
}

#[test]
fn a_pinned_ssh_call_checks_strictly_against_the_pin_alone() {
    let cli = super::super::ssh_cli::SshCli::pinned(
        "root@185.1.2.3",
        22850,
        std::path::Path::new("/p/pin"),
    );
    let options = cli.options();
    for needed in [
        "StrictHostKeyChecking=yes",
        "UserKnownHostsFile=/p/pin",
        "GlobalKnownHostsFile=/dev/null",
        "CheckHostIP=no",
        "UpdateHostKeys=no",
        "BatchMode=yes",
    ] {
        assert!(options.iter().any(|o| o == needed), "{needed}: {options:?}");
    }
    let at = options.iter().position(|o| o == "-p").unwrap();
    assert_eq!(options[at + 1], "22850");
    assert!(
        !options
            .iter()
            .any(|o| o.contains("StrictHostKeyChecking=no") || o.contains("accept-new"))
    );
}
