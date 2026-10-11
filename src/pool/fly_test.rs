use std::collections::BTreeMap;

use super::*;

const LIST: &str = r#"[
  {"id": "aaa111", "name": "faber-slot-1", "state": "stopped", "region": "iad",
   "created_at": "2026-10-11T01:19:34Z",
   "config": {"env": {"GH_TOKEN": "ghs_secret_token_value"},
              "metadata": {"fly_platform_version": "v2", "vivi_pool": "faber",
                           "vivi_pool_slot": "1", "vivi_pool_base": "0123456789abcdef0123456789abcdef01234567"}},
   "events": [{"type": "exit", "request": {"exit_event": {"exit_code": 0}}},
              {"type": "start", "request": {}}]},
  {"id": "bbb222", "name": "slot-quad-3", "state": "stopped", "region": "iad",
   "created_at": "2026-10-11T01:00:00Z",
   "config": {"env": {}},
   "events": [{"type": "exit", "request": {"exit_event": {"exit_code": 143}}}]},
  {"id": "ccc333", "name": "faber-slot-2", "state": "started", "region": "iad",
   "created_at": "2026-10-11T02:00:00Z", "config": {"metadata": {"vivi_pool": "faber", "vivi_pool_slot": "2"}},
   "events": []}
]"#;

#[test]
fn parses_machine_list_fixture() {
    let machines = parse_machine_list(LIST).unwrap();
    assert_eq!(machines.len(), 3);
    assert_eq!(machines[0].id, "aaa111");
    assert_eq!(machines[0].state, "stopped");
    assert_eq!(machines[0].exit_code, Some(0));
    assert_eq!(machines[0].metadata["vivi_pool"], "faber");
    assert_eq!(machines[1].exit_code, Some(143));
    assert!(machines[1].metadata.is_empty());
    assert_eq!(machines[2].exit_code, None);
}

#[test]
fn fixture_becomes_slot_status() {
    let machines = parse_machine_list(LIST).unwrap();
    let slots = crate::pool::slot::pool_slots("faber", &machines);
    assert_eq!(slots.len(), 2);
    assert_eq!(slots[0].name, "faber-slot-1");
    assert_eq!(slots[0].state, "unseeded", "no seed mark in the fixture");
    assert_eq!(
        slots[0].base.as_deref(),
        Some("0123456789abcdef0123456789abcdef01234567")
    );
    assert_eq!(slots[1].state, "seeding");
    assert_eq!(slots[1].base, None);
}

#[test]
fn list_parse_does_not_keep_the_environment() {
    let machines = parse_machine_list(LIST).unwrap();
    assert!(!format!("{machines:?}").contains("ghs_secret_token_value"));
}

#[test]
fn rejects_non_array_output() {
    assert!(parse_machine_list("{}").is_err());
    assert!(parse_machine_list("not json").is_err());
}

#[test]
fn finds_machine_id_in_run_output() {
    let out = "Searching for image 'rust:1.97'...\nMachine ID: 8655521c76d4d8\nInstance ID: 01M4\n";
    assert_eq!(parse_run_output(out).as_deref(), Some("8655521c76d4d8"));
    assert_eq!(parse_run_output("nothing here"), None);
}

#[test]
fn run_args_carry_the_slot_contract() {
    let spec = CreateSpec {
        name: "faber-slot-1".into(),
        region: "iad".into(),
        image: "rust:1.97-bookworm".into(),
        vm_size: "performance-8x".into(),
        memory_mb: 32768,
        metadata: BTreeMap::from([("vivi_pool".into(), "faber".into())]),
        env: BTreeMap::from([("REPO".into(), "o/r".into())]),
        files: vec![("/work/boot.sh".into(), "echo hi\n".into())],
        command: vec!["bash".into(), "/work/boot.sh".into()],
    };
    let args = run_args("lane-pool", &spec);
    let joined = args.join(" ");
    assert!(joined.starts_with("machine run rust:1.97-bookworm --detach"));
    for needle in [
        "--app lane-pool",
        "--restart no",
        "--rootfs-persist always",
        "--name faber-slot-1",
        "--vm-memory 32768",
        "--metadata vivi_pool=faber",
        "--env REPO=o/r",
        "--file-literal /work/boot.sh=echo hi\n",
    ] {
        assert!(joined.contains(needle), "missing {needle}: {joined}");
    }
    assert!(joined.ends_with("-- bash /work/boot.sh"));
}

#[test]
fn scrub_hides_secret_values() {
    assert_eq!(
        scrub("bad token ghs_abcdefgh here", &["ghs_abcdefgh"]),
        "bad token <redacted> here"
    );
    assert_eq!(scrub("short x", &["x"]), "short x");
}

#[test]
fn app_and_secret_names_parse_and_carry_no_values() {
    let apps = parse_app_names(r#"[{"Name": "one", "Status": "x"}, {"Name": "two"}]"#).unwrap();
    assert_eq!(apps, ["one", "two"]);
    let secrets = parse_secret_names(
        r#"[{"name": "MODEL_API_KEY", "digest": "271141684a869909", "status": "Staged"}]"#,
    )
    .unwrap();
    assert_eq!(secrets, ["MODEL_API_KEY"]);
    assert!(parse_secret_names("not json").is_err());
    assert!(parse_app_names("{}").is_err());
}
