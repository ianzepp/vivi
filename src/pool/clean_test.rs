use super::*;
use crate::mailspace::Mailspace;
use crate::pool::clean_rule::{write_baseline, write_mark};
use crate::pump::state::{Status, Unit};
use std::fs;

#[test]
fn after_merged_is_a_noop_without_a_mailspace() {
    let tmp = tempfile::tempdir().unwrap();
    let unit = Unit {
        status: Status::Merged,
        task: Some("abcd1234".into()),
        pool: Some("lanes".into()),
        ..Unit::default()
    };
    let report = after_merged(tmp.path(), &unit).unwrap();
    assert!(report.actions.is_empty());
    assert!(report.skips.is_empty());
}

fn routing_space(toml: &str) -> (tempfile::TempDir, Mailspace) {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join(".vivi")).unwrap();
    fs::write(dir.path().join(".vivi/mailspace.toml"), toml).unwrap();
    let space = Mailspace::discover(Some(dir.path())).unwrap();
    (dir, space)
}

#[test]
fn recycle_selectors_include_the_merge_role_pool() {
    let (_dir, space) = routing_space(
        r#"
name = "t"
[[identities]]
name = "hand"
kind = "hand"
pool = "glm"
[[identities]]
name = "merge"
kind = "merge"
pool = "host"
[pools.glm]
provider = "local"
slots = 2
[pools.host]
provider = "local"
slots = 4
"#,
    );
    let unit = Unit {
        status: Status::Merged,
        pool: Some("glm".into()),
        ..Unit::default()
    };
    let names: Vec<String> = recycle_selectors(&space, &unit)
        .into_iter()
        .map(|name| name.expect("named pool"))
        .collect();
    assert_eq!(names, ["glm", "host"]);
}

#[test]
fn recycle_selectors_open_every_pool_when_the_unit_has_none() {
    let (_dir, space) = routing_space("name = \"t\"\n");
    let unit = Unit {
        status: Status::Merged,
        ..Unit::default()
    };
    assert_eq!(recycle_selectors(&space, &unit), [None]);
}

#[test]
fn after_slot_freed_reclaims_an_over_cap_target() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("slot");
    fs::create_dir_all(dir.join("marks")).unwrap();
    fs::write(dir.join("marks/vivi_pool"), "lanes\n").unwrap();
    write_mark(&dir, "vivi_pool_state", "ready").unwrap();
    write_baseline(&dir, 10).unwrap();
    fs::create_dir_all(dir.join("repo/src")).unwrap();
    fs::write(dir.join("repo/src/lib.rs"), "ok\n").unwrap();
    fs::create_dir_all(dir.join("repo/target")).unwrap();
    fs::write(dir.join("repo/target/blob"), vec![0_u8; 50]).unwrap();
    let report = after_slot_freed(&dir, Some("target")).unwrap();
    assert_eq!(report.actions.len(), 1);
    assert_eq!(report.actions[0].kind, ActionKind::Build);
    assert!(!dir.join("repo/target/blob").exists());
    assert_eq!(
        fs::read_to_string(dir.join("repo/src/lib.rs")).unwrap(),
        "ok\n"
    );
}

#[test]
fn a_claimed_slot_is_skipped_after_free_hook() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("slot");
    fs::create_dir_all(dir.join("marks")).unwrap();
    fs::write(dir.join("marks/vivi_pool"), "lanes\n").unwrap();
    write_mark(&dir, "vivi_pool_state", "ready").unwrap();
    fs::write(dir.join("claim"), "task=abcd\n").unwrap();
    fs::create_dir_all(dir.join("repo/target")).unwrap();
    fs::write(dir.join("repo/target/blob"), vec![0_u8; 50]).unwrap();
    write_baseline(&dir, 10).unwrap();
    let report = after_slot_freed(&dir, Some("target")).unwrap();
    assert!(report.actions.is_empty());
    assert!(dir.join("repo/target/blob").is_file());
}

#[test]
fn slot_dirs_resolve_through_symlinked_roots() {
    let tmp = tempfile::tempdir().unwrap();
    let real = tmp.path().join("real");
    let slot = real.join("proj/lanes/slot-9");
    fs::create_dir_all(slot.join("repo")).unwrap();
    let link = tmp.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let got = slot_dir_under(&link, &slot.join("repo"));
    assert_eq!(
        got,
        Some(real.canonicalize().unwrap().join("proj/lanes/slot-9"))
    );
}

/// Two mailspaces share one local root: cleaning our mailspace's pool must
/// reclaim only our `<mailspace id>/here` slots and never touch
/// `faberlang/glm` slots. A slot lives under the mailspace **id** directory,
/// not the name slug, so the fixture pins the id in the config and plants our
/// over-cap slot under it.
#[test]
fn clean_mailspace_leaves_a_foreign_mailspaces_slots_alone() {
    const ID: &str = "11111111-1111-1111-1111-111111111111";
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("pool-root");
    let toml = format!(
        "\nname = \"alpha\"\nid = \"{ID}\"\n[pools.here]\nprovider = \"local\"\nslots = 1\nbuild_dir = \"target\"\n[pools.here.local]\nroot = \"{}\"\n",
        root.display()
    );
    let (_dir, space) = routing_space(&toml);
    let ours_rel = format!("{ID}/here/slot-1");
    for (rel, pool) in [
        (ours_rel.as_str(), "here"),
        ("faberlang/glm/slot-2", "glm"),
    ] {
        let slot = root.join(rel);
        fs::create_dir_all(slot.join("marks")).unwrap();
        fs::write(slot.join("marks/vivi_pool"), format!("{pool}\n")).unwrap();
        write_mark(&slot, "vivi_pool_state", "ready").unwrap();
        write_baseline(&slot, 10).unwrap();
        fs::create_dir_all(slot.join("repo/src")).unwrap();
        fs::write(slot.join("repo/src/lib.rs"), "ok\n").unwrap();
        fs::create_dir_all(slot.join("repo/target")).unwrap();
        fs::write(slot.join("repo/target/blob"), vec![0_u8; 80]).unwrap();
    }

    clean_mailspace(&space, &CleanOptions::default()).unwrap();

    let ours = root.join(&ours_rel);
    let foreign = root.join("faberlang/glm/slot-2");
    assert!(
        !ours.join("repo/target/blob").exists(),
        "our own over-cap slot should be reclaimed"
    );
    assert!(
        foreign.join("repo/target/blob").exists(),
        "a foreign mailspace's slot build dir must be left alone"
    );
    assert_eq!(
        fs::read_to_string(foreign.join("marks/build_baseline"))
            .unwrap()
            .trim(),
        "10",
        "a foreign mailspace's slot marks must be left alone"
    );
}
