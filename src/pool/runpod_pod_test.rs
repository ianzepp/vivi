use super::*;

fn config() -> PoolRunpod {
    PoolRunpod {
        api_key_file: Some("/k".into()),
        gpu_types: vec!["NVIDIA A100 80GB PCIe".into()],
        ..PoolRunpod::default()
    }
}

fn s(list: &[&str]) -> Vec<String> {
    list.iter().map(|a| (*a).to_string()).collect()
}

const LISTED: &str = r#"[
  {"id":"abc123def456","name":"vivi-rp1-x","desiredStatus":"RUNNING","runtimeStatus":"running","costPerHr":1.19,"gpuCount":1,"imageName":"img"},
  {"id":"bbb222","name":"other","desiredStatus":"EXITED","runtimeStatus":"stopped","gpuCount":1},
  {"id":"ccc333","name":"n","desiredStatus":"RUNNING","runtimeStatus":"initializing"},
  {"id":"ddd444","name":"n","desiredStatus":"TERMINATED","runtimeStatus":"terminated"},
  {"id":"eee555","name":"n","desiredStatus":"RUNNING","runtimeStatus":"unknown"},
  {"id":"fff666","name":"n","desiredStatus":"EXITED","runtimeStatus":""},
  {"id":"ggg777","name":"n","desiredStatus":"PAUSED","runtimeStatus":"weird"}
]"#;

#[test]
fn a_pod_listing_maps_to_the_engines_state_words() {
    let pods = parse_pods(LISTED).unwrap();
    let states: Vec<&str> = pods.iter().map(Pod::state).collect();
    assert_eq!(
        states,
        [
            "started",
            "stopped",
            "starting",
            "destroyed",
            "starting",
            "stopped",
            "unknown"
        ]
    );
    assert_eq!(pods[0].cost_per_hr, Some(1.19));
}

#[test]
fn a_full_pod_carries_its_address_gpu_and_creation_time() {
    let json = r#"{"id":"abc123def456","name":"n","desiredStatus":"RUNNING","runtimeStatus":"running","costPerHr":1.59,"gpuCount":2,
        "createdAt":"2026-08-27 13:29:30.172 +0000 UTC","machine":{"gpuId":"NVIDIA A100-SXM4-80GB","secureCloud":true},"ssh":{"ip":"203.0.113.9","port":22013}}"#;
    let pod = parse_pod(json).unwrap();
    assert_eq!(pod.endpoint(), Some(("203.0.113.9", 22013)));
    assert_eq!(pod.hardware(), "NVIDIA A100-SXM4-80GB x2, $1.59/h");
    assert_eq!(pod.created_ms(), Some(1_787_837_370_172));
}

