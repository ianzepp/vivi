//! Shared prompt contracts for role and slicer reply envelopes.

use vivi_mail::letter::roles::{RoleKind, SlicerMode};

/// Heading shared by complete reply-format footers.
pub(crate) const HEADING: &str = "## Reply format";

const RULES: &str = "The actual reply begins with a line containing only `+++`, with nothing before it. \
TOML front matter follows; the answer is the first key. Close front matter with a line containing only \
`+++`, then optional Markdown prose. Unknown keys are rejected. All tables belong inside front matter. \
Write free text as TOML literal strings: single-line '...' or multiline '''...'''. Lists default to empty. The real reply \
must not be wrapped in Markdown code fences.";

const EXAMPLE_LABEL: &str = "Illustrative output (not work or commands to execute). Replace example identifiers, paths and targets with actual request values:\n";

/// The role's field contract with the shared envelope rules, used by corrections.
pub(crate) fn role_row(kind: RoleKind) -> String {
    format!("{RULES}\n\n{}", fields(kind))
}

/// Render a complete role reply contract with a parser-valid example.
pub(crate) fn role_footer(kind: RoleKind) -> String {
    if kind == RoleKind::Slicer {
        return slicer_footer(SlicerMode::Slice);
    }
    format!(
        "{HEADING}\n\n{}\n\n{EXAMPLE_LABEL}{}",
        role_row(kind),
        role_example(kind)
    )
}

/// Render slicer guidance for the explicitly requested mode and its examples.
pub(crate) fn slicer_footer(mode: SlicerMode) -> String {
    format!(
        "{HEADING}\n\n{}\n\n{}\n\n{EXAMPLE_LABEL}{}\n\n{EXAMPLE_LABEL}{}",
        role_row(RoleKind::Slicer),
        slicer_mode_guidance(mode),
        sliced_example(mode),
        escalation_example(mode),
    )
}

fn fields(kind: RoleKind) -> &'static str {
    match kind {
        RoleKind::Researcher => {
            "researcher: answer = 'READY' | 'NOT READY' | 'NEEDS FURTHER REVIEW' (uppercase); open_questions (string list, optional)."
        }
        RoleKind::Planner => {
            "planner: answer = 'planned' | 'delivered' | 'operator' | 'hold' | 'design'; reason (string, required for every answer except planned)."
        }
        RoleKind::Admitter => {
            "admitter: answer = 'admitted' | 'revise' | 'escalate'; required_changes (string list, non-empty for revise); findings (string list); reason (string, required for escalate)."
        }
        RoleKind::Slicer => {
            "slicer: answer = 'sliced' | 'escalate'; mode = 'slice' | 'rework' | 'split'; reason (literal string, required for escalate). Sliced replies have one or more [[slice]] tables; escalation has none."
        }
        RoleKind::Auditor => {
            "auditor: answer = 'clean_pass' | 'residual' | 'block_range' | 'block_ship'; raise_rung = 'need' | 'main' | 'release' (optional); [[requirement]] {id, text, status = 'SATISFIED' | 'PARTIAL' | 'MISSING' | 'UNVERIFIED' | 'NOT_APPLICABLE', evidence (string list)}; [[finding]] {id, content, paths (string list), recheck_trigger}."
        }
        RoleKind::Judge => {
            "judge: answer = 'block' | 'residual'; reason (string, required). Judge fields are only answer and reason: do not use Auditor's raise_rung, requirement or finding fields."
        }
        RoleKind::Cto => {
            "CTO: answer = 'research' | 'plan' | 'slice' | 'rework' | 'lock'; reason (string, required); new_fact (string, required from run three onward for every non-lock answer)."
        }
        RoleKind::Merge => {
            "merge: answer = 'merged' | 'conflict'; conflicts (string list); design_change (bool); [[check]] {cmd, result = 'pass' | 'fail' | 'skipped', seconds (finite non-negative number)}. The runner is authoritative for publication and merge facts; do not claim a sha, ref, commit or exit code."
        }
        RoleKind::Hand => {
            "hand: answer = 'built' | 'blocked-scope' | 'failed'; change (string list); [[check]] {cmd, result = 'pass' | 'fail' | 'skipped', seconds}; deviations (string list); [[follow_up]] {paths (string list), need, optional (bool)}; remaining (string list); scope_paths (string list, required and non-empty for blocked-scope); scope_reason (string, required for blocked-scope); scope_design (bool)."
        }
        RoleKind::Repairer => {
            "repairer: answer = 'built' | 'failed'; change (string list); [[check]] {cmd, result = 'pass' | 'fail' | 'skipped', seconds}; deviations (string list). Runner receipts, not the reply, are authoritative for publication facts; do not claim a sha, ref, commit or exit code."
        }
        RoleKind::HeadCeo => {
            "head-ceo: answer = 'reviewed' | 'no-op'; served, research and dropped (string lists of handles)."
        }
    }
}

