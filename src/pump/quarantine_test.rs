//! Tests for quarantine: a batch whose shared check failed is pushed aside,
//! one repair unit works on it, and the quarantined units follow the repair's
//! fate.

use std::collections::BTreeMap;
use std::fs;
use std::sync::Arc;
use std::time::Duration;

use tempfile::{TempDir, tempdir};

use crate::pump::collect::fail;
use crate::pump::config::{DispatchConfig, Kind, Lane, MergeConfig, PumpConfig};
use crate::pump::dispatch::dispatch;
use crate::pump::host::{Ctx, FakeHost, test_ctx};
use crate::pump::merge::{Merger, batch_work};
use crate::pump::merge_lane_test::state_with;
use crate::pump::quarantine::{branch, repair_no_defect, repair_of, repair_uid, set_aside};
use crate::pump::ready::ranked;
use crate::pump::state::{MergeJob, MergeResult, State, Status, Unit};
use crate::pump::store::{DB_FILENAME, PumpStore};

/// The merge test context (`merge_test::setup`) with a dispatch config:
/// `kinds` names the kinds, and the default kind is `plain` when it has one.
fn setup_kinds(kinds: &[&str]) -> (TempDir, Ctx, Arc<FakeHost>) {
    let dir = tempdir().unwrap();
    let cfg = PumpConfig {
        branch: String::from("next"),
        committer_name: String::from("Pump Tester"),
        committer_email: String::from("pump@example.com"),
        merge: MergeConfig {
            shared_checks: vec![String::from("cargo test --lib")],
            ..MergeConfig::default()
        },
        dispatch: DispatchConfig {
            kinds: kinds
                .iter()
                .map(|name| (String::from(*name), Kind::default()))
                .collect(),
            default_kind: if kinds.is_empty() {
                String::new()
            } else {
                String::from("plain")
            },
            ..DispatchConfig::default()
        },
        ..PumpConfig::default()
    };
    let (ctx, host) = test_ctx(dir.path(), cfg);
    host.reply("merge-base --is-ancestor origin/work/", 1, "");
    (dir, ctx, host)
}

/// The merge test context with the one dispatch kind `plain`, which the test
/// units (and so the repair unit) name.
fn setup(_format: &str) -> (TempDir, Ctx, Arc<FakeHost>) {
    setup_kinds(&["plain"])
}

/// A plain unit job for branch `work/<uid>`.
fn job(uid: &str) -> MergeJob {
    MergeJob {
        uid: uid.to_string(),
        target: String::from("default"),
        branch: format!("work/{uid}"),
        title: format!("Unit {uid}"),
        need: String::from("n1"),
        proof: vec![format!("./proof-{uid}.sh")],
        tools: String::from("plain"),
        write_scope: vec![format!("src/{uid}.rs")],
        source: None,
    }
}

/// How many recorded commands contain `fragment`.
fn count(cmds: &[String], fragment: &str) -> usize {
    cmds.iter().filter(|c| c.contains(fragment)).count()
}

/// The exceptions file text.
fn exceptions(ctx: &Ctx) -> String {
    fs::read_to_string(ctx.home.join("exceptions.txt")).unwrap_or_default()
}

/// Run a failing two-unit batch and return its two results.
fn quarantined_pair(ctx: &Ctx, host: &Arc<FakeHost>) -> Vec<MergeResult> {
    host.reply("cargo test --lib", 1, "error[E0308]: mismatched types\n");
    batch_work(ctx, &[job("u1"), job("u2")])
}

/// The state a lane's quarantined results leave behind: `u1` and `u2`
/// applied through a detached merger.
fn applied_state(ctx: &Ctx, results: Vec<MergeResult>) -> State {
    let (merger, out, _jobs) = Merger::detached();
    let mut st = state_with(&[("u1", Status::Merging), ("u2", Status::Merging)]);
    for result in results {
        out.send(result).unwrap();
    }
    merger.apply(ctx, &mut st);
    st
}

