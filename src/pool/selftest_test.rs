use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Write;
use std::process::{Command, Stdio};
use std::rc::Rc;

use super::*;
use crate::pool::doctor::{Report, Status};
use crate::pool::fake_provider::{Fake, slot};
use crate::pool::progress::Progress;
use crate::pool::slot::META_TASK;

fn real_config() -> PoolConfig {
    toml::from_str(
        r#"
repo = "acme/widgets"
slots = 4
image = "debian:12"
build_dir = "target"
setup = ["apt-get install -y make"]
seed = ["make warm"]
[fly]
app = "a"
size = "performance-8x"
[github]
app_id = 1
private_key = "k.pem"
"#,
    )
    .unwrap()
}

/// One file out of a job archive delivered to a machine.
fn archive_file(archive: &[u8], name: &str) -> String {
    let mut child = Command::new("tar")
        .args(["-xzOf", "-", name])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(archive).unwrap();
    String::from_utf8(child.wait_with_output().unwrap().stdout).unwrap()
}

fn param(params: &str, key: &str) -> String {
    params
        .lines()
        .find_map(|l| l.strip_prefix(&format!("{key}='")))
        .and_then(|v| v.strip_suffix('\''))
        .unwrap_or_default()
        .to_string()
}

/// What a slot would write for the latest job delivered to it. Each machine
/// has its own remote: a branch exists only on the machine that pushed it.
struct Sim {
    branches: RefCell<HashMap<(String, String), String>>,
    /// Make the first job's push come back refused.
    refuse_push: bool,
}

impl Sim {
    fn output(&self, fake: &Fake, id: &str) -> Option<String> {
        let files = fake.files.borrow();
        let archive = files
            .iter()
            .rev()
            .find(|(m, p, _)| m == id && p == "/work/job.tgz")
            .map(|f| f.2.clone())?;
        drop(files);
        let params = archive_file(&archive, "job/params.env");
        let harness = archive_file(&archive, "job/harness.cmd");
        let (job, branch, from) = (
            param(&params, "JOB"),
            param(&params, "BRANCH"),
            param(&params, "FROM"),
        );
        let budget: u64 = param(&params, "BUDGET").parse().unwrap();
        let nap: u64 = harness
            .strip_prefix("sleep ")
            .and_then(|r| r.split(';').next())
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        let key = |b: &str| (id.to_string(), b.to_string());
        let start = if from == "main" {
            Some("0000000000main".to_string())
        } else {
            self.branches.borrow().get(&key(&from)).cloned()
        };
        let (push, sha, rc, secs) = match start {
            None => ("fetch_failed", String::new(), String::new(), String::new()),
            Some(_) if nap > budget => (
                "no_changes",
                String::new(),
                "124".into(),
                budget.to_string(),
            ),
            Some(_) if self.refuse_push => ("push_refused", String::new(), "0".into(), "0".into()),
            Some(_) => {
                let sha = format!("sha-of-{job}");
                self.branches.borrow_mut().insert(key(&branch), sha.clone());
                ("pushed", sha, "0".into(), nap.to_string())
            }
        };
        let start = start.unwrap_or_default();
        Some(format!(
            "@@vivi-pool result\njob={job}\nstate=done\npush={push}\nbranch={branch}\nsha={sha}\nstart={start}\nagent_rc={rc}\nagent_s={secs}\njob_s=1\nmodel=standin\nslot={id}\n@@vivi-pool receipt\nstand-in receipt\n@@vivi-pool stderr\n\n@@vivi-pool log\n\n"
        ))
    }
}

/// A fake host whose slots behave like the real boot script: a delivered job
/// produces a result, and a machine with a result stops itself after a while.
fn world_fake(refuse_push: bool) -> Fake {
    let fake = Fake::default();
    let sim = Rc::new(Sim {
        branches: RefCell::new(HashMap::new()),
        refuse_push,
    });
    let seen = Rc::new(RefCell::new(HashMap::<String, u32>::new()));
    *fake.exec_hook.borrow_mut() = Some(Box::new(move |f, id| sim.output(f, id)));
    *fake.on_machine_read.borrow_mut() = Some(Box::new(move |f, id| {
        let held = f.meta(id, META_TASK).is_some();
        if f.state_of(id) != "started" || !held {
            return;
        }
        let mut reads = seen.borrow_mut();
        let count = reads.entry(id.to_string()).or_default();
        *count += 1;
        if *count >= 3 {
            *count = 0;
            f.set_state(id, "stopped");
        }
    }));
    fake
}

