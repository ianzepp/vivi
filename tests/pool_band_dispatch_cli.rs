//! The band-dispatch baseline over the portable fixture
//! (`support/pool_band.rs`): one band entry, no per-model caps, and the
//! existing dispatch pipeline untouched. A role-kind read-only job resolves
//! the band's only model-and-reasoning pair, records a truthful dispatch on
//! the task, runs the charter above the pool rules, and publishes nothing.

#[path = "support/pool_band.rs"]
mod pool_band;

use vivi::pool::record::DispatchRecord;

use pool_band::{
    BandRig, add_role, bands_table, collect, committer_table, create, dispatch, events,
    harness_table, job_file, send_task, set_charter, task_body,
};

const CHARTER: &str =
    "Audit faithfully.\nReport every finding.\nReply inside a v20 envelope:\n+++\n";

/// The baseline rig: pool `band` with a one-entry band (`P1` -> standin at
/// reasoning `mid`), no `caps` table, a harness that renders the chosen
/// model and reasoning into its stdout (the job receipt), and an `auditor`
/// role — kind `auditor` (read-only), band `P1`, with a charter.
fn baseline() -> BandRig {
    let rig = pool_band::rig(
        "band",
        1,
        &format!(
            "{}{}{}",
            bands_table("band", &[("P1", "standin", "mid")]),
            harness_table(
                "band",
                "standin",
                "echo model={model} reasoning={reasoning}",
                &["standin"],
            ),
            committer_table("band"),
        ),
    );
    add_role(&rig, "auditor", Some("auditor"), Some("P1"));
    set_charter(&rig, "auditor", CHARTER);
    create(&rig);
    rig
}

#[test]
fn single_entry_without_limits_preserves_existing_dispatch() {
    let rig = baseline();
    // A read-only role job: `--from main` is the only targeting it needs —
    // no branch, and no band line in the body (the role's band decides).
    let body = task_body(Some("main"), None, None, &[], "work");
    let task = send_task(&rig, "auditor", "Audit it", &body);

    // The dispatch chose the band's only pair and forced read-only by kind.
    let (said_ok, said) = dispatch(&rig, &task, &["--from", "main"]);
    assert!(said_ok, "{said}");
    assert!(said.contains("model standin"), "{said}");
    assert!(said.contains("read-only"), "{said}");
    assert!(
        said.contains("role auditor band P1 -> standin mid read-only (role kind auditor)"),
        "{said}"
    );

    // Collect: the job finished read-only and the harness rendered the pair
    // it was given into the receipt.
    let waited = collect(&rig, &task);
    assert!(waited.contains("read_only_"), "{waited}");
    assert!(waited.contains("model=standin reasoning=mid"), "{waited}");

    // The trusted dispatch record is the note of the durable
    // `pool_dispatched` event, read through the library.
    let logged = events(&rig);
    let dispatched = logged
        .iter()
        .find(|e| e.handle == task && e.event_type == "pool_dispatched")
        .expect("a pool_dispatched event");
    let record: DispatchRecord = serde_json::from_str(&dispatched.note).expect("a dispatch record");
    assert_eq!(record.pool, "band");
    assert_eq!(record.model, "standin");
    assert_eq!(record.kind.as_deref(), Some("auditor"));
    assert_eq!(record.from, "main");
    assert!(record.read_only, "{record:?}");
    assert_eq!(record.branch, "", "a read-only job has no branch");

    // The `pool_collected` event closes the dispatch, finished and read-only.
    let collected = logged
        .iter()
        .find(|e| e.handle == task && e.event_type == "pool_collected")
        .expect("a pool_collected event");
    let note: serde_json::Value = serde_json::from_str(&collected.note).unwrap();
    assert_eq!(note["outcome"], "finished");
    assert_eq!(note["read_only"], serde_json::Value::Bool(true));

    // The standing prompt: the charter above the pool rules.
    let rules = std::fs::read_to_string(job_file(&rig, "rules.md")).unwrap();
    assert!(rules.contains("Audit faithfully."), "{rules}");
    assert!(
        rules.contains("# Standing charter (role auditor)"),
        "{rules}"
    );
    assert!(rules.contains("# Pool rules"), "{rules}");
    assert!(
        rules.contains("Standing rules for the remote worker"),
        "{rules}"
    );

    // Nothing was published: the repository still holds only main and feat/a.
    let heads = pool_band::git_out(
        &rig.repo,
        &["for-each-ref", "--format=%(refname)", "refs/heads"],
    );
    assert_eq!(heads.lines().count(), 2, "{heads}");
}
