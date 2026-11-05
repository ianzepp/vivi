use super::*;

#[test]
fn transition_records_immutable_result_and_deduplicates_scope_grants() {
    let dir = tempfile::tempdir().expect("temp dir");
    let store = PumpStore::open(&dir.path().join("pump.db")).expect("store");
    let run = crate::pump::flow::index::RunRow {
        need: "n".into(),
        run: 1,
        flow: "f".into(),
        flow_version: 1,
        kind: "k".into(),
        entry: "e".into(),
        state: "live".into(),
    };
    store.insert_run(&run).expect("run");
    store
        .insert_step(
            "n",
            &run.step("node", "task", crate::pump::flow::index::StepState::Pending),
        )
        .expect("step");
    store
        .insert_slice_scope("task", 1, &["a".into()])
        .expect("scope");
    store
        .link_slice(&crate::pump::store::flow_links::SliceLink::for_run(
            &run,
            "task",
            None,
            vec!["dep".into()],
        ))
        .expect("link");
    let result = SliceScopeResult {
        hand: "h".into(),
        reply_job: "j".into(),
        need: "n".into(),
        run: 1,
        task: "task".into(),
        target: "t".into(),
        source_branch: "b".into(),
        dispatch_base: None,
        authorization_base: None,
        candidate: serde_json::json!({"sha":"x"}),
        original_scope: vec!["a".into()],
        changed_paths: vec!["b".into()],
        additions: vec!["b".into()],
        granted: vec!["b".into(), "b".into()],
        held: vec![],
        owners: vec![],
        disposition: "approved".into(),
        verification_reasons: vec![],
        conflict: None,
    };
    let change = ScopeTransition {
        result,
        deps: vec!["dep".into(), "dep".into()],
        scope_state: "none".into(),
        holders: vec![],
        step_state: "done".into(),
        publication: vec![("built-hook".into(), serde_json::json!({"x":1}))],
    };
    store.apply_scope_transition(&change).expect("transition");
    assert_eq!(
        store.slice_scope("task").expect("scope").expect("row"),
        vec!["a", "b"]
    );
    assert_eq!(
        store.slice_link("task").expect("link").expect("row").deps,
        vec!["dep"]
    );
}
