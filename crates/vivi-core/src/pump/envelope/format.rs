//! Shared envelope prompt formatting.

use vivi_mail::letter::roles::RoleKind;

const RULES: &str = "The actual reply starts immediately with an opening `+++` line and closes its TOML front matter with a matching `+++` line. The `answer` is the first key. Permitted prose follows the envelope. No Markdown preamble or code-fence wrapper surrounds the actual reply. Unknown keys are refused. Free text uses TOML literal strings `'...'`, including multiline literal strings `'''...'''`. Lists default to empty. Examples illustrate syntax only and must not be copied as findings or work performed.";

/// Render the shared rules and the fields accepted for one role.
pub(crate) fn role(kind: RoleKind) -> String {
    format!("{RULES}\n{}", fields(kind))
}

fn fields(kind: RoleKind) -> &'static str {
    match kind {
        RoleKind::Researcher => {
            "researcher: answer = \"READY\" | \"NOT READY\" | \"NEEDS FURTHER REVIEW\"; open_questions (list)"
        }
        RoleKind::Planner => {
            "planner: answer = \"planned\" | \"delivered\" | \"operator\" | \"hold\" | \"design\"; reason (string, required unless planned)"
        }
        RoleKind::Admitter => {
            "admitter: answer = \"admitted\" | \"revise\" | \"escalate\"; required_changes (non-empty list for revise); findings (list); reason (string, required for escalate)"
        }
        RoleKind::Slicer => {
            "slicer: answer = \"sliced\" | \"escalate\"; mode = \"slice\" | \"rework\" | \"split\" (default \"slice\"); [[slice]] {id, title, brief, target, write_scope (list), deps (list)}; at least one for sliced and none for escalate; split uses 2-4 slices with ids `<old id>.<n>`; reason (string, required for escalate)"
        }
        RoleKind::Auditor => {
            "auditor: answer = \"clean_pass\" | \"residual\" | \"block_range\" | \"block_ship\"; raise_rung = \"need\" | \"main\" | \"release\" (optional); [[requirement]] {id, text, status = SATISFIED | PARTIAL | MISSING | UNVERIFIED | NOT_APPLICABLE, evidence (list)}; [[finding]] {id, content, paths (list), recheck_trigger}"
        }
        RoleKind::Judge => "judge: answer = \"block\" | \"residual\"; reason (string, required)",
        RoleKind::Cto => {
            "head-cto: answer = \"research\" | \"plan\" | \"slice\" | \"rework\" | \"lock\"; reason (string, required); new_fact (string, required from run 3 on for non-lock answers)"
        }
        RoleKind::Merge => {
            "merge: answer = \"merged\" | \"conflict\"; conflicts (list of paths); design_change (bool); [[check]] {cmd, result = pass | fail | skipped, seconds (finite, non-negative number)}; no SHA, ref, commit or exit code; published SHA and merged fact come from runner receipt"
        }
        RoleKind::Hand => {
            "hand: answer = \"built\" | \"blocked-scope\" | \"failed\"; change (list); [[check]] {cmd, result = pass | fail | skipped, seconds (finite, non-negative number)}; deviations (list); [[follow_up]] {paths (list), need, optional (bool)}; remaining (list); scope_paths (non-empty list required for blocked-scope); scope_reason (non-empty string required for blocked-scope); scope_design (bool)"
        }
        RoleKind::Repairer => {
            "repairer: answer = \"built\" | \"failed\"; change (list); [[check]] {cmd, result = pass | fail | skipped, seconds (finite, non-negative number)}; deviations (list); no SHA, ref, commit or exit code"
        }
        RoleKind::HeadCeo => {
            "head-ceo: answer = \"reviewed\" | \"no-op\"; served, research, dropped (lists of handles)"
        }
    }
}

#[cfg(test)]
#[path = "format_test.rs"]
mod tests;
