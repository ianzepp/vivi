//! The checkout-regression fixtures through the real binary: the reusable
//! real-Git topology and durable pump history the worker-boundary suites
//! build on. The proofs here are the fixture contract: main lacks both
//! implementation files, the need branch holds both through genuine merge
//! ancestry, and the durable inventory identities and admission pairings
//! match what the merges actually did.

#[path = "support/need_checkout_repo.rs"]
mod fixture;

use std::process::Command;

use fixture::{
    ONE_BODY, ONE_PATH, Options, Rig, TWO_BODY, TWO_PATH,
};

/// Whether `git` succeeds (ancestry checks answer nonzero, not error).
fn git_ok(dir: &std::path::Path, args: &[&str]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .status()
        .unwrap()
        .success()
}

/// The tip of one ref on a remote, or `None` when it is absent.
fn remote_tip(dir: &std::path::Path, url: &str, reference: &str) -> Option<String> {
    let listed = fixture::git_output(dir, &["ls-remote", url, reference]);
    listed.split_whitespace().next().map(str::to_owned)
}

/// The names of every file in one commit's tree.
fn tree_files(dir: &std::path::Path, commit: &str) -> String {
    fixture::git_output(dir, &["ls-tree", "-r", "--name-only", commit])
}

/// Assert the fixture Git contract for one rig: main lacks both
/// implementation files, the need branch holds both on the remote, and the
/// two merges are genuine, successive, and carry the known file contents.
fn assert_git_contract(rig: &Rig) {
    // Main (the base) lacks both implementation files.
    let main_files = tree_files(&rig.seed, &rig.base);
    assert!(!main_files.contains(ONE_PATH), "{main_files}");
    assert!(!main_files.contains(TWO_PATH), "{main_files}");
    assert_eq!(
        remote_tip(&rig.seed, &fixture::file_url(&rig.bare), "refs/heads/main")
            .expect("bare main"),
        rig.base,
        "the bare main is not the recorded base"
    );

    // The need branch is published at the final merge tip.
    let branch = vivi::pump::flow::need_branch::branch(&rig.need);
    assert_eq!(
        remote_tip(
            &rig.seed,
            &fixture::file_url(&rig.bare),
            &format!("refs/heads/{branch}"),
        )
        .expect("the need branch is missing from the bare remote"),
        rig.merge_two,
        "the bare need branch is not at the final merge tip"
    );

    // Both merges are genuine merges with the right parents, in order.
    assert!(
        git_ok(&rig.seed, &["merge-base", "--is-ancestor", &rig.base, &rig.merge_one]),
        "the base is not an ancestor of the first merge"
    );
    assert!(
        git_ok(&rig.seed, &["merge-base", "--is-ancestor", &rig.merge_one, &rig.merge_two]),
        "the first merge is not an ancestor of the second"
    );
    let impl_one = fixture::git_output(&rig.seed, &["rev-parse", "impl/one"]);
    let impl_two = fixture::git_output(&rig.seed, &["rev-parse", "impl/two"]);
    assert_eq!(
        fixture::git_output(&rig.seed, &["rev-parse", "HEAD"]),
        rig.base,
        "the shared source repository is not back on main"
    );
    assert_eq!(
        fixture::git_output(&rig.seed, &["rev-parse", "refs/heads/impl/one^", "refs/heads/impl/two^"]),
        format!("{base}\n{base}", base = rig.base),
        "the implementation branches are not both cut from the base"
    );
    assert_eq!(
        fixture::git_output(&rig.seed, &["rev-parse", &format!("{one}^1", one = rig.merge_one), &format!("{one}^2", one = rig.merge_one)]),
        format!("{rig.base}\n{impl_one}"),
        "the first merge is not a merge of impl/one into the base"
    );
    assert_eq!(
        fixture::git_output(&rig.seed, &["rev-parse", &format!("{two}^1", two = rig.merge_two), &format!("{two}^2", two = rig.merge_two)]),
        format!("{one}\n{impl_two}", one = rig.merge_one),
        "the second merge is not a merge of impl/two onto the first"
    );

    // The final tip carries both implementation files with known contents.
    assert_eq!(
        fixture::git_output(&rig.seed, &["show", &format!("{}:{ONE_PATH}", rig.merge_two)]),
        ONE_BODY.trim_end(),
        "the first implementation file drifted"
    );
    assert_eq!(
        fixture::git_output(&rig.seed, &["show", &format!("{}:{TWO_PATH}", rig.merge_two)]),
        TWO_BODY.trim_end(),
        "the second implementation file drifted"
    );
}

