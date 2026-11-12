use super::*;
use std::process::Command;

#[test]
fn dispatch_delivers_target_tree_from_the_pool_url_and_claimed_slot_name() {
    for root in [None, Some("/Volumes/Build Trees")] {
        let fake = Fake::default();
        fake.add(slot("m1", "space-p-slot-1", "stopped", "p", 1));
        let mut config = config();
        config.tree_root = root.map(str::to_owned);
        let mut plan = plan(&flags("m/ok")).unwrap();
        plan.remote = "git@hub.example:faberlang/radix.git".into();
        plan.target = Some(target::JobTarget {
            check: None,
            id: "not-the-repo-name".into(),
            source: plan.remote.clone(),
            publish_remote: None,
            cache_key: "a".repeat(64),
            local_source: None,
            seed: vec![],
            requires: vec![],
            build_dir: None,
        });
        let token = Secret::new(String::new());
        let progress = crate::pool::progress::Progress::silent();
        let launch = Launch {
            config: &config,
            plan: &plan,
            token: &token,
            start: "main",
            progress: &progress,
            ready_timeout: None,
            reset_base: None,
        };
        dispatch(&fake, &pid("p"), &launch, 123, &pacing()).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("bundle.tgz");
        std::fs::write(&archive, &fake.files.borrow()[0].2).unwrap();
        let out = Command::new("tar")
            .arg("-xOzf")
            .arg(archive)
            .arg("job/params.env")
            .output()
            .unwrap();
        assert!(out.status.success());
        let params = String::from_utf8(out.stdout).unwrap();
        if root.is_some() {
            assert!(
                params.contains(
                    "TARGET_TREE='/Volumes/Build Trees/faberlang/radix/space-p-slot-1'\n"
                ),
                "{params}"
            );
        } else {
            assert!(!params.contains("TARGET_TREE="), "{params}");
        }
        assert!(params.contains(&format!("CHECKOUT_KEY='{}'", "a".repeat(64))));
    }
}