#[test]
fn a_two_unit_batch_failing_a_shared_check_is_pushed_aside_and_quarantined() {
    let (_dir, ctx, host) = setup("");
    let results = quarantined_pair(&ctx, &host);
    assert_eq!(results.len(), 2);
    let id = results[0].detail.clone().unwrap();
    assert!(id.starts_with("q1000000-"));
    for (result, uid) in results.iter().zip(["u1", "u2"]) {
        assert_eq!(result.uid, uid);
        assert_eq!(result.ok, None);
        assert_eq!(result.why.as_deref(), Some("quarantined"));
        let q = result.quarantine.as_ref().unwrap();
        assert_eq!(q.id, id);
        assert_eq!(q.cmd, "cargo test --lib");
        assert_eq!(q.code, 1);
        assert!(q.text.contains("error[E0308]: mismatched types"));
        assert_eq!(q.units, ["u1", "u2"]);
    }
    let cmds = host.commands();
    // The batch is bisected (2 solo merges) and then rebuilt before the
    // quarantine push: 2 batch merges plus 2 bisect merges plus 2 rebuilds.
    assert_eq!(count(&cmds, "merge --no-ff"), 6);
    assert_eq!(count(&cmds, "./proof-u1.sh"), 0);
    let push = format!("git push origin HEAD:refs/heads/{}", branch(&id));
    assert_eq!(count(&cmds, &push), 1);
    assert_eq!(count(&cmds, "refs/heads/next"), 0);
    assert_eq!(count(&cmds, "--force"), 0);
    assert_eq!(
        cmds.last().map(String::as_str),
        Some("git reset -q --hard origin/next")
    );
    let logs = host.logs();
    assert!(logs.contains(&format!("quarantine {id}: 2 units, cargo test --lib rc=1")));
    assert!(logs.iter().any(|l| l.contains("error[E0308]")));
}

#[test]
fn a_failed_shared_check_lands_clean_members_and_quarantines_only_the_culprit() {
    let (_dir, ctx, host) = setup("");
    // The batch check fails; then the first solo check (u1) fails while u2's
    // solo check has no one-shot left and falls through to the passing rule.
    host.reply("cargo test --lib", 0, "");
    host.reply_once("cargo test --lib", 1, "error: boom\n");
    host.reply_once("cargo test --lib", 1, "error: boom\n");
    let results = batch_work(&ctx, &[job("u1"), job("u2")]);
    assert_eq!(results.len(), 2);
    let u1 = results.iter().find(|r| r.uid == "u1").unwrap();
    let u2 = results.iter().find(|r| r.uid == "u2").unwrap();
    assert_eq!(u1.why.as_deref(), Some("quarantined"));
    let q = u1.quarantine.as_ref().unwrap();
    assert_eq!(q.units, ["u1"]);
    let id = q.id.clone();
    // The clean member published in the same cycle; only the culprit was
    // pushed aside.
    assert_eq!(u2.ok, Some(true), "{u2:?}");
    let cmds = host.commands();
    assert_eq!(count(&cmds, "push origin HEAD:refs/heads/next"), 1);
    assert_eq!(
        count(
            &cmds,
            &format!("push origin HEAD:refs/heads/{}", branch(&id))
        ),
        1
    );
    assert!(
        !cmds
            .iter()
            .any(|c| c.contains("vivi/quarantine") && c.contains("u2"))
    );
    let st = applied_state(&ctx, results);
    let repair = repair_uid(&ctx, &id);
    assert_eq!(
        st.units.keys().filter(|u| u.starts_with("pumpfix")).count(),
        1
    );
    assert!(st.units.contains_key(&repair));
    let brief = &st.units[&repair].brief;
    assert!(brief.contains("- u1 (need n1): Unit u1"), "{brief}");
    assert!(!brief.contains("- u2"));
    assert_eq!(st.units["u1"].status, Status::Quarantined);
    assert_eq!(st.units["u2"].status, Status::Merged);
    let logs = host.logs();
    assert!(
        logs.iter().any(|l| l.starts_with("bisect u1 rc=1")),
        "{logs:?}"
    );
    assert!(logs.iter().any(|l| l.contains("culprits")), "{logs:?}");
}

