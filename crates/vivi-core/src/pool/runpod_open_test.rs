use std::os::unix::fs::PermissionsExt;

use super::*;
use crate::pool::provider::PoolProvider;
use crate::pool::runpod_fixture::Rig;
use crate::pool::runpod_fixture::{
    HOST_KEY, NOW, OTHER_KEY, pod_name, recording, request, rig, scan_text,
};

fn open_one(r: &crate::pool::runpod_fixture::Rig, task: &str) -> Result<OpenedSlot, VivariumError> {
    let (progress, _) = recording();
    r.provider.open(&request(&r.id, task, &progress))
}

/// The deadline of a 600 s job from `NOW` on the rig's two GPU types when each
/// pod gets `ready` seconds.
fn due(r: &Rig, ready: u64) -> u64 {
    deadline_of(NOW, 600, setup_secs(2, ready, r.provider.pace.seed_limit))
}

#[test]
fn the_deadline_is_every_wait_plus_seeding_plus_budget_plus_the_silence_the_wait_allows() {
    let seed = Duration::from_mins(30);
    for (types, ready) in [(1_usize, 120_u64), (3, 120), (2, 900)] {
        let setup = setup_secs(types, ready, seed);
        assert_eq!(setup, types as u64 * ready + 1800);
        let end = deadline_of(1000, 600, setup);
        assert_eq!(end, 1000 + setup + 600 + TIMEOUT_GRACE_SECS);
        assert!(
            end > 1000 + 600,
            "a job gets all of its budget before the pod is due to end"
        );
    }
    assert!(
        deadline_of(0, 0, setup_secs(3, 120, seed)) > deadline_of(0, 0, setup_secs(1, 120, seed)),
        "more GPU types to wait for push the deadline out"
    );
}

#[test]
fn the_deadline_of_a_dispatch_follows_the_pool_and_the_override() {
    let r = rig(1, "");
    let cfg = &r.config.runpod;
    let base = dispatch_deadline(cfg, None, (NOW, 600));
    assert_eq!(base, deadline_of(NOW, 600, 2 * 120 + 1800));
    let longer = dispatch_deadline(cfg, Some(300), (NOW, 600));
    assert_eq!(longer, base + 2 * (300 - 120));
    let mut one = cfg.clone();
    one.gpu_types.truncate(1);
    assert_eq!(
        dispatch_deadline(&one, None, (NOW, 600)),
        deadline_of(NOW, 600, 120 + 1800)
    );
}

#[test]
fn a_dispatch_rents_one_pod_named_for_it_with_a_provider_side_deadline() {
    let r = rig(2, "");
    let opened = open_one(&r, "abc12345").unwrap();
    let w = r.account.world();
    assert_eq!(w.live().len(), 1);
    let pod = &w.pods[0];
    let name = PodName::decode(&pod.name).expect("the pod carries Vivi's name layout");
    assert_eq!(
        (name.mailspace.as_str(), name.pool.as_str(), name.index),
        ("ms", "gpu", 1)
    );
    assert_eq!(name.task, "abc12345");
    assert_eq!(name.claimed, NOW);
    assert_eq!(name.deadline, due(&r, 120));
    assert_eq!(
        pod.terminate_after,
        iso(name.deadline),
        "the provider is asked to end the pod by the deadline in its name"
    );
    assert_eq!(opened.id, pod.id);
    assert_eq!(opened.index, 1);
    assert_eq!(opened.name, "ms-gpu-slot-1");
    assert!(
        opened.hardware.starts_with("NVIDIA A100 80GB PCIe x1"),
        "{}",
        opened.hardware
    );
    assert_eq!(
        w.spending_calls(),
        1,
        "one create and nothing else that spends"
    );
}

