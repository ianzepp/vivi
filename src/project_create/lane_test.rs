//! Decision tests for the lanes-half ACL copy. The Linux runtime copy is
//! `linux_copies_group_and_acl_from_the_lexicographic_sibling` and does not
//! run on macOS.

use std::fs;
use std::path::{Path, PathBuf};

use tempfile::tempdir;

use super::acl::{
    AclCopy, AclIntent, HostKind, NOT_LINUX, TOOLS_MISSING, choose_sibling, clean_acl,
    copy_failed_warning, host_kind, list_siblings, listing_failed_warning, no_sibling_warning,
    plan, settle,
};
use super::{LaneCreate, LanePlan, create_lane, run_lane_steps_with};
use crate::project_create::Undo;

fn linux_ready() -> HostKind {
    HostKind::Linux { tools: true }
}

#[test]
fn the_sibling_is_the_lexicographic_first_with_a_readable_acl() {
    let lanes = Path::new("/lanes");
    let earliest = PathBuf::from("/lanes/a.git");
    let middle = PathBuf::from("/lanes/m.git");
    let last = PathBuf::from("/lanes/z.git");
    let intent = plan(
        linux_ready(),
        lanes,
        Ok(vec![last, earliest.clone(), middle.clone()]),
        &|path| path.file_name() != earliest.file_name(),
    );
    assert_eq!(intent, AclIntent::Copy { sibling: middle });
}

#[test]
fn choose_sibling_trusts_the_order_it_is_given() {
    let candidates = vec![PathBuf::from("/lanes/z.git"), PathBuf::from("/lanes/m.git")];
    let chosen = choose_sibling(&candidates, &|_| true);
    assert_eq!(chosen, Some(candidates[0].as_path()));
}

#[test]
fn off_linux_skips_without_probing_a_sibling() {
    let intent = plan(
        HostKind::NotLinux,
        Path::new("/lanes"),
        Err("listing must not matter".into()),
        &|_| panic!("off Linux must not probe a sibling"),
    );
    assert_eq!(
        intent,
        AclIntent::Skip {
            warning: NOT_LINUX.to_string(),
        }
    );
}

#[test]
fn missing_tools_skip_without_probing_a_sibling() {
    let intent = plan(
        HostKind::Linux { tools: false },
        Path::new("/lanes"),
        Err("listing must not matter".into()),
        &|_| panic!("missing tools must not probe a sibling"),
    );
    assert_eq!(
        intent,
        AclIntent::Skip {
            warning: TOOLS_MISSING.to_string(),
        }
    );
}

#[test]
fn no_readable_sibling_names_the_lanes_root() {
    let lanes = Path::new("/lanes");
    let intent = plan(
        linux_ready(),
        lanes,
        Ok(vec![PathBuf::from("/lanes/a.git")]),
        &|_| false,
    );
    assert_eq!(
        intent,
        AclIntent::Skip {
            warning: no_sibling_warning(lanes),
        }
    );
}

#[test]
fn an_empty_lanes_root_has_no_sibling() {
    let lanes = Path::new("/lanes");
    let intent = plan(linux_ready(), lanes, Ok(Vec::new()), &|_| true);
    assert_eq!(
        intent,
        AclIntent::Skip {
            warning: no_sibling_warning(lanes),
        }
    );
}

#[test]
fn a_listing_failure_skips_with_one_line_of_detail() {
    let intent = plan(
        linux_ready(),
        Path::new("/lanes"),
        Err("permission denied\n(os error 13)".into()),
        &|_| panic!("a failed listing must not probe a sibling"),
    );
    let warning = listing_failed_warning("permission denied\n(os error 13)");
    assert_eq!(
        intent,
        AclIntent::Skip {
            warning: warning.clone()
        }
    );
    assert!(!warning.contains('\n'));
    assert!(
        warning.contains("permission denied (os error 13)"),
        "{warning}"
    );
}

#[test]
fn a_failed_copy_is_a_one_line_warning_not_an_error() {
    let sibling = Path::new("/lanes/old.git");
    let warning = copy_failed_warning(sibling, "setfacl: Operation not permitted\n");
    assert_eq!(
        settle(sibling, Err("setfacl: Operation not permitted\n".into())),
        AclCopy::Skipped {
            warning: warning.clone(),
        }
    );
    assert!(!warning.contains('\n'), "{warning}");
    assert!(warning.contains("the lane was still created"), "{warning}");
    assert!(warning.contains("Operation not permitted"), "{warning}");
    assert_eq!(
        settle(sibling, Ok(())),
        AclCopy::Copied {
            sibling: sibling.to_path_buf(),
        }
    );
    assert!(copy_failed_warning(sibling, " \n").contains("no detail"));
}

#[test]
fn acl_text_drops_comments_and_blank_lines() {
    let raw = "\n# file: /lanes/a.git\nuser::rwx\nuser:1000:rwx\t#effective:r-x\n\ngroup::r-x\nother::---\n";
    assert_eq!(
        clean_acl(raw),
        "user::rwx\nuser:1000:rwx\ngroup::r-x\nother::---\n"
    );
}

#[test]
fn only_other_bare_repo_directories_are_siblings() {
    let tmp = tempdir().unwrap();
    let lanes = tmp.path();
    fs::create_dir(lanes.join("a.git")).unwrap();
    fs::create_dir(lanes.join("m.git")).unwrap();
    fs::create_dir(lanes.join("new.git")).unwrap();
    fs::create_dir(lanes.join("notes")).unwrap();
    fs::write(lanes.join("0.git"), b"not a directory").unwrap();
    let mut found = list_siblings(lanes, &lanes.join("new.git")).unwrap();
    found.sort();
    assert_eq!(found, vec![lanes.join("a.git"), lanes.join("m.git")]);
}