#[test]
fn a_conflicting_unit_keeps_its_failure_and_the_rest_are_quarantined() {
    let (_dir, ctx, host) = setup("");
    host.reply("merge u1", 1, "CONFLICT (content)\n");
    host.reply("diff --name-only --diff-filter=U", 0, "src/a.rs\n");
    host.reply("cargo test --lib", 1, "error: boom\n");
    let results = batch_work(&ctx, &[job("u1"), job("u2"), job("u3")]);
    assert_eq!(results.len(), 3);
    assert_eq!(results[0].why.as_deref(), Some("merge-conflict"));
    assert_eq!(results[0].ok, Some(false));
    for result in &results[1..] {
        assert_eq!(result.why.as_deref(), Some("quarantined"));
        let q = result.quarantine.as_ref().unwrap();
        assert_eq!(q.units, ["u2", "u3"]);
    }
}

#[test]
fn a_one_unit_batch_still_fails_the_unit_and_logs_the_failure() {
    let (_dir, ctx, host) = setup("");
    host.reply("cargo test --lib", 1, "error: boom\n");
    let results = batch_work(&ctx, &[job("u1")]);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].ok, Some(false));
    assert_eq!(results[0].why.as_deref(), Some("proof-failed"));
    assert!(results[0].quarantine.is_none());
    assert_eq!(count(&host.commands(), "vivi/quarantine"), 0);
    assert!(host.logs().iter().any(|l| l.contains("error: boom")));
}

#[test]
fn a_refused_quarantine_push_leaves_every_unit_undecided() {
    let (_dir, ctx, host) = setup("");
    host.reply("push origin HEAD:refs/heads/vivi/quarantine", 1, "denied\n");
    let results = quarantined_pair(&ctx, &host);
    assert_eq!(results.len(), 2);
    for result in &results {
        assert_eq!(result.ok, None);
        assert_eq!(result.why.as_deref(), Some("quarantine-push-failed"));
        assert!(result.quarantine.is_none());
    }
}

#[test]
fn applying_a_quarantined_batch_parks_the_units_and_opens_one_repair_unit() {
    let (_dir, ctx, host) = setup("");
    let results = quarantined_pair(&ctx, &host);
    let id = results[0].detail.clone().unwrap();
    let st = applied_state(&ctx, results);
    for uid in ["u1", "u2"] {
        assert_eq!(st.units[uid].status, Status::Quarantined);
        assert_eq!(st.units[uid].quarantine.as_deref(), Some(id.as_str()));
    }
    let repair = repair_uid(&ctx, &id);
    assert_eq!(repair, format!("pumpfix-{id}"));
    assert_eq!(
        st.units.keys().filter(|u| u.starts_with("pumpfix")).count(),
        1
    );
    let unit = &st.units[&repair];
    assert_eq!(unit.status, Status::Pending);
    assert_eq!(unit.need, repair);
    assert_eq!(unit.proof, ["cargo test --lib"]);
    assert_eq!(unit.write_scope, ["src/pump/merge.rs"]);
    assert_eq!(unit.base, Some(format!("origin/{}", branch(&id))));
    // A repair unit repairs the quarantine; it is not a member of it, it
    // merges alone, and it names a kind that dispatch can resolve.
    assert_eq!(unit.repairs.as_deref(), Some(id.as_str()));
    assert_eq!(unit.quarantine, None);
    assert!(unit.solo);
    assert_eq!(unit.tools, "plain");
    assert_eq!(repair_of(&ctx, &st, &repair).as_deref(), Some(id.as_str()));
    assert_eq!(repair_of(&ctx, &st, "u1"), None);
    assert!(unit.brief.contains("(exit 1):\ncargo test --lib"));
    assert!(unit.brief.contains("error[E0308]: mismatched types"));
    assert!(unit.brief.contains("- u1 (need n1): Unit u1"));
    assert!(unit.brief.contains("- u2 (need n1): Unit u2"));
    assert!(
        unit.brief
            .contains("The branch you start on already contains the merged batch.")
    );
    assert!(
        unit.brief
            .contains("do not revert or drop any unit's changes")
    );
    let need = &st.needs[&repair];
    assert_eq!(need.verdict, "units");
    assert_eq!(need.units, [repair.clone()]);
    assert!(st.order.contains(&repair));
    assert!(exceptions(&ctx).contains(&format!("quarantine {id} ")));
    // The repair unit ranks ahead of everything else.
    let order = ranked(&ctx, &st, vec![String::from("u1"), repair.clone()]);
    assert_eq!(order[0], repair);
}

