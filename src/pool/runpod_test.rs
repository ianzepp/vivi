use super::*;
use crate::pool::collect::{self, Pacing, STALE_CLAIM_SECS};
use crate::pool::dispatch::{DispatchFlags, Launch, dispatch, plan_job};
use crate::pool::github::Secret;
use crate::pool::job::JobPlan;
use crate::pool::progress::Progress;
use crate::pool::provider::{CreateSpec, SlotLifetime};
use crate::pool::runpod_fixture::{NOW, Rig, pod_name, recording, request, rig, spec};
use crate::pool::slot::{self, PoolId};

fn pacing() -> Pacing {
    Pacing {
        poll: Duration::ZERO,
        ready_timeout: Duration::from_millis(200),
        stop_timeout: Duration::from_millis(200),
        start_grace: Duration::from_secs(collect::START_GRACE_SECS),
    }
}

fn open(r: &Rig, task: &str) -> crate::pool::provider::OpenedSlot {
    let (progress, _) = recording();
    r.provider
        .open_for_dispatch(&request(&r.id, task, &progress))
        .unwrap()
}

fn plan(r: &Rig) -> JobPlan {
    let flags = DispatchFlags {
        model: "m/ok".into(),
        need: None,
        kind: None,
        from: Some("main".into()),
        branch: Some("factory/a".into()),
        publish_ref: None,
        publish_expect: None,
        publish_contains: None,
        delete_ref: None,
        delete_expect: None,
        with: Vec::new(),
        budget: Some(600),
        allow_default_branch: false,
        read_only: false,
        reasoning: None,
        command: None,
    };
    plan_job(
        &r.config,
        "gpu",
        (
            "abcd1234",
            "Subject",
            &crate::letter::fixture::task_body("body"),
        ),
        &flags,
        "rules".into(),
        "job000000001".into(),
    )
    .unwrap()
}

#[test]
fn this_host_lives_per_dispatch_and_has_no_resting_slots() {
    let r = rig(2, "");
    assert_eq!(r.provider.slot_lifetime(), SlotLifetime::PerDispatch);
    let said = r
        .provider
        .create(&CreateSpec { ..spec(&r.id, 1) })
        .unwrap_err()
        .to_string();
    assert!(
        said.contains("no resting slots") && said.contains("vivi pool dispatch"),
        "{said}"
    );
    assert!(
        r.account.world().calls.is_empty(),
        "nothing was asked of the provider"
    );
    assert!(r.provider.billing_note().contains("billed by the second"));
    // Two GPU types, 120 s each to answer, then the seed limit, and a minute.
    assert_eq!(
        r.provider.setup_grace_secs(),
        2 * 120 + r.provider.pace.seed_limit.as_secs() + 60
    );
    assert_eq!(Pace::default().seed_limit, SEED_LIMIT);
    assert!(
        SEED_LIMIT.as_secs() + 60 > STALE_CLAIM_SECS,
        "a pod being set up is never taken for a leak"
    );
    assert!(
        !r.provider.runs_unprivileged()
            && r.provider.build_persists()
            && !r.provider.supplies_build_mount()
    );
}

#[test]
fn the_live_test_is_gated_on_spend_with_the_price_quoted() {
    let r = rig(1, "");
    let gate = r
        .provider
        .live_test_spend_gate()
        .expect("the live test spends");
    assert!(
        gate.contains("--allow-spend") && gate.contains("Nothing was rented"),
        "{gate}"
    );
    assert!(
        gate.contains("$1.19/h"),
        "the price of the first type is quoted: {gate}"
    );
    assert_eq!(r.account.world().spending_calls(), 0);
    assert!(r.provider.live_test_notice().contains("billable"));
}

#[test]
fn the_terminate_command_names_the_key_file_and_never_a_key() {
    let r = rig(1, "");
    let cmd = r.provider.destroy_command("rp000001");
    assert_eq!(
        cmd,
        "RUNPOD_API_KEY=$(cat /keys/runpod.key) runpodctl pod delete rp000001"
    );
}