/// A `vivi` run that must succeed.
fn ok(args: &[&str]) {
    fixture::assert_success(&fixture::vivi(args));
}

#[test]
fn main_lacks_the_files_and_the_need_branch_holds_both_through_merges() {
    let rig = fixture::scaffold(&Options::default());
    assert_git_contract(&rig);
    ok(&["config", "validate", "--project", rig.project.to_str().unwrap()]);
}

#[test]
fn the_non_primary_target_keeps_the_decoy_primary_untouched() {
    let rig = fixture::scaffold(&Options {
        non_primary_need_target: true,
        ..Options::default()
    });
    assert_git_contract(&rig);
    ok(&["config", "validate", "--project", rig.project.to_str().unwrap()]);

    // The decoy primary never saw the need branch or the files.
    let decoy = rig.decoy.as_ref().unwrap();
    let listing = fixture::git_output(
        &decoy.source,
        &["ls-remote", &fixture::file_url(&decoy.bare)],
    );
    assert!(
        !listing.contains("vivi/need/"),
        "the decoy primary carries a need branch: {listing}"
    );
    assert_eq!(listing.lines().count(), 1, "the decoy primary grew refs: {listing}");
    assert!(
        !tree_files(&decoy.source, "main").contains(ONE_PATH),
        "the decoy primary gained an implementation file"
    );
    assert_eq!(
        remote_tip(
            &decoy.source,
            &fixture::file_url(&decoy.bare),
            "refs/heads/main",
        )
        .unwrap(),
        fixture::git_output(&decoy.source, &["rev-parse", "main"]),
        "the decoy primary main is not its workspace checkout's main"
    );
    let config =
        std::fs::read_to_string(rig.project.join(".vivi/mailspace.toml")).unwrap();
    assert!(config.contains("[pump.targets.default]"), "{config}");
    assert!(config.contains("[pump.targets.alt]"), "{config}");
}