fn run_with(fake: &Fake, long: bool, cancelled: &dyn Fn() -> bool) -> Vec<Check> {
    let options = TestOptions {
        long,
        timing: Timing::instant(),
        cancelled,
        progress: &|_| {},
        starting: &|_| {},
        detail: &Progress::silent(),
    };
    run_test(fake, &real_config(), "p", &options)
}

fn by_name<'a>(checks: &'a [Check], name: &str) -> &'a Check {
    checks
        .iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("no check {name}: {checks:#?}"))
}

fn failures(checks: &[Check]) -> Vec<String> {
    checks
        .iter()
        .filter(|c| c.status == Status::Fail)
        .map(|c| format!("{}: {}", c.name, c.detail))
        .collect()
}

fn test_machines(fake: &Fake) -> usize {
    fake.machines
        .borrow()
        .iter()
        .filter(|m| m.metadata.get("vivi_pool").is_some_and(|p| p == "p-doctor"))
        .count()
}

#[test]
fn a_healthy_provider_passes_every_step_and_leaves_nothing() {
    let fake = world_fake(false);
    fake.add(slot("real-1", "p-slot-1", "stopped", "p", 1));
    let checks = run_with(&fake, false, &|| false);
    assert_eq!(failures(&checks), Vec::<String>::new(), "{checks:#?}");
    let report = Report {
        checks: checks.clone(),
    };
    assert!(report.passed());
    assert_eq!(report.counts().ok, plan(false).len());
    assert_eq!(test_machines(&fake), 0, "the test pool is gone");
    assert_eq!(fake.destroyed.borrow().len(), 2);
    assert_eq!(
        fake.state_of("real-1"),
        "stopped",
        "the real pool is untouched"
    );
    assert!(fake.calls.borrow().iter().all(|c| !c.ends_with("real-1")));
    assert!(
        checks.iter().all(|c| c.millis.is_some()),
        "every step is timed"
    );
}

#[test]
fn steps_are_grouped_by_side_and_run_in_order() {
    let fake = world_fake(false);
    let checks = run_with(&fake, false, &|| false);
    let names: Vec<(&str, &str)> = checks
        .iter()
        .map(|c| (c.group.as_str(), c.name.as_str()))
        .collect();
    assert_eq!(
        names,
        [
            (PROVIDER_GROUP, "create two slots"),
            (PROVIDER_GROUP, "both slots reach free"),
            (PROVIDER_GROUP, "start and stop round trip"),
            (VIVI_GROUP, "dispatch two at once"),
            (VIVI_GROUP, "third dispatch is refused"),
            (VIVI_GROUP, "wait collects a finished job"),
            (VIVI_GROUP, "a job past its budget is killed"),
            (VIVI_GROUP, "continue a branch"),
            (VIVI_GROUP, "unwatched job is collected later"),
            (VIVI_GROUP, "reconcile stops a slot with no dispatch"),
            (PROVIDER_GROUP, "destroy the test slots"),
            (PROVIDER_GROUP, "nothing of the test pool remains"),
        ]
    );
}

#[test]
fn long_adds_one_vivi_step_before_the_cleanup() {
    let fake = world_fake(false);
    let checks = run_with(&fake, true, &|| false);
    assert_eq!(failures(&checks), Vec::<String>::new());
    let at = checks
        .iter()
        .position(|c| c.name.starts_with("wait past six"))
        .unwrap();
    assert_eq!(checks[at].group, VIVI_GROUP);
    assert_eq!(checks[at + 1].name, "destroy the test slots");
    assert_eq!(plan(true).len(), plan(false).len() + 1);
}