#[test]
fn a_finished_job_is_collected_then_the_pod_is_terminated_and_confirmed_gone() {
    let r = rig(1, "");
    let opened = open(&r, "t1");
    // The job ended and left a result on the pod.
    r.pod
        .state()
        .files
        .insert("result.json".into(), b"{\"ok\":true}".to_vec());
    let machine = r.provider.machine(&opened.id).unwrap();
    assert_eq!(
        (machine.state.as_str(), machine.exit_code),
        ("stopped", Some(0))
    );
    let files = r
        .provider
        .read_stopped(&opened.id, &["/work/result.json"])
        .unwrap()
        .unwrap();
    assert_eq!(files["/work/result.json"], b"{\"ok\":true}");
    let pin = crate::pool::runpod_pin::pin_path(&r.provider.pins_dir(), &opened.id);
    assert!(pin.is_file());
    r.provider.destroy(&opened.id).unwrap();
    let w = r.account.world();
    assert!(w.pods.is_empty(), "absent from the listing");
    let last = w.calls.last().unwrap();
    assert_eq!(
        last,
        &["pod", "list", "--all"],
        "the last word is a listing that does not show it"
    );
    assert_eq!(w.calls_to(&["pod", "delete"]).len(), 1);
    assert!(!pin.exists(), "the pin goes with the pod");
}

#[test]
fn termination_waits_for_a_lagging_listing_to_drop_the_pod() {
    let r = rig(1, "");
    let opened = open(&r, "t1");
    r.account.world().delete_lag = 3;
    r.provider.destroy(&opened.id).unwrap();
    let lists = r.account.world().calls_to(&["pod", "list"]).len();
    assert!(
        lists >= 4,
        "it kept looking until the pod was gone ({lists} listings)"
    );
}

#[test]
fn a_pod_listed_as_terminated_counts_as_gone() {
    let r = rig(1, "");
    let opened = open(&r, "t1");
    {
        let mut w = r.account.world();
        w.delete_lag = 50;
        w.show_terminated = true;
    }
    r.provider.destroy(&opened.id).unwrap();
}

#[test]
fn a_pod_still_listed_after_the_wait_is_reported_as_possibly_billing() {
    let r = rig(1, "");
    let opened = open(&r, "t1");
    r.account.world().delete_lag = u32::MAX;
    let said = r.provider.destroy(&opened.id).unwrap_err().to_string();
    assert!(
        said.contains("still listed") && said.contains("may still be billing"),
        "{said}"
    );
    assert!(said.contains("runpodctl pod delete"), "{said}");
}

#[test]
fn a_refused_termination_is_an_error_and_the_pod_stays_known() {
    let r = rig(1, "");
    let opened = open(&r, "t1");
    r.account.world().delete_fails = true;
    let said = r.provider.destroy(&opened.id).unwrap_err().to_string();
    assert!(
        said.contains("pod delete") && said.contains("could not terminate"),
        "{said}"
    );
    assert_eq!(r.account.world().pods.len(), 1);
    assert!(
        crate::pool::runpod_pin::pin_path(&r.provider.pins_dir(), &opened.id).is_file(),
        "the pin is kept while the pod lives"
    );
}

#[test]
fn terminating_a_pod_that_is_already_gone_succeeds() {
    let r = rig(1, "");
    let id = r.account.world().add(&pod_name(1, "t", "aaaaaaaaaaaa"), 1);
    r.account.world().pods.clear();
    r.provider.destroy(&id).unwrap();
}

#[test]
fn an_id_that_is_not_a_pod_id_never_reaches_the_cli() {
    let r = rig(1, "");
    for bad in ["../x", "a b", "rp1;rm", "", "ms/gpu/slot-1"] {
        assert!(r.provider.destroy(bad).is_err(), "{bad:?}");
    }
    assert!(r.account.world().calls.is_empty());
}

#[test]
fn the_listing_shows_only_pods_in_vivis_layout_with_their_marks() {
    let r = rig(2, "");
    {
        let mut w = r.account.world();
        w.add(&pod_name(2, "t9", "aaaaaaaaaaaa"), 1);
        w.add("not-ours", 2);
    }
    let all = r.provider.list().unwrap();
    assert_eq!(all.len(), 1);
    let rec = &all[0];
    assert_eq!(rec.name, "ms-gpu-slot-2");
    assert_eq!(rec.state, "started");
    assert_eq!(rec.metadata[slot::META_TASK], "t9");
    assert_eq!(rec.metadata[slot::META_CLAIMED], NOW.to_string());
    assert_eq!(rec.metadata[slot::META_POOL], "gpu");
}

#[test]
fn a_pod_the_provider_no_longer_lists_reads_as_gone() {
    let r = rig(1, "");
    assert_eq!(r.provider.machine("rp999999").unwrap().state, "destroyed");
}

