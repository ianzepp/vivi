use std::os::unix::fs::PermissionsExt;

use super::*;
use crate::pool::doctor::{Check, Status};
use crate::pool::provider::PoolProvider;
use crate::pool::runpod_fixture::{NOW, market, pod_name, rig, rig_keyed};
use crate::pool::runpod_pod::{User, parse_gpus};

fn good() -> RunpodFacts {
    RunpodFacts {
        pool: "gpu".into(),
        key_file: "/keys/runpod.key".into(),
        cli: Ok("runpodctl version 2.1.9".into()),
        key: Ok(()),
        account: Some(Ok(User {
            id: "user_1".into(),
            client_balance: Some(80.0),
            current_spend_per_hr: Some(0.0),
        })),
        ssh_keys: Some(Ok(1)),
        missing_tools: Vec::new(),
        gpus: Some(Ok(parse_gpus(&market(&[(
            "NVIDIA A100 80GB PCIe",
            1.19,
            "High",
        )]))
        .unwrap())),
        pods: Some(Ok(Survey::default())),
        types: vec!["NVIDIA A100 80GB PCIe".into()],
        cpu: false,
        cloud: "SECURE".into(),
        centers: Vec::new(),
        gpu_count: 1,
        image: "runpod/pytorch:2.4.0-runtime".into(),
        ready_timeout: 120,
        now: NOW,
    }
}

fn status(list: &[Check], name: &str) -> Status {
    list.iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("no check {name}: {list:#?}"))
        .status
}

fn detail<'a>(list: &'a [Check], name: &str) -> &'a str {
    &list.iter().find(|c| c.name == name).unwrap().detail
}

fn pod(id: &str, name: &str) -> Pod {
    Pod {
        id: id.into(),
        name: name.into(),
        desired_status: "RUNNING".into(),
        runtime_status: "running".into(),
        cost_per_hr: Some(1.19),
        gpu_count: 1,
        ..Pod::default()
    }
}

fn pools() -> Vec<String> {
    vec!["gpu".into(), "other".into()]
}

#[test]
fn a_healthy_account_passes_every_check() {
    let list = checks(&good());
    assert!(
        list.iter().all(|c| matches!(c.status, Status::Ok)),
        "{list:#?}"
    );
    assert!(list.iter().all(|c| c.group == GROUP));
    assert!(detail(&list, "account").contains("user_1"));
    assert!(detail(&list, "api key file").contains("not read by vivi"));
    assert!(detail(&list, "pods").contains("none of pool gpu exist"));
}

#[test]
fn a_missing_cli_fails_and_everything_behind_it_is_skipped() {
    let mut f = good();
    f.cli = Err("`runpodctl` is not installed".into());
    f.account = None;
    f.ssh_keys = None;
    f.gpus = None;
    f.pods = None;
    let list = checks(&f);
    assert_eq!(status(&list, "runpodctl"), Status::Fail);
    for name in ["account", "ssh keys", "gpu types", "pods"] {
        assert_eq!(status(&list, name), Status::Skipped, "{name}");
    }
}

#[test]
fn a_key_file_open_to_others_fails_with_the_chmod_fix() {
    let mut f = good();
    f.key = Err("the RunPod API key file /keys/runpod.key is readable by group or others".into());
    let list = checks(&f);
    let check = list.iter().find(|c| c.name == "api key file").unwrap();
    assert_eq!(check.status, Status::Fail);
    assert_eq!(check.fix.as_deref(), Some("chmod 600 /keys/runpod.key"));
}

#[test]
fn a_refused_key_fails_the_account_check_without_echoing_a_key() {
    let mut f = good();
    f.account = Some(Err("`runpodctl user` failed: unauthorized".into()));
    f.ssh_keys = None;
    f.gpus = None;
    f.pods = None;
    let list = checks(&f);
    assert_eq!(status(&list, "account"), Status::Fail);
    assert!(detail(&list, "account").contains("did not accept the key"));
    assert_eq!(status(&list, "pods"), Status::Skipped);
}