#[test]
fn a_missing_lanes_root_cannot_be_listed() {
    let error = list_siblings(Path::new("/no/such/vivi-lanes"), Path::new("app.git")).unwrap_err();
    assert!(!error.is_empty(), "{error}");
}

#[test]
fn this_host_reports_whether_it_can_copy_an_acl() {
    if cfg!(target_os = "linux") {
        assert!(matches!(host_kind(), HostKind::Linux { .. }));
    } else {
        assert_eq!(host_kind(), HostKind::NotLinux);
    }
}

#[test]
fn an_acl_copy_failure_does_not_fail_lane_creation() {
    let tmp = tempdir().unwrap();
    let lanes = tmp.path().join("lanes");
    fs::create_dir(&lanes).unwrap();
    let identity = tmp.path().join("identity");
    fs::create_dir(&identity).unwrap();
    let repo = lanes.join("app.git");
    let plan = LanePlan {
        repo: repo.clone(),
        branch: "main".into(),
        initial_commit: true,
        identity_root: identity,
    };
    let mut undo = Undo::default();
    let warning = copy_failed_warning(
        Path::new("/lanes/old.git"),
        "setfacl: Operation not permitted",
    );
    let copy = move |path: &Path| {
        assert!(
            path.join("HEAD").is_file(),
            "the ACL copy runs after git init"
        );
        AclCopy::Skipped {
            warning: warning.clone(),
        }
    };
    let created = run_lane_steps_with(&plan, &|_| Ok(()), &mut undo, &copy).unwrap();
    assert_eq!(created.repo_path, repo);
    assert!(
        created.initial_commit.is_some(),
        "the seed commit still runs after a failed ACL copy"
    );
}

#[cfg(not(target_os = "linux"))]
#[test]
fn off_linux_creation_skips_acl_copy_and_still_creates_the_repo() {
    let tmp = tempdir().unwrap();
    let lanes = tmp.path().join("lanes");
    fs::create_dir(&lanes).unwrap();
    fs::create_dir(lanes.join("m.git")).unwrap();
    let identity = tmp.path().join("identity");
    fs::create_dir(&identity).unwrap();
    let created = create_lane(&LaneCreate::new("app", &lanes, &identity)).unwrap();
    assert!(created.repo_path.join("HEAD").is_file());
    assert_eq!(
        super::acl::apply(&created.repo_path),
        AclCopy::Skipped {
            warning: NOT_LINUX.to_string(),
        }
    );
}

#[cfg(target_os = "linux")]
#[test]
fn linux_copies_group_and_acl_from_the_lexicographic_sibling() {
    if !linux_acl_tools() {
        eprintln!("skipping linux ACL runtime test: getfacl, setfacl, or chgrp is missing");
        return;
    }
    let tmp = tempdir().unwrap();
    let lanes = tmp.path().join("lanes");
    fs::create_dir(&lanes).unwrap();
    let first = lanes.join("a.git");
    let second = lanes.join("m.git");
    fs::create_dir(&first).unwrap();
    fs::create_dir(&second).unwrap();
    fs::write(lanes.join("0.git"), b"not a directory").unwrap();
    let uid = linux_stdout("id", &["-u"]);
    if !linux_setfacl(&first, &format!("u:{uid}:rwx"))
        || !linux_setfacl(&second, &format!("u:{uid}:r-x"))
    {
        eprintln!("skipping linux ACL runtime test: this filesystem rejected setfacl");
        return;
    }
    if linux_chgrp_other(&first) {
        assert_ne!(linux_gid(&first), linux_gid(&second));
    }
    let identity = tmp.path().join("identity");
    fs::create_dir(&identity).unwrap();
    let created = create_lane(&LaneCreate::new("new", &lanes, &identity)).unwrap();
    assert_eq!(linux_acl_body(&created.repo_path), linux_acl_body(&first));
    assert_ne!(linux_acl_body(&first), linux_acl_body(&second));
    assert_eq!(linux_gid(&created.repo_path), linux_gid(&first));
}

#[cfg(target_os = "linux")]
fn linux_acl_tools() -> bool {
    ["getfacl", "setfacl", "chgrp"]
        .into_iter()
        .all(linux_program_runs)
}

#[cfg(target_os = "linux")]
fn linux_program_runs(program: &str) -> bool {
    std::process::Command::new(program)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
}

#[cfg(target_os = "linux")]
fn linux_stdout(program: &str, args: &[&str]) -> String {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "{program}");
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

#[cfg(target_os = "linux")]
fn linux_setfacl(path: &Path, entry: &str) -> bool {
    std::process::Command::new("setfacl")
        .args(["-m", entry])
        .arg(path)
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(target_os = "linux")]
fn linux_chgrp_other(path: &Path) -> bool {
    let groups = linux_stdout("id", &["-G"]);
    let primary = linux_stdout("id", &["-g"]);
    let Some(other) = groups.split_whitespace().find(|gid| *gid != primary.trim()) else {
        return false;
    };
    std::process::Command::new("chgrp")
        .arg(other)
        .arg(path)
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(target_os = "linux")]
fn linux_gid(path: &Path) -> u32 {
    use std::os::unix::fs::MetadataExt;
    fs::metadata(path).unwrap().gid()
}

#[cfg(target_os = "linux")]
fn linux_acl_body(path: &Path) -> String {
    let output = std::process::Command::new("getfacl")
        .args(["--omit-header", "--numeric", "--no-effective"])
        .arg(path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut lines: Vec<_> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| line.split('#').next().unwrap_or(line).trim().to_string())
        .filter(|line| !line.is_empty())
        .collect();
    lines.sort();
    lines.join("\n")
}
