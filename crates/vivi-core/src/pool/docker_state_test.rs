use super::*;

const SLOT: &str = r#"[{
  "Id": "0bcd9c3683a2c478ecc029835355eb6901286563732b371d1337315b610f5293",
  "Name": "/lane-slot-1",
  "Created": "2026-10-11T13:15:41.365281167Z",
  "State": {"Status": "exited", "ExitCode": 0, "Running": false},
  "Config": {
    "Image": "ubuntu:24.04",
    "Env": ["PATH=/usr/bin", "SECRET_KEY=hunter2-never-shown"],
    "Labels": {"vivi_pool": "lane", "vivi_pool_slot": "1",
               "vivi_pool_base": "abc", "vivi_pool_state": "seeding",
               "vivi_pool_role": "slot", "other": "x"}
  },
  "ImageManifestDescriptor": {"platform": {"architecture": "amd64", "os": "linux"}}
}]"#;

const CLAIM: &str = r#"[{
  "Id": "ffee00112233445566778899aabbccddeeff00112233445566778899aabbccdd",
  "Name": "/vivi-claim-0bcd9c3683a2",
  "Created": "2026-10-11T13:20:00Z",
  "State": {"Status": "created", "ExitCode": 0},
  "Config": {"Image": "ubuntu:24.04",
    "Labels": {"vivi_pool": "lane", "vivi_pool_role": "claim",
               "vivi_pool_claim_of": "0bcd9c3683a2",
               "vivi_pool_task": "task1234", "vivi_pool_claimed": "1700000000"}}
}]"#;

fn one(json: &str) -> Container {
    parse_inspect(json).unwrap().remove(0)
}

fn marks(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

#[test]
fn ids_are_twelve_digits_and_names_lose_the_slash() {
    let slot = one(SLOT);
    assert_eq!(slot.id, "0bcd9c3683a2");
    assert_eq!(slot.name, "lane-slot-1");
    assert_eq!(slot.role(), "slot");
    assert_eq!(slot.arch.as_deref(), Some("amd64"));
    assert_eq!(claim_name(&slot.id), "vivi-claim-0bcd9c3683a2");
    assert_eq!(short("abc"), "abc");
}

#[test]
fn the_environment_is_never_read() {
    assert!(!format!("{:?}", one(SLOT)).contains("hunter2"));
}

#[test]
fn docker_status_words_become_the_pools_state_words() {
    for (status, word) in [
        ("running", "started"),
        ("paused", "started"),
        ("restarting", "starting"),
        ("removing", "stopping"),
        ("dead", "failed"),
        ("exited", "stopped"),
        ("created", "stopped"),
    ] {
        assert_eq!(state_word(status), word, "{status}");
    }
}

#[test]
fn only_a_container_that_ran_and_ended_has_an_exit_code() {
    let mut c = one(SLOT);
    c.exit_code = 7;
    assert_eq!(exit_code(&c), Some(7));
    for status in ["running", "created"] {
        c.status = status.into();
        assert_eq!(exit_code(&c), None, "{status}");
    }
}

#[test]
fn a_seeded_slot_reads_free_from_its_label_marks_and_no_claim() {
    let slot = one(SLOT);
    let seeded = marks(&[("vivi_pool_state", "ready"), ("vivi_pool_arch", "arm64")]);
    let rec = record(&slot, &seeded, None, "local");
    assert_eq!(
        (rec.id.as_str(), rec.name.as_str()),
        ("0bcd9c3683a2", "lane-slot-1")
    );
    assert_eq!(rec.state, "stopped");
    assert_eq!(rec.region, "local");
    assert_eq!(rec.metadata["vivi_pool"], "lane");
    assert_eq!(
        rec.metadata["vivi_pool_state"], "ready",
        "a mark outranks the label"
    );
    assert_eq!(rec.metadata["vivi_pool_arch"], "arm64");
    assert!(!rec.metadata.contains_key("vivi_pool_role"));
    assert!(!rec.metadata.contains_key("other"));
    assert_eq!(crate::pool::slot::slot_state(&rec), "free");
}

#[test]
fn the_label_alone_says_seeding_until_the_mark_is_written() {
    let rec = record(&one(SLOT), &BTreeMap::new(), None, "local");
    assert_eq!(rec.metadata["vivi_pool_state"], "seeding");
    assert_eq!(crate::pool::slot::slot_state(&rec), "unseeded");
}

#[test]
fn a_claim_container_supplies_the_task_and_the_claim_time() {
    let slot = one(SLOT);
    let claim = one(CLAIM);
    let seeded = marks(&[("vivi_pool_state", "ready")]);
    let rec = record(&slot, &seeded, Some(&claim), "local");
    assert_eq!(rec.metadata["vivi_pool_task"], "task1234");
    assert_eq!(rec.metadata["vivi_pool_claimed"], "1700000000");
    assert_eq!(crate::pool::slot::slot_state(&rec), "finished-uncollected");
}

#[test]
fn a_claim_is_found_by_the_slot_it_names() {
    let mut all = parse_inspect(SLOT).unwrap();
    all.extend(parse_inspect(CLAIM).unwrap());
    assert_eq!(
        claim_for(&all, "0bcd9c3683a2").map(|c| c.name.as_str()),
        Some("vivi-claim-0bcd9c3683a2")
    );
    assert!(claim_for(&all, "other").is_none());
}

#[test]
fn bad_output_is_a_parse_error() {
    assert!(parse_inspect("not json").is_err());
    assert!(parse_inspect("{}").is_err());
    assert!(parse_inspect("[]").unwrap().is_empty());
}
