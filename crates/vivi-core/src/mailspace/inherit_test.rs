use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use super::*;
use crate::mailspace::Mailspace;

const ID: &str = "12345678-1234-4234-8234-123456789abc";

/// Write `<dir>/.vivi/mailspace.toml` and return the dir.
fn mailspace(dir: &Path, text: &str) -> PathBuf {
    let vivi = dir.join(".vivi");
    fs::create_dir_all(&vivi).unwrap();
    fs::write(vivi.join("mailspace.toml"), text).unwrap();
    dir.to_path_buf()
}

fn project_text(extra: &str) -> String {
    format!("name = \"proj\"\nid = \"{ID}\"\n{extra}")
}

/// A base at `<tmp>/` and a project at `<tmp>/work/proj`.
fn pair(base: &str, project: &str) -> (TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    mailspace(tmp.path(), &format!("base = true\n{base}"));
    let root = mailspace(&tmp.path().join("work/proj"), &project_text(project));
    (tmp, root)
}

fn load(root: &Path) -> LoadedConfig {
    let path = root.join(".vivi/mailspace.toml");
    let raw = fs::read_to_string(&path).unwrap();
    load_with_origins(&path, &raw).unwrap()
}

const BASE_POOLS: &str = "[pools.studio]\nprovider = \"local\"\nslots = 4\nfallback = \"spare\"\n\
                          setup = [\"one\", \"two\"]\n[pools.spare]\nprovider = \"local\"\nslots = 1\n";

#[test]
fn deeper_values_win_and_tables_merge_key_by_key() {
    let (_tmp, root) = pair(
        BASE_POOLS,
        "[pools.studio]\nslots = 9\n[pools.own]\nprovider = \"local\"\nslots = 2\n",
    );
    let config = load(&root).config;
    let studio = &config.pools["studio"];
    assert_eq!(studio.slots, 9, "project value wins");
    assert_eq!(studio.fallback.as_deref(), Some("spare"), "base key kept");
    assert!(config.pools.contains_key("spare"));
    assert!(config.pools.contains_key("own"));
}

#[test]
fn arrays_replace_whole() {
    let (_tmp, root) = pair(BASE_POOLS, "[pools.studio]\nsetup = [\"only\"]\n");
    let config = load(&root).config;
    assert_eq!(config.pools["studio"].setup, ["only"]);
}

#[test]
fn unset_removes_an_inherited_key_and_a_whole_table() {
    let (_tmp, root) = pair(
        BASE_POOLS,
        "unset = [\"pools.studio.fallback\", \"pools.spare\"]\n[pools.studio]\nslots = 9\n",
    );
    let loaded = load(&root);
    let config = &loaded.config;
    assert_eq!(config.pools["studio"].fallback, None);
    assert!(!config.pools.contains_key("spare"));
    assert_eq!(config.pools["studio"].slots, 9);
    assert_eq!(loaded.origins.warnings(), Vec::<String>::new());
}

#[test]
fn unset_that_matches_nothing_is_a_warning() {
    let (_tmp, root) = pair(BASE_POOLS, "unset = [\"pools.nope.slots\"]\n");
    let warnings = load(&root).origins.warnings();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("pools.nope.slots"));
}

#[test]
fn unset_accepts_quoted_segments() {
    assert_eq!(
        merge::split_path("pools.\"a.b\".size"),
        ["pools", "a.b", "size"]
    );
    assert_eq!(merge::split_path("pools..x"), Vec::<String>::new());
    assert_eq!(merge::split_path("pools.\"x"), Vec::<String>::new());
}

#[test]
fn a_malformed_unset_is_an_error() {
    let (_tmp, root) = pair("", "unset = \"pools.x\"\n");
    let path = root.join(".vivi/mailspace.toml");
    let raw = fs::read_to_string(&path).unwrap();
    assert!(load_with_origins(&path, &raw).is_err());
}

#[test]
fn roles_merge_by_name_and_can_be_unset() {
    let (_tmp, root) = pair(
        "[[identities]]\nname = \"hand\"\nkind = \"hand\"\nmodel = \"m1\"\n\
         [[identities]]\nname = \"auditor\"\nkind = \"auditor\"\n",
        "unset = [\"identities.auditor\"]\n[[identities]]\nname = \"hand\"\nmodel = \"m2\"\n\
         [[identities]]\nname = \"mind\"\nkind = \"mind\"\n",
    );
    let config = load(&root).config;
    let names: Vec<&str> = config.identities.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["hand", "mind"]);
    assert_eq!(config.identities[0].model.as_deref(), Some("m2"));
    assert_eq!(config.identities[0].kind.as_deref(), Some("hand"));
}