#[test]
fn the_pod_is_reached_over_the_pinned_key_and_seeded_in_order() {
    let r = rig(1, "");
    open_one(&r, "t1").unwrap();
    let id = r.account.world().pods[0].id.clone();
    let pin = runpod_pin::pin_path(&r.dir.path().join("runpod/pins"), &id);
    let text = std::fs::read_to_string(&pin).unwrap();
    assert!(
        text.contains(HOST_KEY) && text.starts_with("[203.0.113."),
        "{text}"
    );
    assert_eq!(
        std::fs::metadata(&pin).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let pod = r.pod.state();
    assert!(
        pod.links.iter().all(|(_, _, p)| *p == pin),
        "{:?}",
        pod.links
    );
    let verbs: Vec<&str> = pod.verbs.iter().map(String::as_str).collect();
    let at = |v: &str| {
        verbs
            .iter()
            .position(|x| *x == v)
            .unwrap_or_else(|| panic!("{v} not in {verbs:?}"))
    };
    assert!(
        at("probe") < at("create")
            && at("create") < at("start")
            && at("start") < at("seal")
            && at("seal") < at("claim"),
        "{verbs:?}"
    );
    assert_eq!(
        pod.marks["vivi_pool_state"],
        crate::pool::slot::STATE_READY,
        "sealed ready"
    );
    assert_eq!(pod.claimed_by.as_deref(), Some("t1"));
    assert!(pod.files.contains_key("boot.sh") && pod.files.contains_key("slot.env"));
}

#[test]
fn the_api_key_never_reaches_the_pod() {
    let r = rig(1, "");
    open_one(&r, "t1").unwrap();
    let pod = r.pod.state();
    for (name, content) in &pod.files {
        let text = String::from_utf8_lossy(content);
        assert!(
            !text.contains("RUNPOD")
                && !text.contains("rpa_")
                && !text.contains("/keys/runpod.key"),
            "{name}"
        );
    }
}

#[test]
fn the_first_gpu_type_with_stock_is_taken_and_the_actual_type_is_recorded() {
    let r = rig(1, "");
    r.account
        .world()
        .no_stock
        .push("NVIDIA A100 80GB PCIe".into());
    let opened = open_one(&r, "t1").unwrap();
    assert!(
        opened.hardware.starts_with("NVIDIA H100 PCIe"),
        "{}",
        opened.hardware
    );
    let w = r.account.world();
    assert_eq!(w.live().len(), 1);
    assert_eq!(
        w.calls_to(&["pod", "create"]).len(),
        2,
        "the refused type was tried first"
    );
}

#[test]
fn the_market_orders_types_without_removing_any() {
    let types = vec!["A".to_string(), "B".to_string(), "C".to_string()];
    let market = crate::pool::runpod_pod::parse_gpus(&crate::pool::runpod_fixture::market(&[
        ("A", 1.0, "None"),
        ("B", 1.0, "High"),
    ]))
    .unwrap();
    assert_eq!(
        order_types(&types, Some(&market), "SECURE", &[]),
        ["B", "C"],
        "C is unknown to the market and stays"
    );
    assert_eq!(order_types(&types, None, "SECURE", &[]), ["A", "B", "C"]);
    let none = crate::pool::runpod_pod::parse_gpus(&crate::pool::runpod_fixture::market(&[
        ("A", 1.0, "None"),
        ("B", 1.0, "None"),
        ("C", 1.0, "None"),
    ]))
    .unwrap();
    assert_eq!(
        order_types(&types, Some(&none), "SECURE", &[]),
        ["A", "B", "C"],
        "all out of stock: try them all in order"
    );
}

#[test]
fn when_no_type_can_be_rented_nothing_is_left_behind() {
    let r = rig(1, "");
    r.account.world().no_stock = vec!["NVIDIA A100 80GB PCIe".into(), "NVIDIA H100 PCIe".into()];
    let said = open_one(&r, "t1").unwrap_err().to_string();
    assert!(
        said.contains("no pod could be rented") && said.contains("2 GPU type"),
        "{said}"
    );
    assert!(r.account.world().pods.is_empty());
}

#[test]
fn a_full_pool_makes_no_pod() {
    let r = rig(1, "");
    r.account
        .world()
        .add(&pod_name(1, "other", "aaaaaaaaaaaa"), 1);
    let said = open_one(&r, "t1").unwrap_err().to_string();
    assert!(
        said.contains("no free slot") && said.contains("allows 1"),
        "{said}"
    );
    assert!(
        r.account.world().calls_to(&["pod", "create"]).is_empty(),
        "the cap is checked before anything is rented"
    );
}

#[test]
fn pods_of_other_pools_and_mailspaces_do_not_fill_the_pool() {
    let r = rig(1, "");
    {
        let mut w = r.account.world();
        let other_pool = PodName {
            pool: "elsewhere".into(),
            ..PodName::decode(&pod_name(1, "x", "bbbbbbbbbbbb")).unwrap()
        };
        let other_space = PodName {
            mailspace: "zz".into(),
            ..PodName::decode(&pod_name(1, "x", "cccccccccccc")).unwrap()
        };
        w.add(&other_pool.encode().unwrap(), 1);
        w.add(&other_space.encode().unwrap(), 1);
        w.add("someone-elses-pod", 1);
        w.add("vivi-rp1-garbage", 1);
    }
    open_one(&r, "t1").unwrap();
    assert_eq!(
        r.account.world().pods.len(),
        5,
        "only one pod was added; none of the others was touched"
    );
}

#[test]
fn a_new_pod_takes_the_lowest_unused_index() {
    let r = rig(4, "");
    {
        let mut w = r.account.world();
        w.add(&pod_name(1, "a", "aaaaaaaaaaaa"), 1);
        w.add(&pod_name(3, "b", "bbbbbbbbbbbb"), 2);
    }
    let opened = open_one(&r, "t1").unwrap();
    assert_eq!(opened.index, 2);
}

#[test]
fn a_dispatcher_that_made_its_pod_second_loses_a_shared_index_and_ends_its_pod() {
    let r = rig(3, "");
    // A rival picks the same index; the provider made its pod first.
    let rival = pod_name(1, "rival", "dddddddddddd");
    r.account.world().after_create = Some(Box::new(move |w| {
        let earlier = w.clock() - 5000;
        w.add(&rival, earlier);
    }));
    let said = open_one(&r, "t1").unwrap_err().to_string();
    assert!(
        said.contains("lost its slot") && said.contains("slot 1"),
        "{said}"
    );
    let w = r.account.world();
    assert_eq!(w.pods.len(), 1, "only the rival's pod remains");
    assert!(w.pods[0].name.contains("dddddddddddd"));
    assert!(!runpod_pin::pin_path(&r.dir.path().join("runpod/pins"), &w.pods[0].id).exists());
    drop(w);
    let pins = r.dir.path().join("runpod/pins");
    assert!(
        !pins.exists() || std::fs::read_dir(&pins).unwrap().count() == 0,
        "the loser kept no pin"
    );
}

#[test]
fn a_dispatcher_that_made_its_pod_first_keeps_it_and_leaves_the_rival_alone() {
    let r = rig(3, "");
    let rival = pod_name(1, "rival", "dddddddddddd");
    r.account.world().after_create = Some(Box::new(move |w| {
        let later = w.clock() + 5000;
        w.add(&rival, later);
    }));
    let opened = open_one(&r, "t1").unwrap();
    assert_eq!(opened.index, 1);
    assert_eq!(
        r.account.world().pods.len(),
        2,
        "the rival is the rival's to end"
    );
    assert!(r.account.world().calls_to(&["pod", "delete"]).is_empty());
}

#[test]
fn a_pod_over_the_cap_ends_itself() {
    let r = rig(1, "");
    // Another dispatcher's pod (another index) was made before ours and is not
    // listed yet when we count, but is when we verify.
    let other = pod_name(2, "rival", "eeeeeeeeeeee");
    r.account.world().after_create = Some(Box::new(move |w| {
        let earlier = w.clock() - 5000;
        w.add(&other, earlier);
    }));
    let said = open_one(&r, "t1").unwrap_err().to_string();
    assert!(
        said.contains("lost its slot") && said.contains("allows 1"),
        "{said}"
    );
    assert_eq!(r.account.world().pods.len(), 1);
}

#[test]
fn a_rival_the_provider_cannot_order_makes_this_dispatch_give_way() {
    let r = rig(3, "");
    let rival = pod_name(1, "rival", "ffffffffffff");
    r.account.world().after_create = Some(Box::new(move |w| {
        let id = w.add(&rival, 0);
        // `pod get` and the listing both lack a creation time for it.
        w.pods.iter_mut().find(|p| p.id == id).unwrap().created_ms = i64::MIN;
    }));
    let said = open_one(&r, "t1").unwrap_err().to_string();
    assert!(said.contains("lost its slot"), "{said}");
    assert_eq!(r.account.world().pods.len(), 1);
}

#[test]
fn a_host_key_that_changes_between_scans_fails_closed_and_ends_the_pod() {
    let r = rig(1, "");
    *r.scans.lock().unwrap() = vec![scan_text(&[HOST_KEY]), scan_text(&[OTHER_KEY])];
    let said = open_one(&r, "t1").unwrap_err().to_string();
    assert!(said.contains("changed between two scans"), "{said}");
    assert!(r.account.world().pods.is_empty(), "the pod is terminated");
    assert!(
        r.pod.state().verbs.is_empty(),
        "nothing was sent to a pod whose key was not pinned"
    );
    assert!(
        !r.dir.path().join("runpod/pins").exists()
            || std::fs::read_dir(r.dir.path().join("runpod/pins"))
                .unwrap()
                .count()
                == 0
    );
}

#[test]
fn an_image_without_the_tools_the_scripts_need_ends_the_pod() {
    let r = rig(1, "");
    r.pod.state().missing = vec!["git".into(), "setsid".into()];
    let said = open_one(&r, "t1").unwrap_err().to_string();
    assert!(
        said.contains("lacks what the slot scripts need") && said.contains("git, setsid"),
        "{said}"
    );
    assert!(r.account.world().pods.is_empty());
}

#[test]
fn a_seed_that_fails_ends_the_pod_and_says_why() {
    let r = rig(1, "");
    r.pod.state().seed_exit = 3;
    let said = open_one(&r, "t1").unwrap_err().to_string();
    assert!(said.contains("seed failed (exit 3)"), "{said}");
    assert!(r.account.world().pods.is_empty());
}

#[test]
fn an_unreachable_pod_ends_the_pod_too() {
    let r = rig(1, "");
    r.pod.state().unreachable = Some("Connection timed out".into());
    assert!(open_one(&r, "t1").is_err());
    assert!(r.account.world().pods.is_empty());
}

#[test]
fn a_failed_termination_says_the_pod_is_still_billing_and_how_to_end_it() {
    let r = rig(1, "");
    r.pod.state().seed_exit = 1;
    r.account.world().delete_fails = true;
    let said = open_one(&r, "t1").unwrap_err().to_string();
    assert!(
        said.contains("could not be terminated") && said.contains("bills until"),
        "{said}"
    );
    assert!(said.contains("runpodctl pod delete rp"), "{said}");
    assert!(
        said.contains(&iso(due(&r, 120))),
        "the provider-side deadline is stated: {said}"
    );
    assert!(!said.contains("/keys/runpod.key\n"));
}

#[test]
fn a_create_that_fails_after_making_a_pod_removes_that_pod_before_the_next_try() {
    let r = rig(1, "");
    r.account.world().half_made = vec!["NVIDIA A100 80GB PCIe".into()];
    let opened = open_one(&r, "t1").unwrap();
    assert!(
        opened.hardware.starts_with("NVIDIA H100 PCIe"),
        "{}",
        opened.hardware
    );
    let w = r.account.world();
    assert_eq!(w.live().len(), 1, "the half-made pod is not left billing");
    assert_eq!(w.calls_to(&["pod", "delete"]).len(), 1);
}

#[test]
fn when_every_create_leaves_a_half_made_pod_none_remains() {
    let r = rig(1, "");
    r.account.world().half_made = vec!["NVIDIA A100 80GB PCIe".into(), "NVIDIA H100 PCIe".into()];
    let said = open_one(&r, "t1").unwrap_err().to_string();
    assert!(said.contains("no pod could be rented"), "{said}");
    assert!(r.account.world().pods.is_empty());
}

#[test]
fn harness_environment_travels_inside_the_pinned_connection_and_not_the_api() {
    let r = rig(1, "");
    // SAFETY-free approach: name a variable that is certainly set and non-empty.
    let var = "PATH";
    let (progress, _) = recording();
    let mut req = request(&r.id, "t1", &progress);
    req.spec.passthrough_env = vec![var.into(), "VIVI_SURELY_UNSET_VARIABLE".into()];
    r.provider.open(&req).unwrap();
    let pod = r.pod.state();
    let env = String::from_utf8_lossy(&pod.files["slot.env"]).into_owned();
    assert!(env.contains("PATH="), "{env}");
    assert!(!env.contains("VIVI_SURELY_UNSET_VARIABLE"));
    for call in &r.account.world().calls {
        assert!(
            !call.iter().any(|a| a.contains("PATH=")),
            "no value goes through runpodctl: {call:?}"
        );
    }
}

#[test]
fn the_uncollected_note_states_the_cost_and_the_unverified_deadline() {
    let record = crate::pool::record::DispatchRecord {
        target: None,
        repository: None,
        pool: "gpu".into(),
        need: None,
        kind: None,
        slot: "ms-gpu-slot-1".into(),
        machine: "rp000001".into(),
        job: "j".into(),
        model: "m/ok".into(),
        from: "main".into(),
        branch: "b".into(),
        start: "a".repeat(40),
        with: Vec::new(),
        budget: 600,
        at: NOW,
        read_only: false,
        hardware: "NVIDIA A100 80GB PCIe x1, $1.19/h".into(),
        abandoned: 0,
        abandoned_cost_micros: 0,
    };
    let deadline = deadline_of(NOW, 600, setup_secs(1, 120, Duration::from_mins(30)));
    let note = uncollected_note(&record, deadline);
    assert!(
        note.contains("bills by the second")
            && note.contains("rp000001")
            && note.contains("$1.19/h"),
        "{note}"
    );
    assert!(note.contains(&iso(deadline)), "{note}");
    assert!(note.contains("not verified"), "{note}");
}

#[test]
fn the_pod_deadline_in_the_create_is_an_iso_time_in_the_future_of_the_dispatch() {
    let r = rig(1, "");
    open_one(&r, "t1").unwrap();
    let w = r.account.world();
    let create = &w.calls_to(&["pod", "create"])[0];
    let at = create
        .iter()
        .position(|a| a == "--terminate-after")
        .unwrap();
    let when = chrono::DateTime::parse_from_rfc3339(&create[at + 1]).unwrap();
    assert!(when.timestamp() > i64::try_from(NOW).unwrap() + 600);
}

#[test]
fn two_dispatches_in_one_process_are_serialised_by_the_lock_file() {
    let r = rig(2, "");
    let lock = runpod_lock::path_for(&r.provider.locks_dir(), "ms", "gpu");
    let _held = runpod_lock::acquire(&lock, Duration::ZERO, Duration::ZERO, &|_| {}).unwrap();
    let said = open_one(&r, "t1").unwrap_err().to_string();
    assert!(said.contains("another dispatch"), "{said}");
    assert!(r.account.world().calls_to(&["pod", "create"]).is_empty());
}

/// The `--wait-timeout` the first create asked for.
fn wait_asked(r: &Rig) -> String {
    let w = r.account.world();
    let create = &w.calls_to(&["pod", "create"])[0];
    let at = create.iter().position(|a| a == "--wait-timeout").unwrap();
    create[at + 1].clone()
}

#[test]
fn a_new_pod_gets_120_seconds_unless_the_pool_or_the_dispatch_says_otherwise() {
    let r = rig(1, "");
    open_one(&r, "t1").unwrap();
    assert_eq!(wait_asked(&r), "120s");

    let r = rig(1, "ready_timeout = 45");
    open_one(&r, "t1").unwrap();
    assert_eq!(wait_asked(&r), "45s", "the pool's setting");
    let w = r.account.world();
    let name = PodName::decode(&w.pods[0].name).unwrap();
    assert_eq!(name.deadline, deadline_of(NOW, 600, 2 * 45 + 20));
    drop(w);

    let r = rig(1, "ready_timeout = 45");
    let (progress, said) = recording();
    let mut req = request(&r.id, "t1", &progress);
    req.ready_timeout_secs = Some(200);
    r.provider.open(&req).unwrap();
    assert_eq!(wait_asked(&r), "200s", "the dispatch's override wins");
    let w = r.account.world();
    let name = PodName::decode(&w.pods[0].name).unwrap();
    assert_eq!(
        name.deadline,
        due(&r, 200),
        "the provider-side deadline allows every type its full wait"
    );
    assert!(
        said.lock()
            .unwrap()
            .iter()
            .any(|l| l.contains("200s to answer on SSH")),
        "the wait is announced"
    );
}

#[test]
fn a_pod_that_never_answers_is_terminated_reported_and_the_next_type_works() {
    let r = rig(1, "");
    r.account.world().half_made_with_id = vec!["NVIDIA A100 80GB PCIe".into()];
    let (progress, said) = recording();
    let opened = r.provider.open(&request(&r.id, "t1", &progress)).unwrap();
    assert!(
        opened.hardware.starts_with("NVIDIA H100 PCIe"),
        "the hardware of the pod that works is recorded: {}",
        opened.hardware
    );
    assert_eq!(opened.abandoned.len(), 1);
    let gone = &opened.abandoned[0];
    assert_eq!(gone.gpu, "NVIDIA A100 80GB PCIe");
    assert!(gone.cost_micros.is_some(), "the price was known");
    let w = r.account.world();
    assert_eq!(w.live().len(), 1, "only the pod that works is left");
    assert!(
        w.pods.iter().all(|p| p.id != gone.id),
        "the abandoned pod is confirmed gone from the list"
    );
    assert_ne!(gone.id, opened.id);
    assert!(
        w.calls_to(&["pod", "get", gone.id.as_str()]).len() == 1,
        "the id in the error was used"
    );
    let lines = said.lock().unwrap().join("\n");
    assert!(
        lines.contains("abandoned pod")
            && lines.contains(&gone.id)
            && lines.contains("NVIDIA A100 80GB PCIe")
            && lines.contains("within 120s")
            && lines.contains("terminated after")
            && lines.contains("approximate charge $"),
        "{lines}"
    );
}

#[test]
fn a_timed_out_create_whose_error_names_no_pod_is_found_by_its_nonce() {
    let r = rig(1, "");
    r.account.world().half_made = vec!["NVIDIA A100 80GB PCIe".into()];
    let opened = open_one(&r, "t1").unwrap();
    assert_eq!(opened.abandoned.len(), 1);
    let w = r.account.world();
    assert!(
        w.calls_to(&["pod", "get"])
            .iter()
            .all(|c| c[2] == opened.id),
        "no id was named, so no pod was asked for by one: {:?}",
        w.calls_to(&["pod", "get"])
    );
    assert_eq!(w.live().len(), 1);
    assert!(w.pods.iter().all(|p| p.id != opened.abandoned[0].id));
}

#[test]
fn an_id_in_the_error_that_is_not_this_dispatchs_pod_is_never_terminated() {
    let r = rig(2, "");
    let other = r
        .account
        .world()
        .add(&pod_name(2, "else", "eeeeeeeeeeee"), 5);
    let found = r
        .provider
        .pod_of_failed_create(&format!("timed out on pod {other}"), "ffffffffffff")
        .unwrap();
    assert!(found.is_none(), "another dispatch's pod is not ours");
    assert!(r.account.world().pods.iter().any(|p| p.id == other));
    let found = r
        .provider
        .pod_of_failed_create(&format!("timed out on pod {other}"), "eeeeeeeeeeee")
        .unwrap();
    assert_eq!(found.map(|p| p.id), Some(other), "its own nonce proves it");
}

#[test]
fn when_no_type_answers_every_pod_is_terminated_and_the_failure_counts_them() {
    let r = rig(1, "");
    r.account.world().half_made_with_id = vec!["NVIDIA A100 80GB PCIe".into()];
    r.account.world().half_made = vec!["NVIDIA H100 PCIe".into()];
    let said = open_one(&r, "t1").unwrap_err().to_string();
    assert!(said.contains("no pod could be rented"), "{said}");
    assert!(
        said.contains("2 pod(s) were made earlier in this dispatch")
            && said.contains("did not become reachable")
            && said.contains("approximately $")
            || said.contains("price unknown"),
        "{said}"
    );
    assert!(r.account.world().pods.is_empty(), "nothing is left billing");
    assert_eq!(r.account.world().calls_to(&["pod", "create"]).len(), 2);
}

#[test]
fn an_abandoned_pod_that_cannot_be_terminated_says_it_still_bills() {
    let r = rig(1, "");
    r.account.world().half_made_with_id = vec!["NVIDIA A100 80GB PCIe".into()];
    r.account.world().delete_fails = true;
    let said = open_one(&r, "t1").unwrap_err().to_string();
    assert!(
        said.contains("did not become reachable")
            && said.contains("could not be terminated")
            && said.contains("bills until")
            && said.contains("runpodctl pod delete rp"),
        "{said}"
    );
}

#[test]
fn the_approximate_charge_is_the_hourly_price_times_the_wait() {
    assert_eq!(charge_micros(1.19, Duration::from_mins(2)), 39_667);
    assert_eq!(charge_micros(2.39, Duration::from_hours(1)), 2_390_000);
    assert_eq!(charge_micros(1.19, Duration::ZERO), 0);
    assert_eq!(charge_micros(f64::NAN, Duration::from_mins(2)), 0);
    assert_eq!(dollars(39_667), "0.04");
    assert_eq!(dollars(123_456), "0.12");
    assert_eq!(dollars(4_000), "0.00");
    assert_eq!(dollars(2_390_000), "2.39");
    let line = abandoned_line(
        &Abandoned {
            id: "rp000001".into(),
            gpu: "G".into(),
            waited_secs: 125,
            cost_micros: Some(41_319),
        },
        120,
        "timed out",
    );
    assert!(
        line.contains("rp000001")
            && line.contains("(G)")
            && line.contains("within 120s")
            && line.contains("terminated after 125s")
            && line.contains("approximate charge $0.04"),
        "{line}"
    );
}