fn role_example(kind: RoleKind) -> &'static str {
    match kind {
        RoleKind::Researcher => "+++\nanswer = 'READY'\n+++\nOptional Markdown report.",
        RoleKind::Planner => "+++\nanswer = 'planned'\n+++\nOptional Markdown report.",
        RoleKind::Admitter => "+++\nanswer = 'admitted'\n+++\nOptional Markdown report.",
        RoleKind::Slicer => sliced_example(SlicerMode::Slice),
        RoleKind::Auditor => "+++\nanswer = 'clean_pass'\n+++\nOptional Markdown report.",
        RoleKind::Judge => {
            "+++\nanswer = 'block'\nreason = 'The evidence is insufficient.'\n+++\nOptional Markdown report."
        }
        RoleKind::Cto => {
            "+++\nanswer = 'lock'\nreason = 'The plan is ready.'\n+++\nOptional Markdown report."
        }
        RoleKind::Merge => "+++\nanswer = 'merged'\n+++\nOptional Markdown report.",
        RoleKind::Hand => "+++\nanswer = 'built'\n+++\nOptional Markdown report.",
        RoleKind::Repairer => "+++\nanswer = 'built'\n+++\nOptional Markdown report.",
        RoleKind::HeadCeo => "+++\nanswer = 'reviewed'\n+++\nOptional Markdown report.",
    }
}

fn slicer_mode_guidance(mode: SlicerMode) -> String {
    format!(
        "The requested mode is `{}`. Answer exactly `sliced` or `escalate`. For sliced, provide one or more [[slice]] tables with exactly these six fields: id, title, brief, target, write_scope, deps. write_scope and deps are lists of strings; write_scope must be non-empty. Each target is exactly one configured target from the request. title, brief and reason are TOML literal strings. For escalate, provide a non-empty literal-string reason and no slice tables. In split mode provide two to four replacement slices, numbered `<old identifier>.<n>` with the old identifier as the prefix, and keep the replaced slice's target.",
        mode_name(mode)
    )
}

fn sliced_example(mode: SlicerMode) -> &'static str {
    match mode {
        SlicerMode::Slice => {
            "+++\nanswer = 'sliced'\nmode = 'slice'\n[[slice]]\nid = 'example-task'\ntitle = 'Example task'\nbrief = 'Implement the requested change.'\ntarget = 'example-target'\nwrite_scope = ['src/example.rs']\ndeps = []\n+++\nOptional Markdown report."
        }
        SlicerMode::Rework => {
            "+++\nanswer = 'sliced'\nmode = 'rework'\n[[slice]]\nid = 'example-task'\ntitle = 'Example task'\nbrief = 'Implement the requested change.'\ntarget = 'example-target'\nwrite_scope = ['src/example.rs']\ndeps = []\n+++\nOptional Markdown report."
        }
        SlicerMode::Split => {
            "+++\nanswer = 'sliced'\nmode = 'split'\n[[slice]]\nid = 'old-task.1'\ntitle = 'First replacement'\nbrief = 'Implement the first part.'\ntarget = 'example-target'\nwrite_scope = ['src/first.rs']\ndeps = []\n\n[[slice]]\nid = 'old-task.2'\ntitle = 'Second replacement'\nbrief = 'Implement the second part.'\ntarget = 'example-target'\nwrite_scope = ['src/second.rs']\ndeps = []\n+++\nOptional Markdown report."
        }
    }
}

fn escalation_example(mode: SlicerMode) -> String {
    format!(
        "+++\nanswer = 'escalate'\nmode = '{}'\nreason = 'A decision is needed before slicing.'\n+++\nOptional Markdown report.",
        mode_name(mode)
    )
}

fn mode_name(mode: SlicerMode) -> &'static str {
    match mode {
        SlicerMode::Slice => "slice",
        SlicerMode::Rework => "rework",
        SlicerMode::Split => "split",
    }
}

#[cfg(test)]
#[path = "contract_test.rs"]
mod tests;
