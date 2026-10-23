use super::*;
use crate::letter::fixture::{
    need_body, need_body_with, sentinel_body, step_body, task_body, want_body,
};

#[test]
fn front_matter_need_want_task_parse() {
    let need = need_of("n1", &need_body(&["a", "b"], "prose")).unwrap();
    assert_eq!(need.front.done_when, vec!["a", "b"]);
    assert_eq!(need.body, "prose");

    let want = want_of("w1", &want_body("why?", "wprose")).unwrap();
    assert_eq!(want.front.question, "why?");
    assert_eq!(want.body, "wprose");

    let task = task_of("t1", &task_body("tprose")).unwrap();
    assert_eq!(task.front, TaskFront::default());
    assert_eq!(task.body, "tprose");
}

#[test]
fn leading_blank_lines_before_the_fence_still_parse() {
    let body = format!("\n\n{}", need_body(&["a"], "p"));
    assert_eq!(need_of("n", &body).unwrap().front.done_when, vec!["a"]);
}

#[test]
fn invalid_front_matter_is_an_error() {
    let blank = need_body_with(&NeedFront::new(vec!["  ".to_string()]), "p");
    assert!(matches!(need_of("n", &blank), Err(LetterError::Invalid(_))));
    assert!(matches!(
        want_of("w", "+++\nquestion = \"q\"\nbogus = 1\n+++\nbody"),
        Err(LetterError::Toml { .. })
    ));
    assert_eq!(
        task_of("t", "+++\nband = \"P1\"\n"),
        Err(LetterError::Unclosed)
    );
}

#[test]
fn step_of_reads_step_sentinel_plain_and_need() {
    let step = step_of(&step_body("ab12", 3, "plan", "p")).unwrap();
    assert_eq!(step.need, "ab12");
    assert_eq!(step.run, 3);
    assert_eq!(step.node.as_deref(), Some("plan"));
    assert!(!step.sentinel);

    let sentinel = step_of(&sentinel_body("ab12", 2, "p")).unwrap();
    assert!(sentinel.sentinel);
    assert_eq!(sentinel.node, None);
    assert_eq!(sentinel.run, 2);

    assert_eq!(step_of(&task_body("p")), None);
    assert_eq!(step_of(&need_body(&["a"], "p")), None);
}

#[test]
fn legacy_prose_bodies_are_refused_naming_the_handle() {
    for (kind, got) in [
        ("need", need_of("n9", "done_when: ships\nfor: <w1>\n").err()),
        ("want", want_of("n9", "a question").err()),
        (
            "task",
            task_of("n9", "pump-step: n1 r2 plan\nfrom: main\n").err(),
        ),
    ] {
        assert_eq!(
            got,
            Some(LetterError::Legacy {
                handle: "n9".to_string(),
                kind: kind.to_string(),
            })
        );
    }
    assert!(
        task_of("n9", "x")
            .unwrap_err()
            .to_string()
            .contains("letter n9")
    );
    assert_eq!(step_of("pump-step: n1 r2 plan"), None);
}
