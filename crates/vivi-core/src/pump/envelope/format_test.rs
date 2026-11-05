//! Extraction-level tests for shared envelope formatting guidance.

use super::role;
use vivi_mail::letter::roles::RoleKind;

#[test]
fn shared_rules_describe_the_reply_boundary_and_literal_strings() {
    let text = role(RoleKind::Hand);
    for rule in [
        "The actual reply starts immediately with an opening `+++` line",
        "closes its TOML front matter with a matching `+++` line",
        "`answer` is the first key",
        "Permitted prose follows the envelope",
        "No Markdown preamble or code-fence wrapper surrounds the actual reply",
        "Unknown keys are refused",
        "TOML literal strings `'...'`",
        "multiline literal strings `'''...'''`",
        "Examples illustrate syntax only and must not be copied as findings or work performed",
        "Lists default to empty",
    ] {
        assert!(text.contains(rule), "missing shared rule: {rule}\n{text}");
    }
}

#[test]
fn role_descriptions_cover_each_roles_fields_and_contract() {
    let expected = [
        (
            RoleKind::Researcher,
            "researcher: answer = \"READY\" | \"NOT READY\" | \"NEEDS FURTHER REVIEW\"; open_questions (list)",
        ),
        (
            RoleKind::Planner,
            "planner: answer = \"planned\" | \"delivered\" | \"operator\" | \"hold\" | \"design\"; reason (string, required unless planned)",
        ),
        (
            RoleKind::Admitter,
            "admitter: answer = \"admitted\" | \"revise\" | \"escalate\"; required_changes (non-empty list for revise); findings (list); reason (string, required for escalate)",
        ),
        (
            RoleKind::Slicer,
            "slicer: answer = \"sliced\" | \"escalate\"; mode = \"slice\" | \"rework\" | \"split\" (default \"slice\"); [[slice]] {id, title, brief, target, write_scope (list), deps (list)}; at least one for sliced and none for escalate; split uses 2-4 slices with ids `<old id>.<n>`; reason (string, required for escalate)",
        ),
        (
            RoleKind::Auditor,
            "auditor: answer = \"clean_pass\" | \"residual\" | \"block_range\" | \"block_ship\"; raise_rung = \"need\" | \"main\" | \"release\" (optional); [[requirement]] {id, text, status = SATISFIED | PARTIAL | MISSING | UNVERIFIED | NOT_APPLICABLE, evidence (list)}; [[finding]] {id, content, paths (list), recheck_trigger}",
        ),
        (
            RoleKind::Judge,
            "judge: answer = \"block\" | \"residual\"; reason (string, required)",
        ),
        (
            RoleKind::Cto,
            "head-cto: answer = \"research\" | \"plan\" | \"slice\" | \"rework\" | \"lock\"; reason (string, required); new_fact (string, required from run 3 on for non-lock answers)",
        ),
        (
            RoleKind::Merge,
            "merge: answer = \"merged\" | \"conflict\"; conflicts (list of paths); design_change (bool); [[check]] {cmd, result = pass | fail | skipped, seconds (finite, non-negative number)}; no SHA, ref, commit or exit code; published SHA and merged fact come from runner receipt",
        ),
        (
            RoleKind::Hand,
            "hand: answer = \"built\" | \"blocked-scope\" | \"failed\"; change (list); [[check]] {cmd, result = pass | fail | skipped, seconds (finite, non-negative number)}; deviations (list); [[follow_up]] {paths (list), need, optional (bool)}; remaining (list); scope_paths (non-empty list required for blocked-scope); scope_reason (non-empty string required for blocked-scope); scope_design (bool)",
        ),
        (
            RoleKind::Repairer,
            "repairer: answer = \"built\" | \"failed\"; change (list); [[check]] {cmd, result = pass | fail | skipped, seconds (finite, non-negative number)}; deviations (list); no SHA, ref, commit or exit code",
        ),
        (
            RoleKind::HeadCeo,
            "head-ceo: answer = \"reviewed\" | \"no-op\"; served, research, dropped (lists of handles)",
        ),
    ];
    for (kind, description) in expected {
        let rendered = role(kind);
        assert!(
            rendered.contains(description),
            "missing description for {kind:?}: {rendered}"
        );
    }
}

#[test]
fn every_declared_role_has_a_role_specific_description() {
    for kind in RoleKind::ALL {
        let rendered = role(kind);
        assert!(
            rendered.contains("answer ="),
            "missing answer description for {kind:?}"
        );
    }
}