#[test]
fn no_ssh_key_and_a_low_balance_are_warnings() {
    let mut f = good();
    f.ssh_keys = Some(Ok(0));
    f.account = Some(Ok(User {
        id: "u".into(),
        client_balance: Some(1.25),
        current_spend_per_hr: None,
    }));
    let list = checks(&f);
    assert_eq!(status(&list, "ssh keys"), Status::Warn);
    assert_eq!(status(&list, "account"), Status::Warn);
    assert!(detail(&list, "account").contains("$1.25"));
}

#[test]
fn a_gpu_type_is_checked_against_the_market() {
    let mut f = good();
    f.types = vec![
        "NVIDIA A100 80GB PCIe".into(),
        "NVIDIA Imaginary".into(),
        "NVIDIA H100 PCIe".into(),
    ];
    f.gpus = Some(Ok(parse_gpus(&market(&[
        ("NVIDIA A100 80GB PCIe", 1.19, "High"),
        ("NVIDIA H100 PCIe", 2.39, "None"),
    ]))
    .unwrap()));
    let list = checks(&f);
    assert_eq!(status(&list, "gpu NVIDIA A100 80GB PCIe"), Status::Ok);
    assert!(detail(&list, "gpu NVIDIA A100 80GB PCIe").contains("$1.19/h"));
    assert_eq!(status(&list, "gpu NVIDIA Imaginary"), Status::Warn);
    assert_eq!(status(&list, "gpu NVIDIA H100 PCIe"), Status::Warn);
    assert!(detail(&list, "gpu NVIDIA H100 PCIe").contains("no stock"));
}

#[test]
fn a_cpu_pool_checks_the_compute_tier_and_never_the_gpu_market() {
    let mut f = good();
    f.cpu = true;
    f.types = Vec::new();
    let list = checks(&f);
    assert_eq!(status(&list, "compute"), Status::Ok);
    assert!(
        detail(&list, "compute").contains("cpu pods"),
        "{}",
        detail(&list, "compute")
    );
    assert!(
        list.iter()
            .all(|c| !c.name.starts_with("gpu ") && c.name != "gpu types"),
        "no gpu check on a cpu pool: {list:#?}"
    );
}

#[test]
fn the_cpu_spend_refusal_names_the_cpu_tier_and_the_flag() {
    let text = cpu_spend_text();
    assert!(
        text.contains("CPU pod") && text.contains("--allow-spend"),
        "{text}"
    );
}

#[test]
fn a_devel_image_is_warned_about() {
    let mut f = good();
    f.image = "runpod/pytorch:2.4.0-devel-ubuntu22.04".into();
    assert_eq!(status(&checks(&f), "image"), Status::Warn);
}

#[test]
fn missing_local_ssh_tools_fail() {
    let mut f = good();
    f.missing_tools = vec!["ssh-keyscan".into()];
    let list = checks(&f);
    assert_eq!(status(&list, "local ssh tools"), Status::Fail);
    assert!(detail(&list, "local ssh tools").contains("ssh-keyscan"));
}