#[test]
fn a_pod_that_is_not_running_keeps_no_files() {
    let r = rig(1, "");
    let opened = open(&r, "t1");
    r.account.world().delete_lag = 5;
    r.account.world().show_terminated = true;
    let _ = r.provider.destroy(&opened.id);
    let files = r
        .provider
        .read_stopped(&opened.id, &["/work/result.json"])
        .unwrap()
        .unwrap();
    assert!(
        files.is_empty(),
        "a stopped pod has lost its disk: the engine reads that as a lost job"
    );
}

#[test]
fn a_pod_not_of_vivis_is_not_acted_on() {
    let r = rig(1, "");
    let id = r.account.world().add("someone-elses-gpu", 1);
    let said = r.provider.machine(&id).unwrap_err().to_string();
    assert!(said.contains("not a pod of Vivi's"), "{said}");
    assert!(r.provider.exec(&id, "true", 5).is_err());
    assert!(r.account.world().calls_to(&["pod", "delete"]).is_empty());
}

#[test]
fn a_pod_with_no_pin_on_this_machine_cannot_be_reached() {
    let r = rig(1, "");
    let id = r.account.world().add(&pod_name(1, "t", "aaaaaaaaaaaa"), 1);
    let said = r.provider.exec(&id, "true", 5).unwrap_err().to_string();
    assert!(
        said.contains("no pinned SSH host key") && said.contains("runpodctl pod delete"),
        "{said}"
    );
    assert!(r.pod.state().verbs.is_empty());
}

#[test]
fn starting_rechecks_the_claim_and_a_loser_is_terminated() {
    let r = rig(3, "");
    let opened = open(&r, "t1");
    // A rival with the same index, made before ours, becomes visible now.
    let rival = pod_name(1, "rival", "dddddddddddd");
    r.account.world().add(&rival, 1);
    let said = r.provider.start(&opened.id).unwrap_err().to_string();
    assert!(said.contains("lost its slot"), "{said}");
    let w = r.account.world();
    assert_eq!(w.pods.len(), 1);
    assert!(w.pods[0].name.contains("dddddddddddd"));
}

#[test]
fn starting_a_pod_that_won_goes_through() {
    let r = rig(3, "");
    let opened = open(&r, "t1");
    r.provider.start(&opened.id).unwrap();
    assert_eq!(
        r.pod.state().verbs.last().map(String::as_str),
        Some("start")
    );
}

#[test]
fn dispatch_collect_terminate_confirm_absent_against_the_engine() {
    let r = rig(1, "");
    let plan = plan(&r);
    let token = Secret::new("tok".into());
    let progress = Progress::silent();
    let launch = Launch {
        config: &r.config,
        plan: &plan,
        token: &token,
        start: &"c".repeat(40),
        progress: &progress,
        ready_timeout: None,
        reset_base: None,
    };
    let record = dispatch(&r.provider, &r.id, &launch, NOW, &pacing()).unwrap();
    assert_eq!(record.pool, "gpu");
    assert!(
        record.hardware.starts_with("NVIDIA A100 80GB PCIe x1"),
        "{}",
        record.hardware
    );
    assert_eq!(
        r.account.world().live().len(),
        1,
        "the pod exists while the job runs"
    );
    {
        let pod = r.pod.state();
        assert!(
            pod.files
                .contains_key(crate::pool::job::JOB_ARCHIVE.trim_start_matches("/work/")),
            "{:?}",
            pod.files.keys()
        );
        assert_eq!(
            pod.verbs.last().map(String::as_str),
            Some("start"),
            "the job was started last"
        );
    }
    // Collection ends the pod and proves it is gone.
    collect::release(&r.provider, &record.machine, &plan.task, &pacing()).unwrap();
    assert!(r.account.world().pods.is_empty());
    assert_eq!(
        r.account.world().calls.last().unwrap(),
        &["pod", "list", "--all"]
    );
}

