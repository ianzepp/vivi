use super::*;

#[test]
fn done_when_and_write_scope_lines() {
    let body = "intro\n  Done_When: first one\ndone_when:second\nwrite_scope: src/a.rs\n  write_scope:  src/b.rs \ndone_whenx: no\n";
    let need = need(body);
    assert_eq!(need.front.done_when, vec!["first one", "second"]);
    assert_eq!(need.front.write_scope, vec!["src/a.rs", "src/b.rs"]);
    assert_eq!(need.body, body.trim());
    let task = task(body);
    assert_eq!(task.front.done_when, vec!["first one", "second"]);
    assert_eq!(task.front.write_scope, vec!["src/a.rs", "src/b.rs"]);
}

#[test]
fn explores_takes_the_first_for_line() {
    assert_eq!(
        need("for: w1\nfor: w2").front.explores.as_deref(),
        Some("w1")
    );
    assert_eq!(need("  for: <w9>").front.explores.as_deref(), Some("w9"));
    assert_eq!(need("for: `w3`").front.explores.as_deref(), Some("w3"));
    assert_eq!(
        need("for: <>\nfor: w4").front.explores.as_deref(),
        Some("w4")
    );
    assert_eq!(need("nothing here").front.explores, None);
}

#[test]
fn step_markers() {
    let step = task("\n  pump-step: n1 r3 plan\nmore").front.step.unwrap();
    assert_eq!(
        step,
        StepFront {
            need: "n1".to_string(),
            run: 3,
            node: Some("plan".to_string()),
            sentinel: false,
        }
    );
    let bad_run = task("pump-step: n1 rX plan").front.step.unwrap();
    assert_eq!(bad_run.run, 0);
    assert_eq!(bad_run.node.as_deref(), Some("plan"));
    let no_node = task("pump-step: n1 r4").front.step.unwrap();
    assert_eq!(no_node.node, None);
    let sentinel = task("pump-run-sentinel: n2 r5\nx").front.step.unwrap();
    assert_eq!(
        sentinel,
        StepFront {
            need: "n2".to_string(),
            run: 5,
            node: None,
            sentinel: true,
        }
    );
    assert_eq!(task("pump-step:").front.step, None);
    assert_eq!(task("pump-run-sentinel:   ").front.step, None);
    assert_eq!(task("text\npump-step: n1 r1 x").front.step, None);
}

#[test]
fn target_line_is_not_trimmed_first() {
    assert_eq!(task("target: vivi\n").front.target.as_deref(), Some("vivi"));
    assert_eq!(task("  target: vivi").front.target, None);
    assert_eq!(task("target:   \ntarget: b").front.target, None);
}

#[test]
fn ref_lines_from_branch_with_band() {
    let body = "- from: main\n* branch: w3/x\nfrom: other\nbranch: y\n  band: P1-S2\nband: P3\nwith: a\n- with: b\nWITH: c\nFrom: ignored\n";
    let front = task(body).front;
    assert_eq!(front.from.as_deref(), Some("main"));
    assert_eq!(front.branch.as_deref(), Some("w3/x"));
    assert_eq!(front.band.as_deref(), Some("P1-S2"));
    assert_eq!(front.with, vec!["a", "b", "c"]);
}

#[test]
fn values_with_spaces_or_empty_are_skipped() {
    let front = task("from: two words\nfrom: \nfrom: real\nband: P1 fast\nband: P2\n").front;
    assert_eq!(front.from.as_deref(), Some("real"));
    assert_eq!(front.band.as_deref(), Some("P2"));
}

#[test]
fn want_has_an_empty_question() {
    let want = want("  some prose \n");
    assert_eq!(want.front.question, "");
    assert_eq!(want.body, "some prose");
}