#[test]
fn the_survey_sorts_pods_into_mine_strays_others_and_malformed() {
    let pods = vec![
        pod("rp000001", &pod_name(1, "t1", "aaaaaaaaaaaa")),
        pod(
            "rp000002",
            &crate::pool::runpod_name::PodName {
                pool: "retired".into(),
                ..crate::pool::runpod_name::PodName::decode(&pod_name(1, "t", "bbbbbbbbbbbb"))
                    .unwrap()
            }
            .encode()
            .unwrap(),
        ),
        pod(
            "rp000003",
            &crate::pool::runpod_name::PodName {
                pool: "other-doctor".into(),
                ..crate::pool::runpod_name::PodName::decode(&pod_name(1, "t", "cccccccccccc"))
                    .unwrap()
            }
            .encode()
            .unwrap(),
        ),
        pod(
            "rp000004",
            &crate::pool::runpod_name::PodName {
                mailspace: "zz".into(),
                ..crate::pool::runpod_name::PodName::decode(&pod_name(1, "t", "dddddddddddd"))
                    .unwrap()
            }
            .encode()
            .unwrap(),
        ),
        pod("rp000005", "vivi-rp1-garbled"),
        pod("rp000006", "somebody-elses-training-run"),
    ];
    let survey = Survey::of(&pods, "ms", "gpu", &pools());
    assert_eq!(
        survey
            .mine
            .iter()
            .map(|p| p.id.as_str())
            .collect::<Vec<_>>(),
        ["rp000001"]
    );
    assert_eq!(
        survey
            .strays
            .iter()
            .map(|p| p.id.as_str())
            .collect::<Vec<_>>(),
        ["rp000002"],
        "a pool the mailspace does not have"
    );
    assert_eq!(
        survey.others, 1,
        "another mailspace's pod is counted and left alone"
    );
    assert_eq!(
        survey.malformed,
        [("rp000005".to_string(), "vivi-rp1-garbled".to_string())]
    );
    // `other-doctor` is the self-test pool of the known pool `other`: neither stray nor mine.
    assert!(!survey.strays.iter().any(|p| p.id == "rp000003"));
}

#[test]
fn strays_fail_with_the_cost_framing_and_a_delete_command() {
    let mut f = good();
    let stray = pod(
        "rp000002",
        &crate::pool::runpod_name::PodName {
            pool: "retired".into(),
            ..crate::pool::runpod_name::PodName::decode(&pod_name(1, "t", "bbbbbbbbbbbb")).unwrap()
        }
        .encode()
        .unwrap(),
    );
    f.pods = Some(Ok(Survey::of(
        &[stray, pod("rp000005", "vivi-rp1-garbled")],
        "ms",
        "gpu",
        &pools(),
    )));
    let list = checks(&f);
    let check = list.iter().find(|c| c.name == "stray pods").unwrap();
    assert_eq!(check.status, Status::Fail);
    assert!(
        check.detail.contains("billing until terminated")
            && check.detail.contains("rp000002")
            && check.detail.contains("rp000005"),
        "{}",
        check.detail
    );
    assert_eq!(
        check
            .fix
            .as_deref()
            .map(|f| f.starts_with("runpodctl pod delete rp000002")),
        Some(true)
    );
}

#[test]
fn a_pod_of_the_pool_is_listed_with_its_cost_and_deadline_and_a_late_one_fails() {
    let mut f = good();
    let live = pod("rp000001", &pod_name(1, "t1", "aaaaaaaaaaaa"));
    f.pods = Some(Ok(Survey::of(&[live.clone()], "ms", "gpu", &pools())));
    let list = checks(&f);
    assert_eq!(status(&list, "pods"), Status::Ok);
    let text = detail(&list, "pods");
    assert!(
        text.contains("rp000001")
            && text.contains("task t1")
            && text.contains("$1.19/h")
            && text.contains("ends by"),
        "{text}"
    );
    f.now = NOW + 6000;
    let late = checks(&f);
    assert_eq!(status(&late, "pods"), Status::Fail);
    assert!(detail(&late, "pods").contains("past the provider-side deadline"));
    let fix = late
        .iter()
        .find(|c| c.name == "pods")
        .unwrap()
        .fix
        .clone()
        .unwrap();
    assert!(fix.contains("vivi pool reconcile --pool gpu"), "{fix}");
}

#[test]
fn the_refusal_text_says_it_would_rent_names_the_price_and_the_flag() {
    let text = spend_text(
        &["NVIDIA A100 80GB PCIe".into(), "NVIDIA H100 PCIe".into()],
        2,
        "SECURE",
        Some(1.19),
    );
    assert!(text.contains("would rent a real GPU pod"), "{text}");
    assert!(
        text.contains("$1.19/h per GPU") && text.contains("$2.38/h"),
        "{text}"
    );
    assert!(
        text.contains("--allow-spend") && text.contains("Nothing was rented"),
        "{text}"
    );
    assert!(
        text.contains("bills by the second") && text.contains("reconcile"),
        "{text}"
    );
    let unknown = spend_text(&["X".into()], 1, "COMMUNITY", None);
    assert!(
        unknown.contains("price could not be read") && unknown.contains("--allow-spend"),
        "{unknown}"
    );
}

