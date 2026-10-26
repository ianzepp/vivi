//! Fixture smoke for the real-Git checkout regression rig, in both local
//! pool source modes. The seeded need branch really carries two
//! no-fast-forward merge commits cut at the side main tip, the markers never
//! touch main, the worker's integration checkout stays isolated before any
//! dispatch (no marker, no local need-branch ref), and the durable run,
//! slice and need-branch rows match the Git history.

#[path = "support/pump_need_checkout.rs"]
mod fixture;

use vivi::pump::flow::fanout::SLICE_NODE_PREFIX;
use vivi::pump::flow::index::run_state;
use vivi::pump::flow::need_branch;

use fixture::{
    LocalMode, MARKER_A, MARKER_B, PRIMARY_TARGET, RUN, SIDE_MAIN, SIDE_TARGET, file_on_ref,
    git_succeeds, is_ancestor, merge_base, open_store, parents, remote_tip, scaffold, seed,
};

#[test]
fn fixture_seeds_a_real_merged_need_branch_over_an_isolated_worker() {
    for mode in [LocalMode::Fetch, LocalMode::Worktree] {
        let rig = scaffold(mode);
        let seeded = seed(&rig);
        let branch = need_branch::branch(&seeded.need);

        // Real no-fast-forward merge ancestry: both merges have two parents,
        // the first parent chain runs merge_b -> merge_a -> base, and each
        // merge carries its own slice side branch as the second parent.
        let merge_a_parents = parents(&rig.side_seed, &seeded.merge_a);
        assert_eq!(
            merge_a_parents,
            vec![seeded.base.clone(), seeded.slice_a_tip.clone()],
            "the first integration is not the recorded no-fast-forward merge"
        );
        let merge_b_parents = parents(&rig.side_seed, &seeded.merge_b);
        assert_eq!(
            merge_b_parents,
            vec![seeded.merge_a.clone(), seeded.slice_b_tip.clone()],
            "the second integration is not the recorded no-fast-forward merge"
        );
        for ancestor in [&seeded.base, &seeded.merge_a, &seeded.merge_b] {
            assert!(
                is_ancestor(&rig.side_seed, ancestor, &seeded.need_tip),
                "{ancestor} is not ancestry of the need tip"
            );
        }
        // The branch was cut exactly at main: their merge base is the
        // recorded base, and main itself holds neither marker.
        assert_eq!(
            merge_base(&rig.side_seed, SIDE_MAIN, &seeded.need_tip),
            seeded.base,
            "the need branch does not diverge from main at the recorded base"
        );
        assert_eq!(
            remote_tip(&rig.side_seed, &rig.side_bare, &branch).as_deref(),
            Some(seeded.need_tip.as_str()),
            "the bare origin does not hold the seeded need tip"
        );
        assert!(
            file_on_ref(&rig.side_seed, &branch, MARKER_A)
                && file_on_ref(&rig.side_seed, &branch, MARKER_B),
            "the need branch tip does not carry both markers"
        );
        for marker in [MARKER_A, MARKER_B] {
            assert!(
                !file_on_ref(&rig.side_seed, SIDE_MAIN, marker),
                "{marker} leaked onto {SIDE_MAIN}"
            );
        }

        // The worker is isolated before any dispatch: its side integration
        // checkout has neither marker and no local need-branch ref, and the
        // primary checkout is equally clean.
        let worker = rig.target_repo(SIDE_TARGET);
        for marker in [MARKER_A, MARKER_B] {
            assert!(
                !worker.join(marker).exists(),
                "the worker checkout already carries {marker}"
            );
        }
        let local_ref = format!("refs/heads/{branch}");
        assert!(
            !git_succeeds(&worker, &["rev-parse", "-q", "--verify", &local_ref]),
            "the worker holds a local need-branch ref before dispatch"
        );
        let primary = rig.target_repo(PRIMARY_TARGET);
        assert!(
            !primary.join(MARKER_A).exists() && !primary.join(MARKER_B).exists(),
            "the primary integration checkout carries a marker"
        );

        // The durable evidence matches: the need branch row points at the
        // non-primary target with the recorded base, the run is live, and
        // the slice step is a real slice node.
        let row = open_store(&rig)
            .need_branch(&seeded.need)
            .unwrap()
            .expect("need branch row");
        assert_eq!(
            row.target, SIDE_TARGET,
            "the need is not on the side target"
        );
        assert_eq!(
            row.base.as_deref(),
            Some(seeded.base.as_str()),
            "the recorded base is not the side main tip"
        );
        assert_eq!(row.branch, branch);
        assert_eq!(row.status, need_branch::LIVE);
        assert_eq!(row.run, RUN);
        let store = open_store(&rig);
        let run = store.run_row(&seeded.need, RUN).unwrap();
        assert_eq!(run.state, run_state::LIVE);
        let slice = store
            .step_by_task(&seeded.slice_task)
            .unwrap()
            .expect("slice step row");
        assert!(
            slice.node.starts_with(SLICE_NODE_PREFIX),
            "the seeded slice evidence is not a slice step: {}",
            slice.node
        );
    }
}
