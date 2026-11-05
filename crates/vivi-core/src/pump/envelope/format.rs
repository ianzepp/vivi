//! Shared envelope prompt formatting.

use vivi_mail::letter::roles::{RoleKind, SlicerMode};

const RULES: &str = "The actual reply starts immediately with an opening `+++` line and closes its TOML front matter with a matching `+++` line. The `answer` is the first key. Permitted prose follows the envelope. No Markdown preamble or code-fence wrapper surrounds the actual reply. Unknown keys are refused. Free text uses TOML literal strings `'...'`, including multiline literal strings `'''...'''`. Lists default to empty. Examples illustrate syntax only; they are non-evidentiary and must not be copied as findings, checks, or work performed.";

/// Render shared reply rules, fields and a minimal illustrative example.
pub(crate) fn role(kind: RoleKind) -> String {
    let (fields, example) = role_content(kind);
    footer(fields, example)
}

/// Render mode-specific slicer guidance for a dispatched request.
///
/// `id` is the original parent ID for split mode and a complete illustrative
/// slice ID for slice and rework modes.
pub(crate) fn slicer(mode: SlicerMode, target: &str, id: &str) -> String {
    let mode_name = mode_name(mode);
    let id_note = if mode == SlicerMode::Split {
        "The supplied id is the original parent ID; replacement IDs are `<id>.1`, `<id>.2`, and so on."
    } else {
        "The supplied id is already a complete illustrative slice ID."
    };
    let fields = format!(
        "slicer: answer = `sliced` | `escalate`; requested mode = `{mode_name}`. Set `mode` to exactly `{mode_name}`; do not substitute another mode. For `sliced`, include at least one `[[slice]]`; for `escalate`, include no slice rows and a nonblank literal `reason`. Each slice has exactly these six keys: `id`, `title`, `brief`, `target`, `write_scope`, `deps`. `title` and `brief` are literal free-text strings; `write_scope` is nonempty; every dependency is expressed as a slice ID. {id_note} Keep the supplied target `{target}` on every slice. Slice/rework describes one slice; split permits two to four replacement slices on the original target. Paths and descriptions below are illustrative."
    );
    let example = slicer_example(mode, target, id);
    let escalation = format!(
        "Example escalation (illustrative only):\n+++\nanswer = \"escalate\"\nmode = \"{mode_name}\"\nreason = 'Cannot safely assign the requested work to slices.'\n+++"
    );
    format!("{}\n\n{}\n\n{}\n\n{}", RULES, fields, example, escalation)
}

/// Render the narrowed auditor contract used after landed repairs.
pub(crate) fn repair_audit() -> String {
    let fields = "repair-audit auditor: answer = `clean_pass` | `residual`; repeated `[[finding]]` rows are optional and have exactly `id`, `content`, `paths`, `recheck_trigger`. Do not provide or invent a requirement inventory. A weakened-test finding ID starts with `weakened-test` (for example, `weakened-test-example`). Finding content, paths and trigger are literal free text where applicable.";
    let clean = "Example clean pass (illustrative only):\n+++\nanswer = \"clean_pass\"\n+++";
    let residual = "Example residual (illustrative only):\n+++\nanswer = \"residual\"\n[[finding]]\nid = 'weakened-test-example'\ncontent = 'A test was weakened and needs restoration.'\npaths = ['crates/example/src/example_test.rs']\nrecheck_trigger = 'Run the focused test after restoring its assertion.'\n+++";
    format!("{RULES}\n\n{fields}\n\n{clean}\n\n{residual}")
}

fn footer(fields: &str, example: &str) -> String {
    format!("{RULES}\n\n{fields}\n\nExample (illustrative only):\n+++\n{example}\n+++")
}

