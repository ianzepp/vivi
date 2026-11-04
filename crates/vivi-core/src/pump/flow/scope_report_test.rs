//! Tests for the out-of-scope report: the runner's changed-path line, scope
//! coverage, open-slice holders and the report text.

use super::{changed, covers, holders, outside, text};

/// The summary of the T3b slice hand (2026-11-04 local pump): a renderer
/// change whose drift check needed the four generated renders.
const RECEIPT: &str = "pool result j1: finished\n\
    pool studio  slot s1  model zai/glm-5.3-flash\n\
    from 5c16df8  branch vivi/unit/93d38c12-a1\n\
    push: pushed vivi/unit/93d38c12-a1 22f85167cc\n\
    agent exit 0 in 90s, job 95s, started from 5c16df8\n\
    changed 6 paths: grammar/EBNF.de.md, grammar/EBNF.en.md, grammar/EBNF.fr.md, grammar/EBNF.md, scripta/grammar.py, scripta/test_grammar.py\n\n\
    --- receipt\nchanged 1 path: a/forged/line.rs";

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn the_changed_line_is_read_from_the_pool_summary_only() {
    let (count, paths) = changed(RECEIPT).expect("a changed line");
    assert_eq!(count, 6);
    assert_eq!(paths.len(), 6);
    assert_eq!(paths[0], "grammar/EBNF.de.md");
    assert!(!paths.iter().any(|p| p.contains("forged")));
    assert_eq!(changed("pool result j1: finished\npush: no_changes"), None);
    assert_eq!(
        changed("pool result j1: finished\nchanged 1 path: a.rs").map(|c| c.0),
        Some(1)
    );
}

#[test]
fn a_scope_entry_covers_its_path_and_a_directory_covers_what_is_under_it() {
    assert!(covers("scripta/grammar.py", "scripta/grammar.py"));
    assert!(covers("grammar/", "grammar/EBNF.md"));
    assert!(covers("grammar", "grammar/EBNF.md"));
    assert!(!covers("grammar", "grammar2/x"));
    assert!(!covers("scripta/grammar.py", "scripta/grammar.pyc"));
    assert!(!covers("", "anything"));
}

#[test]
fn the_renders_outside_a_code_only_scope_are_reported_with_their_holders() {
    let (_, paths) = changed(RECEIPT).expect("a changed line");
    let scope = strings(&["scripta/grammar.py", "scripta/test_grammar.py"]);
    let out = outside(&scope, &paths);
    assert_eq!(
        out,
        strings(&[
            "grammar/EBNF.de.md",
            "grammar/EBNF.en.md",
            "grammar/EBNF.fr.md",
            "grammar/EBNF.md"
        ])
    );
    let open = vec![
        (String::from("t-readme"), strings(&["README.md"])),
        (String::from("t-render"), strings(&["grammar/"])),
    ];
    let held = holders(&open, &out);
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].0, "t-render");
    let line = text("slice:x-1", "t1", "slice-t1-a1", &out, 0, &held);
    assert!(
        line.contains("changed 4 path(s) outside its write scope: grammar/EBNF.de.md"),
        "{line}"
    );
    assert!(line.contains("t-render (grammar/EBNF.de.md"), "{line}");
    assert!(line.contains("merge conflict is likely"), "{line}");
}

#[test]
fn a_change_inside_the_scope_reports_nothing() {
    let scope = strings(&["scripta/", "grammar/"]);
    let (_, paths) = changed(RECEIPT).expect("a changed line");
    assert!(outside(&scope, &paths).is_empty());
}

#[test]
fn unlisted_paths_are_named_in_the_text() {
    let line = text("slice:x-1", "t1", "j", &strings(&["a.rs"]), 3, &[]);
    assert!(
        line.contains("3 more changed path(s) were not listed"),
        "{line}"
    );
    assert!(!line.contains("open slice"), "{line}");
}