#[test]
fn quarantined_units_are_neither_fed_to_a_lane_nor_dispatched() {
    let (_dir, ctx, host) = setup("");
    let results = quarantined_pair(&ctx, &host);
    let mut st = applied_state(&ctx, results);
    let (merger, _out, jobs) = Merger::detached();
    merger.feed(&ctx, &mut st);
    assert!(jobs.try_recv().is_err());
    assert_eq!(st.units["u1"].status, Status::Quarantined);
}

/// A dispatch config with one lane, one kind and `merge.batch` 8.
fn dispatch_cfg() -> PumpConfig {
    PumpConfig {
        branch: String::from("next"),
        merge: MergeConfig {
            shared_checks: vec![String::from("cargo test --lib")],
            ..MergeConfig::default()
        },
        dispatch: DispatchConfig {
            lanes: vec![Lane {
                model: String::from("m/a"),
                cap: 2,
            }],
            kinds: BTreeMap::from([(
                String::from("plain"),
                Kind {
                    pools: vec![String::from("fly4")],
                    ..Kind::default()
                },
            )]),
            default_kind: String::from("plain"),
            ..DispatchConfig::default()
        },
        ..PumpConfig::default()
    }
}

#[test]
fn the_repair_unit_is_dispatched_from_its_quarantine_branch() {
    let dir = tempdir().unwrap();
    let (ctx, host) = test_ctx(dir.path(), dispatch_cfg());
    host.reply("merge-base --is-ancestor origin/work/", 1, "");
    host.reply("pool status", 0, "slot fly4 bridge slot-1 free\n");
    host.reply("task send", 0, "created hand 1a2b3c4d\n");
    host.reply(
        "pool dispatch",
        0,
        "dispatched 1a2b3c4d bridge slot-1 m/a x",
    );
    host.reply("cargo test --lib", 1, "error: boom\n");
    let results = batch_work(&ctx, &[job("u1"), job("u2")]);
    let id = results[0].detail.clone().unwrap();
    let mut st = applied_state(&ctx, results);
    dispatch(&ctx, &mut st);
    let repair = repair_uid(&ctx, &id);
    assert_eq!(st.units[&repair].status, Status::Inflight);
    let sent: Vec<String> = host
        .commands()
        .into_iter()
        .filter(|c| c.contains("pool dispatch --pool"))
        .collect();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].contains(&format!("--from origin/{}", branch(&id))));
    assert_eq!(st.units["u1"].status, Status::Quarantined);
}