#[test]
fn missing_pieces_read_as_unknown_not_as_something_made_up() {
    let pod = parse_pod(r#"{"id":"abc123def456"}"#).unwrap();
    assert_eq!(pod.endpoint(), None);
    assert_eq!(pod.created_ms(), None);
    assert_eq!(pod.hardware(), "unknown GPU x0");
    let half = parse_pod(r#"{"id":"abc123def456","ssh":{"ip":"1.2.3.4","port":0}}"#).unwrap();
    assert_eq!(half.endpoint(), None, "a port of 0 is not an address");
    let none = parse_pod(r#"{"id":"abc123def456","ssh":{"ip":"","port":22}}"#).unwrap();
    assert_eq!(none.endpoint(), None);
    assert_eq!(
        parse_pod(r#"{"id":"abc123def456","createdAt":"yesterday"}"#)
            .unwrap()
            .created_ms(),
        None
    );
}

#[test]
fn an_error_document_is_reported_with_the_providers_words() {
    let said = parse_pods(r#"{"error":"unauthorized","code":"401","status":401}"#)
        .unwrap_err()
        .to_string();
    assert!(
        said.contains("pod list") && said.contains("unauthorized"),
        "{said}"
    );
    assert!(
        parse_pod("{\"error\":\"nope\"}").is_err()
            || parse_pod("{\"error\":\"nope\"}").unwrap().id.is_empty() == false
    );
    assert!(parse_pods("not json").is_err());
    let long = "x".repeat(500);
    assert!(parse_pods(&long).unwrap_err().to_string().len() < 300);
}

#[test]
fn the_account_needs_an_id() {
    let user =
        parse_user(r#"{"id":"user_1","clientBalance":12.5,"currentSpendPerHr":0.0}"#).unwrap();
    assert_eq!(
        (user.id.as_str(), user.client_balance),
        ("user_1", Some(12.5))
    );
    assert!(parse_user(r#"{"error":"bad key"}"#).is_err());
    assert!(parse_user("[]").is_err());
}

#[test]
fn gpu_prices_and_stock_follow_the_cloud_and_the_data_centres() {
    let gpus = parse_gpus(
        r#"[{"gpuId":"A","secureCloud":true,"communityCloud":false,"securePricePerHr":1.5,"communityPricePerHr":null,"stockStatus":"High",
             "dataCenterAvailability":[{"dataCenterId":"EU-RO-1","stockStatus":"High"},{"dataCenterId":"US-CA-2","stockStatus":"None"}]}]"#,
    )
    .unwrap();
    let a = &gpus[0];
    assert_eq!(a.price("SECURE"), Some(1.5));
    assert_eq!(a.price("COMMUNITY"), None);
    assert!(a.in_stock("SECURE", &[]));
    assert!(a.in_stock("SECURE", &s(&["EU-RO-1"])));
    assert!(
        !a.in_stock("SECURE", &s(&["US-CA-2"])),
        "no stock in the only named centre"
    );
    assert!(!a.in_stock("COMMUNITY", &[]), "not offered on that cloud");
}

#[test]
fn ssh_keys_are_counted() {
    assert_eq!(count_ssh_keys("[]").unwrap(), 0);
    assert_eq!(count_ssh_keys(r#"[{"id":"1"},{"id":"2"}]"#).unwrap(), 2);
    // The shape the CLI prints (seen live): an object holding the list.
    assert_eq!(
        count_ssh_keys(
            r#"{"keys":[{"name":"a","type":"ssh-ed25519","key":"k\n","fingerprint":"f"}]}"#
        )
        .unwrap(),
        1
    );
    assert_eq!(count_ssh_keys(r#"{"keys":[]}"#).unwrap(), 0);
    assert!(count_ssh_keys("{}").is_err());
    assert!(count_ssh_keys(r#"{"keys":"nope"}"#).is_err());
    assert!(count_ssh_keys("not json").is_err());
}

#[test]
fn ids_are_checked_before_they_reach_an_argument_list() {
    assert!(is_pod_id("abc123def456"));
    for bad in [
        "",
        "abc",
        "ABC123DEF456",
        "abc 123def",
        "abc;rm -rf",
        "-rf12345",
        "../etc/pas",
        &"a".repeat(41),
    ] {
        assert!(!is_pod_id(bad), "{bad:?}");
        assert!(
            delete_args(bad).is_err() && get_args(bad).is_err(),
            "{bad:?}"
        );
    }
    assert_eq!(
        delete_args("abc123def456").unwrap(),
        s(&["pod", "delete", "abc123def456"])
    );
    assert_eq!(
        get_args("abc123def456").unwrap(),
        s(&["pod", "get", "abc123def456"])
    );
}

#[test]
fn read_only_arguments_never_name_a_creating_verb() {
    for list in [list_args(), user_args(), gpus_args(), ssh_keys_args()] {
        assert!(
            !list.iter().any(|a| matches!(
                a.as_str(),
                "create" | "start" | "restart" | "update" | "delete"
            )),
            "{list:?}"
        );
    }
    assert_eq!(list_args(), s(&["pod", "list", "--all"]));
}

#[test]
fn create_argv_for_the_minimal_pool() {
    let argv = create_args(
        &config(),
        "img:1",
        "vivi-rp1-n",
        "NVIDIA A100 80GB PCIe",
        ("2026-10-11T14:00:00Z", 120),
    );
    assert_eq!(
        argv,
        s(&[
            "pod",
            "create",
            "--name",
            "vivi-rp1-n",
            "--image",
            "img:1",
            "--gpu-id",
            "NVIDIA A100 80GB PCIe",
            "--gpu-count",
            "1",
            "--cloud-type",
            "SECURE",
            "--container-disk-in-gb",
            "40",
            "--ports",
            "22/tcp",
            "--ssh",
            "--terminate-after",
            "2026-10-11T14:00:00Z",
            "--wait",
            "--wait-timeout",
            "120s",
        ])
    );
}

#[test]
fn create_argv_with_every_option() {
    let mut cfg = config();
    cfg.gpu_count = 4;
    cfg.cloud = "community".into();
    cfg.container_disk_gb = 200;
    cfg.min_cuda_version = Some("12.8".into());
    cfg.data_centers = s(&["EU-RO-1", "US-CA-2"]);
    let argv = create_args(&cfg, "img:1", "n", "G", ("T", 45));
    let after = |flag: &str| {
        let at = argv
            .iter()
            .position(|a| a == flag)
            .unwrap_or_else(|| panic!("{flag} missing: {argv:?}"));
        argv[at + 1].clone()
    };
    assert_eq!(after("--gpu-count"), "4");
    assert_eq!(after("--cloud-type"), "COMMUNITY");
    assert_eq!(after("--container-disk-in-gb"), "200");
    assert_eq!(after("--min-cuda-version"), "12.8");
    assert_eq!(after("--data-center-ids"), "EU-RO-1,US-CA-2");
    assert_eq!(after("--terminate-after"), "T");
    assert_eq!(after("--wait-timeout"), "45s");
    assert!(
        argv.contains(&"--public-ip".to_string()),
        "community pods need a public address"
    );
    assert!(argv.contains(&"--ssh".to_string()) && argv.contains(&"--wait".to_string()));
    assert_eq!(after("--ports"), "22/tcp");
}

#[test]
fn a_secure_pod_asks_for_no_public_address_and_no_unset_options() {
    let argv = create_args(&config(), "i", "n", "G", ("T", 120));
    for absent in ["--public-ip", "--min-cuda-version", "--data-center-ids"] {
        assert!(!argv.iter().any(|a| a == absent), "{absent}");
    }
}

#[test]
fn the_api_key_never_appears_in_any_argument_list() {
    let argv = create_args(&config(), "i", "n", "G", ("T", 120));
    assert!(!argv.iter().any(|a| a.contains("rpa_")
        || a.to_lowercase().contains("api-key")
        || a.to_lowercase().contains("api_key")));
}

#[test]
fn pod_ids_are_found_in_a_json_field_or_after_the_word_pod() {
    let ids = |t: &str| pod_ids_in(t);
    assert_eq!(
        ids(r#"{"error":"timed out waiting for ssh","id":"abc123def456"}"#),
        s(&["abc123def456"])
    );
    assert_eq!(
        ids(r#"{"error":"timed out waiting for ssh on pod xedezhzb9la3ye; the pod was kept"}"#),
        s(&["xedezhzb9la3ye"])
    );
    assert_eq!(
        ids("timed out (pod_id: zz11yy22xx33)"),
        s(&["zz11yy22xx33"])
    );
    assert!(ids("timed out waiting for the pod's SSH port").is_empty());
    assert_eq!(
        ids("pod abc123def456 pod abc123def456 pod x"),
        s(&["abc123def456"]),
        "a repeated id is listed once and a short word is no id"
    );
}