#[test]
fn the_test_pool_is_isolated_from_the_project() {
    let fake = world_fake(false);
    run_with(&fake, false, &|| false);
    let specs = fake.created.borrow();
    assert_eq!(specs.len(), 2);
    for spec in specs.iter() {
        assert!(spec.name.starts_with("p-doctor-slot-"), "{}", spec.name);
        assert_eq!(spec.metadata["vivi_pool"], "p-doctor");
        assert_eq!(spec.env["REMOTE"], "file:///work/remote.git");
        assert!(
            !spec.env.contains_key("GH_TOKEN"),
            "no token is minted or sent"
        );
        assert!(
            !spec
                .env
                .values()
                .any(|v| v.contains("github") || v.contains("acme"))
        );
        assert!(
            spec.files
                .iter()
                .any(|(p, _)| p == "/work/source/README.md")
        );
        let seed = &spec
            .files
            .iter()
            .find(|(p, _)| p == "/work/seed.sh")
            .unwrap()
            .1;
        assert!(seed.ends_with("true\n") && !seed.contains("make"));
        let setup = &spec
            .files
            .iter()
            .find(|(p, _)| p == "/work/setup.sh")
            .unwrap()
            .1;
        assert_eq!(setup, "#!/bin/bash\nset -e\n");
        assert_eq!(spec.image, "debian:12", "the real image is reused");
    }
}

#[test]
fn a_provider_failure_names_the_provider_side_skips_the_rest_and_still_cleans_up() {
    let fake = world_fake(false);
    fake.fail_start.set(true);
    let checks = run_with(&fake, false, &|| false);
    let bad = by_name(&checks, "start and stop round trip");
    assert_eq!(
        (bad.status, bad.group.as_str()),
        (Status::Fail, PROVIDER_GROUP)
    );
    assert!(
        bad.detail.starts_with("expected start accepted; observed"),
        "{}",
        bad.detail
    );
    assert_eq!(
        by_name(&checks, "dispatch two at once").status,
        Status::Skipped
    );
    assert!(
        by_name(&checks, "continue a branch")
            .detail
            .contains("start and stop round trip")
    );
    assert_eq!(
        by_name(&checks, "destroy the test slots").status,
        Status::Ok
    );
    assert_eq!(test_machines(&fake), 0);
    assert!(!Report { checks }.passed());
}

#[test]
fn a_vivi_failure_names_the_vivi_side_with_expected_and_observed() {
    let fake = world_fake(true);
    let checks = run_with(&fake, false, &|| false);
    let bad = by_name(&checks, "wait collects a finished job");
    assert_eq!((bad.status, bad.group.as_str()), (Status::Fail, VIVI_GROUP));
    assert!(
        bad.detail.contains("expected a finished job that pushed"),
        "{}",
        bad.detail
    );
    assert!(bad.detail.contains("push=push_refused"), "{}", bad.detail);
    assert!(
        bad.fix
            .as_deref()
            .unwrap()
            .contains("Vivi's pool machinery")
    );
    assert_eq!(
        by_name(&checks, "destroy the test slots").status,
        Status::Ok
    );
    assert_eq!(test_machines(&fake), 0);
}

#[test]
fn an_interrupt_between_steps_stops_before_the_next_one_and_the_cleanup_still_runs() {
    let fake = world_fake(false);
    let checks = run_with(&fake, false, &|| fake.calls_to("start") > 0);
    let bad = by_name(&checks, "dispatch two at once");
    assert_eq!(bad.status, Status::Fail);
    assert!(
        bad.detail.contains("interrupted before this step ran"),
        "{}",
        bad.detail
    );
    assert_eq!(
        by_name(&checks, "start and stop round trip").status,
        Status::Ok
    );
    assert_eq!(
        by_name(&checks, "continue a branch").status,
        Status::Skipped
    );
    assert_eq!(
        by_name(&checks, "destroy the test slots").status,
        Status::Ok
    );
    assert_eq!(test_machines(&fake), 0);
}

#[test]
fn an_interrupt_while_waiting_stops_the_wait_and_the_cleanup_still_runs() {
    let fake = world_fake(false);
    *fake.exec_hook.borrow_mut() = Some(Box::new(|_, _| None));
    let checks = run_with(&fake, false, &|| fake.calls_to("exec") > 0);
    let bad = by_name(&checks, "wait collects a finished job");
    assert_eq!(bad.status, Status::Fail);
    assert!(bad.detail.contains("interrupted"), "{}", bad.detail);
    assert_eq!(
        by_name(&checks, "destroy the test slots").status,
        Status::Ok
    );
    assert_eq!(
        by_name(&checks, "nothing of the test pool remains").status,
        Status::Ok
    );
    assert_eq!(test_machines(&fake), 0);
}