fn role_content(kind: RoleKind) -> (&'static str, &'static str) {
    match kind {
        RoleKind::Researcher => (
            "researcher: answer = `READY` | `NOT READY` | `NEEDS FURTHER REVIEW` (uppercase exactly); `open_questions` is a list.",
            "answer = \"READY\"\nopen_questions = []",
        ),
        RoleKind::Planner => (
            "planner: answer = `planned` | `delivered` | `operator` | `hold` | `design`; literal `reason` is required unless answer is `planned`.",
            "answer = \"delivered\"\nreason = 'The requested work is complete.'",
        ),
        RoleKind::Admitter => (
            "admitter: answer = `admitted` | `revise` | `escalate`; `required_changes` must be nonempty for `revise`; `findings` is a list; literal `reason` is required for `escalate`.",
            "answer = \"revise\"\nrequired_changes = ['Clarify the expected output.']\nfindings = []",
        ),
        RoleKind::Slicer => (
            "slicer correction: answer = `sliced` | `escalate`; preserve the original request's exact `mode` and supplied `target`—do not change either. `mode` is `slice` | `rework` | `split`; each `[[slice]]` has exactly `id`, `title`, `brief`, `target`, `write_scope`, `deps`; title/brief/reason are literal free text, write_scope is nonempty, and deps are slice IDs. At least one slice is required for `sliced`; `escalate` requires a nonblank literal reason and no slices. Split has two to four replacement slices on the original target, numbered `<parent-id>.<n>`. Paths and descriptions are illustrative.",
            "answer = \"sliced\"\nmode = \"slice\"\n[[slice]]\nid = 'slice-example'\ntitle = 'Illustrative slice'\nbrief = 'Implement one bounded part of the supplied work.'\ntarget = \"vivi\"\nwrite_scope = ['crates/example/src/lib.rs']\ndeps = []",
        ),
        RoleKind::Auditor => (
            "auditor: answer = `clean_pass` | `residual` | `block_range` | `block_ship`; optional `raise_rung` = `need` | `main` | `release`. Repeated `[[requirement]]` rows have `id`, literal `text`, `status` = `SATISFIED` | `PARTIAL` | `MISSING` | `UNVERIFIED` | `NOT_APPLICABLE`, and `evidence` (list). Repeated `[[finding]]` rows have `id`, literal `content`, `paths` (list), and literal `recheck_trigger`.",
            "answer = \"residual\"\nraise_rung = \"need\"\n[[requirement]]\nid = 'R1'\ntext = 'The illustrative requirement.'\nstatus = \"PARTIAL\"\nevidence = ['Focused check remains.']\n[[finding]]\nid = 'F1'\ncontent = 'An illustrative residual remains.'\npaths = ['crates/example/src/lib.rs']\nrecheck_trigger = 'Re-run the focused check.'",
        ),
        RoleKind::Judge => (
            "judge: answer = `block` | `residual`; literal `reason` is required. Judge replies have no auditor requirement or finding tables.",
            "answer = \"residual\"\nreason = 'The stated evidence leaves a residual concern.'",
        ),
        RoleKind::Cto => (
            "head-cto: answer = `research` | `plan` | `slice` | `rework` | `lock`; literal `reason` is always required. From run three onward, every non-`lock` answer requires `new_fact`; include it in this example so the example is valid for a late run.",
            "answer = \"plan\"\nreason = 'A plan is the next step.'\nnew_fact = 'The available evidence supports planning.'",
        ),
        RoleKind::Merge => (
            "merge: answer = `merged` | `conflict`; optional `conflicts` (path list), `design_change` (bool), and repeated `[[check]]` rows (`cmd`, `result` = `pass` | `fail` | `skipped`, finite non-negative `seconds`). Do not report SHA, ref, commit, exit code, or publication facts; the runner receipt owns publication evidence.",
            "answer = \"conflict\"\nconflicts = ['crates/example/src/lib.rs']\ndesign_change = false\n[[check]]\ncmd = 'illustrative check not run'\nresult = \"skipped\"\nseconds = 0",
        ),
        RoleKind::Hand => (
            "hand: answer = `built` | `blocked-scope` | `failed`; `change`, `deviations`, `remaining` are lists; repeated `[[check]]` has `cmd`, `result` = `pass` | `fail` | `skipped`, finite non-negative `seconds`; repeated `[[follow_up]]` has `paths`, `need`, optional `optional` (bool). For `blocked-scope`, `scope_paths` must be nonempty and `scope_reason` nonblank; `scope_design` is bool.",
            "answer = \"built\"\nchange = []\ndeviations = []\nremaining = []",
        ),
        RoleKind::Repairer => (
            "repairer: answer = `built` | `failed`; `change` and `deviations` are lists; repeated `[[check]]` has `cmd`, `result` = `pass` | `fail` | `skipped`, finite non-negative `seconds`. Repairers do not have Hand-only scope, follow-up, or remaining fields. No SHA, ref, commit, or exit code.",
            "answer = \"built\"\nchange = []\ndeviations = []",
        ),
        RoleKind::HeadCeo => (
            "head-ceo: answer = `reviewed` | `no-op`; `served`, `research`, and `dropped` are lists of handles.",
            "answer = \"no-op\"\nserved = []\nresearch = []\ndropped = []",
        ),
    }
}

fn mode_name(mode: SlicerMode) -> &'static str {
    match mode {
        SlicerMode::Slice => "slice",
        SlicerMode::Rework => "rework",
        SlicerMode::Split => "split",
    }
}

fn slicer_example(mode: SlicerMode, target: &str, id: &str) -> String {
    let target = toml::Value::String(target.to_owned()).to_string();
    let ids = if mode == SlicerMode::Split {
        vec![format!("{id}.1"), format!("{id}.2")]
    } else {
        vec![id.to_owned()]
    };
    let mut example = format!(
        "Example sliced reply (illustrative only):\n+++\nanswer = \"sliced\"\nmode = \"{}\"",
        mode_name(mode)
    );
    for (index, slice_id) in ids.iter().enumerate() {
        let slice_id = toml::Value::String(slice_id.clone());
        example.push_str(&format!(
            "\n[[slice]]\nid = {slice_id}\ntitle = 'Illustrative slice {}'\nbrief = 'Implement one bounded part of the supplied work.'\ntarget = {target}\nwrite_scope = ['crates/example/src/part{}.rs']\ndeps = {}",
            index + 1,
            index + 1,
            if mode == SlicerMode::Split && index == 1 {
                format!("[{}]", toml::Value::String(format!("{id}.1")))
            } else {
                "[]".to_owned()
            }
        ));
    }
    example.push_str("\n+++");
    example
}

#[cfg(test)]
#[path = "format_test.rs"]
mod tests;