#[test]
fn a_merged_repair_unit_merges_every_quarantined_unit_and_closes_the_needs() {
    let (_dir, ctx, host) = setup("");
    let results = quarantined_pair(&ctx, &host);
    let id = results[0].detail.clone().unwrap();
    let mut st = applied_state(&ctx, results);
    let repair = repair_uid(&ctx, &id);
    let (merger, out, _jobs) = Merger::detached();
    out.send(MergeResult {
        uid: repair.clone(),
        ok: Some(true),
        tip: Some(String::from("tip1234567890abc")),
        secs: Some(30),
        ..MergeResult::default()
    })
    .unwrap();
    merger.apply(&ctx, &mut st);
    for uid in ["u1", "u2", repair.as_str()] {
        let unit = &st.units[uid];
        assert_eq!(unit.status, Status::Merged, "{uid}");
        assert_eq!(unit.commit.as_deref(), Some("tip1234567890abc"));
        assert_eq!(unit.merged_at, Some(1_000_000));
    }
    assert_eq!(st.merges, 3);
    assert!(st.closed_needs.contains(&String::from("n1")));
    assert!(st.closed_needs.contains(&repair));
    let ready = fs::read_to_string(ctx.home.join("needs-ready.txt")).unwrap();
    assert!(ready.contains("n1 tip1234567 u1,u2"));
    assert!(ready.contains(&format!("{repair} tip1234567 {repair}")));
}

#[test]
fn a_stuck_repair_unit_returns_the_units_as_solo_and_feed_batches_each_alone() {
    let (_dir, ctx, host) = setup("");
    let results = quarantined_pair(&ctx, &host);
    let id = results[0].detail.clone().unwrap();
    let mut st = applied_state(&ctx, results);
    let repair = repair_uid(&ctx, &id);
    st.units.get_mut(&repair).unwrap().attempts = ctx.cfg.dispatch.max_attempts;
    fail(&ctx, &mut st, &repair, "proof-failed", "still red");
    assert_eq!(st.units[&repair].status, Status::Stuck);
    for uid in ["u1", "u2"] {
        let unit = &st.units[uid];
        assert_eq!(unit.status, Status::Delivered);
        assert!(unit.solo);
        assert_eq!(unit.quarantine, None);
    }
    assert!(exceptions(&ctx).contains(&format!("quarantine-stuck {id} ")));

    // A normal unit joins the line: each solo unit goes alone, the normal
    // one takes the other lane.
    st.order.push(String::from("u3"));
    let extra = state_with(&[("u3", Status::Delivered)]);
    st.units
        .insert(String::from("u3"), extra.units["u3"].clone());
    let (merger, _out, lanes) = Merger::detached_lanes(3);
    merger.feed(&ctx, &mut st);
    let mut sent: Vec<Vec<String>> = lanes
        .iter()
        .map(|lane| {
            lane.recv_timeout(Duration::from_secs(5))
                .unwrap()
                .into_iter()
                .map(|job| job.uid)
                .collect()
        })
        .collect();
    sent.sort();
    assert_eq!(sent, [vec!["u1"], vec!["u2"], vec!["u3"]]);
}

#[test]
fn a_solo_unit_in_front_of_a_batch_does_not_pull_the_others_in() {
    let (_dir, ctx, _host) = setup("");
    let mut st = state_with(&[
        ("u1", Status::Delivered),
        ("u2", Status::Delivered),
        ("u3", Status::Delivered),
    ]);
    st.units.get_mut("u2").unwrap().solo = true;
    let (merger, _out, lanes) = Merger::detached_lanes(2);
    merger.feed(&ctx, &mut st);
    let first: Vec<String> = lanes[0]
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .into_iter()
        .map(|job| job.uid)
        .collect();
    let second: Vec<String> = lanes[1]
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .into_iter()
        .map(|job| job.uid)
        .collect();
    assert_eq!(first, ["u1", "u3"]);
    assert_eq!(second, ["u2"]);
}

#[test]
fn a_repair_unit_that_finds_no_defect_returns_the_units_to_delivered() {
    let (_dir, ctx, host) = setup("");
    let results = quarantined_pair(&ctx, &host);
    let id = results[0].detail.clone().unwrap();
    let mut st = applied_state(&ctx, results);
    let repair = repair_uid(&ctx, &id);
    repair_no_defect(&ctx, &mut st, &repair);
    for uid in ["u1", "u2"] {
        assert_eq!(st.units[uid].status, Status::Delivered);
        assert!(st.units[uid].solo);
        assert_eq!(st.units[uid].quarantine, None);
    }
    // Any other unit is left alone.
    repair_no_defect(&ctx, &mut st, "u1");
}