#[test]
fn a_dispatch_records_the_pods_it_gave_up_on_before_the_one_that_works() {
    let r = rig(1, "");
    r.account.world().half_made_with_id = vec!["NVIDIA A100 80GB PCIe".into()];
    let plan = plan(&r);
    let token = Secret::new("tok".into());
    let progress = Progress::silent();
    let launch = Launch {
        config: &r.config,
        plan: &plan,
        token: &token,
        start: &"c".repeat(40),
        progress: &progress,
        ready_timeout: Some(30),
        reset_base: None,
    };
    let record = dispatch(&r.provider, &r.id, &launch, NOW, &pacing()).unwrap();
    assert!(
        record.hardware.starts_with("NVIDIA H100 PCIe"),
        "{}",
        record.hardware
    );
    assert_eq!(record.abandoned, 1);
    let line = crate::pool::porcelain::dispatched("abcd1234", &record);
    let fields: Vec<&str> = line.split(' ').collect();
    assert_eq!(fields[fields.len() - 2], "1", "{line}");
    let text = crate::pool::record::dispatch_text(&record);
    assert!(text.contains("abandoned 1 pod(s)"), "{text}");
    let wait = r
        .account
        .world()
        .calls_to(&["pod", "create"])
        .iter()
        .map(|c| c[c.iter().position(|a| a == "--wait-timeout").unwrap() + 1].clone())
        .collect::<Vec<_>>();
    assert_eq!(wait, ["30s", "30s"], "the override reached every create");
}

#[test]
fn a_dispatch_that_loses_the_last_claim_check_leaves_no_pod_of_its_own() {
    let r = rig(3, "");
    let plan = plan(&r);
    let token = Secret::new("tok".into());
    let progress = Progress::silent();
    let launch = Launch {
        config: &r.config,
        plan: &plan,
        token: &token,
        start: &"c".repeat(40),
        progress: &progress,
        ready_timeout: None,
        reset_base: None,
    };
    // A rival with our index, made before ours, is not in the listing when the
    // dispatch first checks, and is by the time the job is about to start.
    let rival = pod_name(1, "rival", "dddddddddddd");
    r.account.world().after_create = Some(Box::new(move |w| {
        let earlier = w.clock() - 5000;
        let id = w.add(&rival, earlier);
        w.pods.iter_mut().find(|p| p.id == id).unwrap().hidden = 1;
    }));
    let said = dispatch(&r.provider, &r.id, &launch, NOW, &pacing())
        .unwrap_err()
        .to_string();
    assert!(said.contains("lost its slot"), "{said}");
    let w = r.account.world();
    assert_eq!(
        w.pods.len(),
        1,
        "ours was terminated, the rival's was not touched"
    );
    assert!(w.pods[0].name.contains("dddddddddddd"));
}

#[test]
fn a_pod_with_no_dispatch_behind_it_is_a_leak_that_reconcile_clears_after_the_grace() {
    let r = rig(2, "");
    let opened = open(&r, "t1");
    let records = r.provider.list().unwrap();
    let id: &PoolId = &r.id;
    let slots = slot::pool_slots(id, &records);
    let grace = r.provider.setup_grace_secs();
    let soon = collect::stale_claims(&slots, &[], NOW + grace - 1, grace);
    assert!(
        soon.is_empty(),
        "a pod still within its setup time is not a leak yet"
    );
    let later = collect::stale_claims(&slots, &[], NOW + grace + 1, grace);
    assert_eq!(later.len(), 1);
    assert_eq!(later[0].id, opened.id);
    // The engine's reconcile path frees it by terminating it.
    collect::release(
        &r.provider,
        &later[0].id,
        later[0].task.as_deref().unwrap(),
        &pacing(),
    )
    .unwrap();
    assert!(r.account.world().pods.is_empty());
}

#[test]
fn the_pool_view_of_a_pod_carries_its_state_for_the_engine() {
    let r = rig(1, "");
    let opened = open(&r, "t1");
    let rec = r.provider.machine(&opened.id).unwrap();
    assert_eq!(
        rec.state, "stopped",
        "the seed's boot ended, as the pod itself says"
    );
    assert_eq!(rec.metadata[slot::META_TASK], "t1");
}

#[test]
fn iso_times_are_utc_and_round_trip() {
    assert_eq!(iso(0), "1970-01-01T00:00:00Z");
    assert_eq!(iso(NOW), "2026-09-21T14:13:20Z");
    assert_eq!(
        iso(u64::MAX),
        "",
        "an unrepresentable time is empty, not a panic"
    );
}

#[test]
fn error_text_from_the_cli_is_scrubbed_of_key_shapes() {
    let out = RunOutput {
        code: 1,
        stdout: Vec::new(),
        stderr: r#"{"error":"bad key RUNPOD_API_KEY=rpa_SECRET123 rejected","code":"401"}"#.into(),
    };
    let said = failure("user", &out).to_string();
    assert!(!said.contains("rpa_SECRET123"), "{said}");
    assert!(said.contains("runpodctl user"));
}