#[test]
fn leftovers_are_named_with_the_command_that_removes_them() {
    let fake = world_fake(false);
    fake.destroy_fails.set(true);
    let checks = run_with(&fake, false, &|| false);
    let gone = by_name(&checks, "destroy the test slots");
    assert_eq!(gone.status, Status::Fail);
    let left = by_name(&checks, "nothing of the test pool remains");
    assert_eq!(left.status, Status::Fail);
    assert!(
        left.detail.contains("p-doctor-slot-1=free"),
        "{}",
        left.detail
    );
    assert!(
        left.detail.contains("fake destroy id-p-doctor-slot-1"),
        "{}",
        left.detail
    );
    assert!(
        left.detail.contains("fake destroy id-p-doctor-slot-2"),
        "{}",
        left.detail
    );
}

#[test]
fn slots_left_by_an_earlier_run_are_removed_first_and_reported() {
    let fake = world_fake(false);
    fake.add(slot("old-1", "p-doctor-slot-1", "stopped", "p-doctor", 1));
    let checks = run_with(&fake, false, &|| false);
    let earlier = by_name(&checks, "earlier test slots");
    assert_eq!(earlier.status, Status::Warn);
    assert!(earlier.detail.contains("p-doctor-slot-1"));
    assert!(fake.destroyed.borrow().contains(&"old-1".to_string()));
    assert_eq!(failures(&checks), Vec::<String>::new());
}

#[test]
fn seeding_that_never_finishes_fails_the_create_step_and_removes_the_machines() {
    let mut fake = world_fake(false);
    fake.never_stop = true;
    let checks = run_with(&fake, false, &|| false);
    let bad = by_name(&checks, "create two slots");
    assert_eq!(bad.status, Status::Fail);
    assert!(
        bad.detail.contains("expected two slots seeded cleanly"),
        "{}",
        bad.detail
    );
    assert_eq!(test_machines(&fake), 0);
}

#[test]
fn the_report_renders_durations_and_json_keeps_them() {
    let fake = world_fake(false);
    let checks = run_with(&fake, false, &|| false);
    let report = Report { checks };
    assert!(report.render().contains("create two slots"));
    assert!(report.render().contains("s)"));
    let json = report.to_json().unwrap();
    assert!(json.contains("\"millis\""));
    assert!(json.contains(PROVIDER_GROUP) && json.contains(VIVI_GROUP));
}

#[test]
fn every_step_announces_itself_before_it_runs_and_a_quiet_detail_says_nothing() {
    let fake = world_fake(false);
    let started = RefCell::new(Vec::<String>::new());
    let finished = RefCell::new(Vec::<String>::new());
    let heard = Rc::new(RefCell::new(Vec::<String>::new()));
    let sink = Rc::clone(&heard);
    let detail = Progress::to(true, move |l| sink.borrow_mut().push(l.to_string()));
    let options = TestOptions {
        long: false,
        timing: Timing::instant(),
        cancelled: &|| false,
        progress: &|c| finished.borrow_mut().push(c.name.clone()),
        starting: &|name| started.borrow_mut().push(name.to_string()),
        detail: &detail,
    };
    run_test(&fake, &real_config(), "p", &options);
    assert_eq!(
        started.borrow().first().map(String::as_str),
        Some("create two slots")
    );
    assert_eq!(
        *started.borrow(),
        *finished.borrow(),
        "one start per finish, in order"
    );
    assert!(heard.borrow().is_empty(), "a quiet detail drops its lines");
}

#[test]
fn the_steps_of_a_wait_say_when_they_start_and_beat_while_they_wait() {
    let fake = world_fake(false);
    let heard = Rc::new(RefCell::new(Vec::<String>::new()));
    let sink = Rc::clone(&heard);
    let detail = Progress::to(false, move |l| sink.borrow_mut().push(l.to_string()));
    let mut timing = Timing::instant();
    timing.heartbeat = std::time::Duration::ZERO;
    let options = TestOptions {
        long: false,
        timing,
        cancelled: &|| false,
        progress: &|_| {},
        starting: &|_| {},
        detail: &detail,
    };
    run_test(&fake, &real_config(), "p", &options);
    let lines = heard.borrow();
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("creating p-doctor-slot-1")),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("waiting for 1 dispatch(es):")),
        "{lines:?}"
    );
}