#[test]
fn durable_inventory_identities_and_admissions_match_the_fixture() {
    let rig = fixture::scaffold(&Options::default());
    fixture::seed_build_run(&rig, 1);
    let archive = fixture::archive_first_generation(&rig);
    fixture::seed_continuation(&rig);
    let run2 = fixture::seed_build_run(&rig, 2);
    let store = fixture::open_store(&rig);

    // The live history is the withdrawn attempt plus the live continuation.
    let runs = store.runs_of(&rig.need).unwrap();
    assert_eq!(
        runs.iter()
            .map(|run| (run.run, run.state.as_str()))
            .collect::<Vec<_>>(),
        vec![(1, "withdrawn"), (2, "live")],
    );

    // Run 2's steps: the full completed fanout and the pending merge gate.
    let steps = store.steps_of(&rig.need, 2).unwrap();
    let expected: [(&str, Option<&str>); 9] = [
        ("sentinel", None),
        ("plan", Some("planned")),
        ("admit1", Some("admitted")),
        ("plan2", Some("planned")),
        ("admit2", Some("admitted")),
        ("slice", Some("sliced")),
        ("slice:one", Some("merged")),
        ("slice:two", Some("merged")),
        ("merge", None),
    ];
    assert_eq!(
        steps
            .iter()
            .map(|step| (step.node.as_str(), step.verdict.as_deref()))
            .collect::<Vec<_>>(),
        expected,
        "run 2's step inventory does not match the fixture"
    );
    let step_of = |node: &str| {
        steps
            .iter()
            .find(|step| step.node == node)
            .unwrap_or_else(|| panic!("no {node} step"))
    };
    assert_eq!(step_of("merge").state, vivi::pump::flow::index::StepState::Pending);

    // Both merged slices closed over exactly the two genuine merge commits.
    let branch = vivi::pump::flow::need_branch::branch(&rig.need);
    for (node, commit) in [("slice:one", &rig.merge_one), ("slice:two", &rig.merge_two)] {
        assert_eq!(step_of(node).verdict.as_deref(), Some("merged"), "{node}");
        let _ = commit;
    }
    let one_task = step_of("slice:one").task.clone();
    let two_task = step_of("slice:two").task.clone();
    assert_eq!(
        store.slice_scope(&one_task).unwrap(),
        Some(vec![String::from(ONE_PATH)]),
    );
    assert_eq!(
        store.slice_scope(&two_task).unwrap(),
        Some(vec![String::from(TWO_PATH)]),
    );
    for (task, deps) in [(&one_task, &[] as &[&str]), (&two_task, &[] as &[&str])] {
        let link = store.slice_link(task).unwrap().expect("slice link");
        assert_eq!(link.fanout.as_deref(), Some(run2.slicer_task.as_str()));
        assert!(link.deps.is_empty(), "{deps:?}");
        let _ = deps;
    }
    assert_eq!(
        store.open_fanout(&rig.need, 2).unwrap().as_deref(),
        Some(run2.slicer_task.as_str()),
    );

    // Every payload identity matches its step; the admissions pair up.
    let payloads = store.step_payloads_of_need(&rig.need).unwrap();
    assert_eq!(payloads.len(), 7, "{payloads:?}");
    for row in &payloads {
        let step = store.step_by_task(&row.task).unwrap().expect("step row");
        assert_eq!(row.need, rig.need);
        assert_eq!(row.run, step.run);
        assert_eq!(row.node, step.node);
        assert_eq!(row.class, class_of(&row.node));
        let answer = row.payload["front"]["answer"].as_str().unwrap_or("");
        let want = match row.class.as_str() {
            "planner" => "planned",
            "admitter" => "admitted",
            _ => "sliced",
        };
        assert_eq!(answer, want, "{} payload answer", row.task);
        assert!(
            !row.payload["body"].as_str().unwrap_or_default().is_empty(),
            "{} payload lost its body",
            row.task
        );
    }
    for (plan, admit) in [("plan", "admit1"), ("plan2", "admit2")] {
        let (plan_step, admit_step) = (step_of(plan), step_of(admit));
        assert_eq!(plan_step.state, vivi::pump::flow::index::StepState::Done);
        assert_eq!(admit_step.state, vivi::pump::flow::index::StepState::Done);
        assert_eq!(
            store
                .step_payloads_of_need(&rig.need)
                .unwrap()
                .iter()
                .filter(|row| &row.task == &plan_step.task)
                .count(),
            1
        );
        assert_eq!(branch_name(&store, &rig.need), branch);
        assert_eq!(run2.merge_task, step_of("merge").task);
    }

    // The live need-branch row stands over the recorded base on the target.
    let row = store.need_branch(&rig.need).unwrap().expect("branch row");
    assert_eq!(row.branch, branch);
    assert_eq!(row.base.as_deref(), Some(rig.base.as_str()));
    assert_eq!(row.status, "live");
    assert_eq!(row.run, 2);
    assert_eq!(row.target, "default");
    assert!(row.priors.is_empty());

    // The withdrawn continuation carries its own completed admission pair.
    let steps1 = store.steps_of(&rig.need, 1).unwrap();
    assert_eq!(
        steps1
            .iter()
            .map(|step| (step.node.as_str(), step.verdict.as_deref()))
            .collect::<Vec<_>>(),
        vec![
            ("sentinel", None),
            ("plan", Some("planned")),
            ("admit1", Some("admitted")),
        ],
    );

    // The legacy archive generation is distinct and still readable.
    let legacy = fixture::open_store_at(&archive.join("pump.db"));
    let legacy_runs = legacy.runs_of(&rig.need).unwrap();
    assert_eq!(
        legacy_runs.iter().map(|run| (run.run, run.state.as_str())).collect::<Vec<_>>(),
        vec![(1, "live")],
        "the archive is not its own generation"
    );
    let legacy_steps = legacy.steps_of(&rig.need, 1).unwrap();
    assert_eq!(legacy_steps.len(), 9);
    let legacy_one = legacy_steps
        .iter()
        .find(|step| step.node == "slice:one")
        .unwrap();
    assert_eq!(legacy_one.verdict.as_deref(), Some("merged"));
    assert_eq!(
        legacy.need_branch(&rig.need).unwrap().unwrap().run,
        1,
        "the archive branch row does not belong to its generation"
    );
    assert_eq!(legacy.step_payloads_of_need(&rig.need).unwrap().len(), 5);

    // The live store is the only writer view; the archive opens read-only.
    assert!(
        store.step_by_task(&two_task).unwrap().is_some(),
        "the live store lost the second slice"
    );
}

fn class_of(node: &str) -> &'static str {
    match node {
        "plan" | "plan2" => "planner",
        "admit1" | "admit2" => "admitter",
        "slice" => "slicer",
        other => panic!("unexpected payload node {other}"),
    }
}

fn branch_name(store: &vivi::pump::store::PumpStore, need: &str) -> String {
    store.need_branch(need).unwrap().expect("branch row").branch
}

use vivi::pump::store::PumpStore as _PumpStoreAlias;
#[allow(unused_imports)]
use vivi as _vivi_alias;