#[test]
fn identity_and_store_keys_are_never_inherited() {
    let tmp = tempfile::tempdir().unwrap();
    mailspace(
        tmp.path(),
        "base = true\nname = \"basename\"\nid = \"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa\"\n\
         description = \"d\"\narchive = \"/somewhere\"\n[sync]\nserver = \"s\"\n",
    );
    let root = mailspace(&tmp.path().join("proj"), &project_text(""));
    let config = load(&root).config;
    assert_eq!(config.name, "proj");
    assert_eq!(config.id.as_deref(), Some(ID));
    assert!(!config.base);
    assert!(config.description.is_none());
    assert!(config.archive.is_none());
    assert!(config.sync.is_none());
}

#[test]
fn an_ancestor_without_the_base_flag_is_ignored() {
    let tmp = tempfile::tempdir().unwrap();
    mailspace(tmp.path(), &project_text(BASE_POOLS));
    let root = mailspace(&tmp.path().join("proj"), &project_text(""));
    let loaded = load(&root);
    assert!(loaded.config.pools.is_empty());
    assert!(!loaded.origins.inherited());
}

#[test]
fn base_false_is_not_a_base() {
    let tmp = tempfile::tempdir().unwrap();
    mailspace(tmp.path(), &format!("base = false\n{BASE_POOLS}"));
    let root = mailspace(&tmp.path().join("proj"), &project_text(""));
    assert!(load(&root).config.pools.is_empty());
}

#[test]
fn no_base_leaves_the_config_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    let text = project_text(BASE_POOLS);
    let root = mailspace(&tmp.path().join("proj"), &text);
    let loaded = load(&root);
    let plain: MailspaceConfig = toml::from_str(&text).unwrap();
    assert_eq!(
        serde_json::to_value(&loaded.config).unwrap(),
        serde_json::to_value(&plain).unwrap()
    );
    assert!(!loaded.origins.inherited());
    assert_eq!(loaded.origins.render(), "");
}

#[test]
fn chained_bases_apply_farthest_first() {
    let tmp = tempfile::tempdir().unwrap();
    mailspace(
        tmp.path(),
        "base = true\n[pools.studio]\nprovider = \"local\"\nslots = 1\nfallback = \"far\"\n",
    );
    mailspace(
        &tmp.path().join("org"),
        "base = true\n[pools.studio]\nslots = 2\n",
    );
    let root = mailspace(&tmp.path().join("org/proj"), &project_text(""));
    let loaded = load(&root);
    let studio = &loaded.config.pools["studio"];
    assert_eq!(studio.slots, 2, "nearer base wins");
    assert_eq!(studio.fallback.as_deref(), Some("far"));
    assert_eq!(loaded.origins.files.len(), 3);
}

#[test]
fn an_unparseable_ancestor_is_skipped_and_reported() {
    let tmp = tempfile::tempdir().unwrap();
    mailspace(tmp.path(), "this is [ not toml");
    let root = mailspace(&tmp.path().join("proj"), &project_text(""));
    let loaded = load(&root);
    assert_eq!(loaded.origins.skipped.len(), 1);
    assert_eq!(loaded.origins.warnings().len(), 1);
}

#[cfg(unix)]
#[test]
fn an_unreadable_ancestor_ends_the_walk_without_error() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    mailspace(tmp.path(), &format!("base = true\n{BASE_POOLS}"));
    let near = mailspace(
        &tmp.path().join("org"),
        "base = true\n[pools.near]\nprovider = \"local\"\nslots = 1\n",
    );
    let locked = near.join(".vivi/mailspace.toml");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let root = mailspace(&near.join("proj"), &project_text(""));
    let readable = fs::read_to_string(&locked).is_ok();
    let loaded = load(&root);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o600)).unwrap();
    if readable {
        return; // running as a user the mode does not bind
    }
    assert!(loaded.config.pools.is_empty(), "walk ended at the lock");
}

#[test]
fn an_inherited_charter_is_visible_and_the_project_charter_overrides_it() {
    let (tmp, root) = pair("", "");
    let base_charters = tmp.path().join(".vivi/charters");
    fs::create_dir_all(&base_charters).unwrap();
    fs::write(base_charters.join("hand.md"), "base hand").unwrap();
    fs::write(base_charters.join("gate.md"), "base gate").unwrap();
    let own = root.join(".vivi/charters");
    fs::create_dir_all(&own).unwrap();
    fs::write(own.join("hand.md"), "own hand").unwrap();
    let vivi = root.join(".vivi");
    assert_eq!(
        fs::read_to_string(charter_file(&vivi, "hand")).unwrap(),
        "own hand"
    );
    assert_eq!(
        fs::read_to_string(charter_file(&vivi, "gate")).unwrap(),
        "base gate"
    );
    assert_eq!(charter_file(&vivi, "none"), own.join("none.md"));
}