fn private_key(dir: &std::path::Path, mode: u32) -> String {
    let path = dir.join("runpod.key");
    std::fs::write(&path, "rpa_not_a_real_key\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
    path.display().to_string()
}

#[test]
fn the_provider_doctor_reads_the_account_and_spends_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let key = private_key(dir.path(), 0o600);
    let r = rig_keyed(2, "", &key);
    {
        let mut w = r.account.world();
        w.add(&pod_name(1, "t1", "aaaaaaaaaaaa"), 5);
    }
    let list = r.provider.doctor(&[]);
    assert_eq!(status(&list, "account"), Status::Ok, "{list:#?}");
    assert_eq!(status(&list, "api key file"), Status::Ok);
    assert_eq!(status(&list, "runpodctl"), Status::Ok);
    assert!(detail(&list, "pods").contains("rp000001"));
    let w = r.account.world();
    assert_eq!(w.spending_calls(), 0);
    for call in &w.calls {
        assert!(
            matches!(
                call.iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .as_slice(),
                ["user"] | ["gpu", "list"] | ["ssh", "list-keys"] | ["pod", "list", "--all"]
            ),
            "a call that is not a plain read: {call:?}"
        );
    }
    let all = format!("{list:#?}");
    assert!(
        !all.contains("rpa_not_a_real_key"),
        "the key is never read into a report"
    );
}

#[test]
fn a_key_file_open_to_others_stops_the_provider_doctor_before_any_call() {
    let dir = tempfile::tempdir().unwrap();
    let key = private_key(dir.path(), 0o644);
    let r = rig_keyed(1, "", &key);
    let list = r.provider.doctor(&[]);
    assert_eq!(status(&list, "api key file"), Status::Fail);
    assert_eq!(status(&list, "account"), Status::Skipped);
    assert!(
        r.account.world().calls.is_empty(),
        "no call is made with a key file that is too open"
    );
}

#[test]
fn a_refused_key_is_reported_by_the_provider_doctor() {
    let dir = tempfile::tempdir().unwrap();
    let key = private_key(dir.path(), 0o600);
    let r = rig_keyed(1, "", &key);
    r.account.world().auth_fails = true;
    let list = r.provider.doctor(&[]);
    assert_eq!(status(&list, "account"), Status::Fail);
    assert_eq!(status(&list, "pods"), Status::Skipped);
}

#[test]
fn the_spend_notice_quotes_the_first_types_price_from_the_market() {
    let r = rig(1, "gpu_count = 2\n");
    let notice = r.provider.spend_notice();
    assert!(
        notice.contains("$1.19/h per GPU") && notice.contains("$2.38/h"),
        "{notice}"
    );
    assert_eq!(r.account.world().spending_calls(), 0);
}

#[test]
fn the_doctor_shows_the_effective_ready_timeout() {
    let list = checks(&good());
    let c = list.iter().find(|c| c.name == "ready timeout").unwrap();
    assert_eq!(c.status, Status::Ok);
    assert!(
        c.detail.contains("120s")
            && c.detail.contains("1 type(s)")
            && c.detail.contains("--ready-timeout"),
        "{}",
        c.detail
    );
    let r = rig(1, "ready_timeout = 45");
    let facts = r.provider.gather();
    assert_eq!(facts.ready_timeout, 45);
    let list = checks(&facts);
    let c = list.iter().find(|c| c.name == "ready timeout").unwrap();
    assert!(
        c.detail.contains("45s") && c.detail.contains("90s of waiting across 2"),
        "{}",
        c.detail
    );
}
