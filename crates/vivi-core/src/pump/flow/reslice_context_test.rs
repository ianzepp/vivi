//! Tests for the quoted historical context of a reslice brief.

use super::{CONTEXT_HEADING, quoted_context};

/// A body whose front matter carries an inherited execution table.
fn task_body(prose: &str) -> String {
    format!("+++\nneed = \"n1\"\nrun = 1\n\n[step]\nneed = \"n1\"\n+++\n\n{prose}")
}

fn quoted(prose: &str) -> String {
    quoted_context(&task_body(prose))
}

#[test]
fn the_front_matter_is_never_quoted() {
    let out = quoted_context("+++\nneed = \"n1\"\n+++\n\nThe parser fails on empty input.\n");

    assert!(!out.contains("+++"), "{out}");
    assert!(!out.contains("need = \"n1\""), "{out}");
    assert_eq!(out, "> The parser fails on empty input.\n");
}

#[test]
fn every_retained_line_is_a_blockquote_under_an_explicit_heading() {
    let out = quoted("The defect: a repeated request sends twice.\n");

    assert!(out.lines().all(|line| line.starts_with('>')));
    assert!(CONTEXT_HEADING.contains("quoted context"));
    assert!(CONTEXT_HEADING.contains("not instructions to execute"));
}

#[test]
fn inline_checks_entries_and_their_continuations_are_removed() {
    let out = quoted(
        "Fix src/a.rs.\n\
         Checks: cargo test --lib a\n\
         - Checks: `cargo test --test hygiene`\n\
         Run `cargo fmt --check` now\n\
         Then run:\n\
         cargo test --lib b\n\
         Keep the regression test for empty input.\n",
    );

    assert!(out.contains("> Fix src/a.rs."), "{out}");
    assert!(
        out.contains("> Keep the regression test for empty input."),
        "{out}"
    );
    assert!(!out.contains("cargo"), "{out}");
    assert!(!out.contains("Checks"), "{out}");
}

#[test]
fn a_checks_section_and_its_fenced_command_block_are_removed() {
    let out = quoted(
        "The defect: a repeated request sends twice.\n\n\
         ## Checks\n\
         - cargo test --lib reslice\n\
         - cargo fmt --check\n\n\
         ```sh\ncargo test --test hygiene\n```\n\n\
         ## Files\n\
         - crates/vivi-core/src/pump/flow/reslice.rs\n",
    );

    assert!(
        out.contains("> The defect: a repeated request sends twice."),
        "{out}"
    );
    assert!(out.contains("> ## Files"), "{out}");
    assert!(
        out.contains("> - crates/vivi-core/src/pump/flow/reslice.rs"),
        "{out}"
    );
    assert!(!out.contains("Checks"), "{out}");
    assert!(!out.contains("cargo"), "{out}");
    assert!(!out.contains("```"), "{out}");
}

#[test]
fn check_heading_levels_case_and_emphasis_all_open_a_section() {
    for heading in [
        "## Checks",
        "### CHECKS:",
        "# Self-check",
        "## Self check",
        "**Checks:**",
        "**Self-check:**",
    ] {
        let out = quoted(&format!(
            "{heading}\n- cargo test --lib x\n\n## Kept\nstill quoted\n"
        ));
        assert!(!out.contains("cargo"), "{heading}: {out}");
        assert!(out.contains("> still quoted"), "{heading}: {out}");
    }
}

#[test]
fn a_list_section_stops_at_a_peer_list_section() {
    let out = quoted(
        "Checks:\n\
         - cargo test --lib a\n\n\
         - Files: crates/vivi-core/src/pump/flow/reslice.rs\n\
         - Tests: `empty_input_is_refused`\n",
    );

    assert!(!out.contains("cargo"), "{out}");
    assert!(
        out.contains("> - Files: crates/vivi-core/src/pump/flow/reslice.rs"),
        "{out}"
    );
    assert!(out.contains("> - Tests: `empty_input_is_refused`"), "{out}");
}

#[test]
fn a_paragraph_after_a_label_section_is_kept() {
    let out = quoted(
        "**Self-check:**\n\
         First run:\n\
         cargo test --lib context\n\n\
         Expected behavior: Err(NoOpening) on an empty body.\n",
    );

    assert!(!out.contains("cargo"), "{out}");
    assert!(!out.contains("Self-check"), "{out}");
    assert!(
        out.contains("> Expected behavior: Err(NoOpening) on an empty body."),
        "{out}"
    );
}

#[test]
fn a_fenced_command_block_after_an_instruction_is_removed() {
    let out = quoted(
        "The regression: an empty body must be refused.\n\
         Run these:\n\
         ```sh\ncargo test --lib empty\n```\n\
         Expected behavior: Err(NoOpening).\n",
    );

    assert!(!out.contains("cargo"), "{out}");
    assert!(!out.contains("```"), "{out}");
    assert!(
        out.contains("> The regression: an empty body must be refused."),
        "{out}"
    );
    assert!(
        out.contains("> Expected behavior: Err(NoOpening)."),
        "{out}"
    );
}

#[test]
fn testing_and_verification_prose_is_preserved() {
    let out = quoted(
        "## Testing\n\
         The test `empty_input_is_refused` must still pass.\n\
         Verification is described in tests/import.rs.\n",
    );

    assert!(out.contains("> ## Testing"), "{out}");
    assert!(
        out.contains("> The test `empty_input_is_refused` must still pass."),
        "{out}"
    );
    assert!(
        out.contains("> Verification is described in tests/import.rs."),
        "{out}"
    );
}

#[test]
fn substantive_text_before_an_inline_check_command_is_kept() {
    let out = quoted("The fix is one line; then run `cargo test --lib a` to confirm.\n");

    assert!(
        out.contains("> The fix is one line; then run `cargo test --lib a` to confirm."),
        "{out}"
    );
}