#[test]
fn open_root_reads_the_merged_view_but_writers_touch_only_the_project_file() {
    let (tmp, root) = pair(BASE_POOLS, "[pools.own]\nprovider = \"local\"\nslots = 2\n");
    let base_file = tmp.path().join(".vivi/mailspace.toml");
    let base_before = fs::read_to_string(&base_file).unwrap();
    let mailspace = Mailspace::open_root(root.clone()).unwrap();
    assert!(mailspace.config.pools.contains_key("studio"));
    crate::mailspace::config_edit::apply(&mailspace.config_path(), |doc| {
        crate::mailspace::config_edit::set_pool_key(doc, "own", "slots", "5")
    })
    .unwrap();
    let written = fs::read_to_string(root.join(".vivi/mailspace.toml")).unwrap();
    assert!(
        !written.contains("studio"),
        "merged view leaked:\n{written}"
    );
    assert!(!written.contains("base"), "{written}");
    assert_eq!(fs::read_to_string(&base_file).unwrap(), base_before);
    let reopened = Mailspace::open_root(root).unwrap();
    assert_eq!(reopened.config.pools["own"].slots, 5);
    assert!(reopened.config.pools.contains_key("studio"));
}

#[test]
fn origins_name_the_defining_file_per_pool_and_role() {
    let (tmp, root) = pair(
        BASE_POOLS,
        "[pools.studio]\nslots = 9\n[[identities]]\nname = \"mind\"\nkind = \"mind\"\n",
    );
    let loaded = load(&root);
    let base = tmp.path().join(".vivi/mailspace.toml");
    assert_eq!(
        loaded.origins.files,
        [base, root.join(".vivi/mailspace.toml")]
    );
    let studio = loaded
        .origins
        .entries
        .iter()
        .find(|entry| entry.section == "pools.studio")
        .unwrap();
    let files: Vec<usize> = studio.by_file.iter().map(|(file, _)| *file).collect();
    assert_eq!(files, [0, 1], "base supplies some keys, project slots");
    assert_eq!(studio.by_file[1].1, ["slots"]);
    let spare = loaded
        .origins
        .entries
        .iter()
        .find(|entry| entry.section == "pools.spare")
        .unwrap();
    assert_eq!(spare.by_file.len(), 1);
    assert_eq!(spare.by_file[0].0, 0);
    let mind = loaded
        .origins
        .entries
        .iter()
        .find(|entry| entry.section == "identities.mind")
        .unwrap();
    assert_eq!(mind.by_file[0].0, 1);
    let text = loaded.origins.render();
    assert!(
        text.contains("[base]") && text.contains("[project]"),
        "{text}"
    );
    assert!(text.contains("pools.studio"), "{text}");
}

#[test]
fn an_unset_key_has_no_origin() {
    let (_tmp, root) = pair(BASE_POOLS, "unset = [\"pools.spare\"]\n");
    let loaded = load(&root);
    assert!(
        loaded
            .origins
            .entries
            .iter()
            .all(|entry| entry.section != "pools.spare")
    );
    assert!(loaded.origins.render().contains("unset pools.spare"));
}

#[test]
fn a_base_root_is_detected_and_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let base = mailspace(&tmp.path().join("base"), "base = true\n");
    let project = mailspace(&tmp.path().join("project"), &project_text(""));
    assert!(is_base_root(&base));
    assert!(!is_base_root(&project));
    assert!(!is_base_root(&tmp.path().join("missing")));
    assert!(refuse_base(&base).is_err());
    assert!(refuse_base(&project).is_ok());
}

#[test]
fn the_service_refuses_to_register_or_pump_a_base() {
    let tmp = tempfile::tempdir().unwrap();
    let base = mailspace(&tmp.path().join("base"), "base = true\n");
    let change = crate::pump::service::FlagChange::default();
    let error = crate::pump::service::add(&base, change).unwrap_err();
    assert!(error.to_string().contains("base"), "{error}");
    let error = crate::pump::run::run(&base, true).unwrap_err();
    assert!(error.to_string().contains("base"), "{error}");
    assert!(!base.join(".vivi/mailspace.id").exists(), "no id minted");
}