#[test]
fn a_restart_leaves_quarantined_units_alone() {
    let mut st = state_with(&[("u1", Status::Quarantined), ("u2", Status::Merging)]);
    st.recover_after_restart();
    assert_eq!(st.units["u1"].status, Status::Quarantined);
    assert_eq!(st.units["u2"].status, Status::Delivered);
    assert_eq!(st.counts().get("quarantined"), Some(&1));
    assert_eq!(Status::from_name("quarantined"), Status::Quarantined);
    assert_eq!(Status::Quarantined.as_str(), "quarantined");
}

#[test]
fn the_quarantine_fields_round_trip_through_the_store() {
    let dir = tempdir().unwrap();
    let mut store = PumpStore::open(&dir.path().join(DB_FILENAME)).unwrap();
    let mut st = state_with(&[("u1", Status::Quarantined), ("u2", Status::Delivered)]);
    {
        let u1 = st.units.get_mut("u1").unwrap();
        u1.quarantine = Some(String::from("q1-abc"));
        u1.repairs = Some(String::from("q0-xyz"));
        u1.base = Some(String::from("pump/quarantine/q1-abc"));
        u1.extra
            .insert(String::from("future"), serde_json::json!(7));
    }
    st.units.get_mut("u2").unwrap().solo = true;
    store.save(&st).unwrap();
    let back = store.load().unwrap();
    let u1 = &back.units["u1"];
    assert_eq!(u1.status, Status::Quarantined);
    assert_eq!(u1.quarantine.as_deref(), Some("q1-abc"));
    assert_eq!(u1.repairs.as_deref(), Some("q0-xyz"));
    assert_eq!(u1.base.as_deref(), Some("pump/quarantine/q1-abc"));
    assert!(!u1.solo);
    assert_eq!(u1.extra.len(), 1);
    assert_eq!(u1.extra["future"], serde_json::json!(7));
    let u2 = &back.units["u2"];
    assert!(u2.solo);
    assert_eq!(u2.quarantine, None);
    assert_eq!(u2.repairs, None);
    assert_eq!(u2.base, None);
    assert!(u2.extra.is_empty());
}

#[test]
fn a_repair_unit_that_times_out_fails_an_attempt_and_is_never_split() {
    let (_dir, ctx, _host) = setup("");
    let mut st = State::default();
    st.units.insert(
        String::from("pumpfix-q1"),
        Unit {
            need: String::from("pumpfix-q1"),
            status: Status::Inflight,
            attempts: 1,
            base: Some(String::from("pump/quarantine/q1")),
            ..Unit::default()
        },
    );
    crate::pump::collect::split(&ctx, &mut st, "pumpfix-q1", "agent exit 124");
    let unit = &st.units["pumpfix-q1"];
    assert_ne!(unit.status, Status::Splitting);
    assert!(!ctx.home.join("resplit/pumpfix-q1.json").exists());
}

#[test]
fn a_repair_that_met_another_batch_still_lands_its_members() {
    let (_dir, ctx, host) = setup("");
    let results = quarantined_pair(&ctx, &host);
    let id = results[0].detail.clone().unwrap();
    let mut st = applied_state(&ctx, results);
    let repair = repair_uid(&ctx, &id);
    // Somehow the repair unit rides in a second failing batch with `u3`.
    let extra = state_with(&[("u3", Status::Merging)]);
    st.order.push(String::from("u3"));
    st.units
        .insert(String::from("u3"), extra.units["u3"].clone());
    st.units.get_mut(&repair).unwrap().status = Status::Merging;
    let q2 = crate::pump::state::Quarantine {
        id: String::from("q2-aaaaaa"),
        cmd: String::from("cargo test --lib"),
        code: 1,
        text: String::from("boom"),
        units: vec![repair.clone(), String::from("u3")],
    };
    for uid in [repair.as_str(), "u3"] {
        let result = MergeResult {
            uid: uid.to_string(),
            why: Some(String::from("quarantined")),
            quarantine: Some(q2.clone()),
            ..MergeResult::default()
        };
        set_aside(&ctx, &mut st, &result);
    }
    // The repair unit was not parked as a member: it keeps what it repairs
    // and merges alone again; `u3` is the only member of q2.
    let unit = &st.units[&repair];
    assert_eq!(unit.status, Status::Delivered);
    assert!(unit.solo);
    assert_eq!(unit.quarantine, None);
    assert_eq!(repair_of(&ctx, &st, &repair).as_deref(), Some(id.as_str()));
    assert_eq!(st.units["u3"].status, Status::Quarantined);
    assert_eq!(st.units["u3"].quarantine.as_deref(), Some("q2-aaaaaa"));
    // The second repair does not repair the first one.
    let repair2 = repair_uid(&ctx, "q2-aaaaaa");
    assert_eq!(st.units[&repair2].repairs.as_deref(), Some("q2-aaaaaa"));
    assert!(st.units[&repair2].brief.contains("- u3 (need n1)"));
    assert!(!st.units[&repair2].brief.contains(&format!("- {repair} ")));

    // The repair lands: q1's members land with it.
    let (merger, out, _jobs) = Merger::detached();
    out.send(MergeResult {
        uid: repair.clone(),
        ok: Some(true),
        tip: Some(String::from("tip1234567890abc")),
        ..MergeResult::default()
    })
    .unwrap();
    merger.apply(&ctx, &mut st);
    for uid in ["u1", "u2", repair.as_str()] {
        assert_eq!(st.units[uid].status, Status::Merged, "{uid}");
    }
    assert_eq!(st.units["u3"].status, Status::Quarantined);
}

#[test]
fn the_repair_unit_takes_the_default_kind_when_no_member_names_one() {
    let (_dir, ctx, host) = setup("");
    let results = quarantined_pair(&ctx, &host);
    let id = results[0].detail.clone().unwrap();
    let (merger, out, _jobs) = Merger::detached();
    let mut st = state_with(&[("u1", Status::Merging), ("u2", Status::Merging)]);
    for unit in st.units.values_mut() {
        unit.tools = String::new();
    }
    for result in results {
        out.send(result).unwrap();
    }
    merger.apply(&ctx, &mut st);
    assert_eq!(st.units[&repair_uid(&ctx, &id)].tools, "plain");
}

#[test]
fn no_resolvable_kind_releases_the_units_as_solo_and_opens_no_repair() {
    let (_dir, ctx, host) = setup_kinds(&[]);
    let results = quarantined_pair(&ctx, &host);
    let id = results[0].detail.clone().unwrap();
    let mut st = applied_state(&ctx, results);
    assert_eq!(
        st.units.keys().filter(|u| u.starts_with("pumpfix")).count(),
        0
    );
    for uid in ["u1", "u2"] {
        let unit = &st.units[uid];
        assert_eq!(unit.status, Status::Delivered, "{uid}");
        assert!(unit.solo);
        assert_eq!(unit.quarantine, None);
    }
    // One exception for the batch, not one per unit.
    let text = exceptions(&ctx);
    assert_eq!(
        text.matches(&format!("quarantine-no-kind {id} ")).count(),
        1
    );
    assert!(!text.contains(&format!("quarantine {id} ")));
    // The released units are fed alone.
    let (merger, _out, lanes) = Merger::detached_lanes(2);
    merger.feed(&ctx, &mut st);
    let mut sent: Vec<Vec<String>> = lanes
        .iter()
        .map(|lane| {
            lane.recv_timeout(Duration::from_secs(5))
                .unwrap()
                .into_iter()
                .map(|job| job.uid)
                .collect()
        })
        .collect();
    sent.sort();
    assert_eq!(sent, [vec!["u1"], vec!["u2"]]);
}
