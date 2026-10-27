use super::*;
use crate::letter::fixture::need_body;
use crate::letter::parse_letter;

fn s(text: &str) -> String {
    text.to_string()
}

fn err(result: Result<Sugared, LetterError>) -> String {
    result.unwrap_err().to_string()
}

#[test]
fn need_flags_build_front_matter_with_the_body_as_prose() {
    let sugar = Sugar {
        done_when: vec![s("a"), s("b")],
        target: Some(s("vivi")),
        write_scope: vec![s("src/x.rs")],
        serves: Some(s("w1")),
        depends_on: vec![s("n1")],
        ..Sugar::default()
    };
    let sugared = apply("need", "do it", &sugar).unwrap();
    let parsed = parse_letter::<NeedFront>(&sugared.body).unwrap();
    assert_eq!(parsed.front.done_when, vec![s("a"), s("b")]);
    assert_eq!(parsed.front.target.as_deref(), Some("vivi"));
    assert_eq!(parsed.front.write_scope, vec![s("src/x.rs")]);
    assert_eq!(parsed.front.serves.as_deref(), Some("w1"));
    assert_eq!(parsed.front.depends_on, vec![s("n1")]);
    assert_eq!(parsed.body.trim(), "do it");
}

#[test]
fn need_with_flags_but_no_done_when_is_refused() {
    let sugar = Sugar {
        target: Some(s("x")),
        ..Sugar::default()
    };
    assert_eq!(
        err(apply("need", "prose", &sugar)),
        "need has no done_when: pass --done-when or add done_when = [...] to the front matter"
    );
}

#[test]
fn need_serving_and_exploring_is_refused_by_validation() {
    let sugar = Sugar {
        done_when: vec![s("a")],
        serves: Some(s("w1")),
        explores: Some(s("w2")),
        ..Sugar::default()
    };
    assert!(err(apply("need", "p", &sugar)).contains("never both"));
}

#[test]
fn task_flags_build_front_matter() {
    let sugar = Sugar {
        done_when: vec![s("d")],
        target: Some(s("t")),
        write_scope: vec![s("a"), s("b")],
        depends_on: vec![s("t1")],
        ..Sugar::default()
    };
    let sugared = apply("task", "prose", &sugar).unwrap();
    let parsed = parse_letter::<TaskFront>(&sugared.body).unwrap();
    assert_eq!(parsed.front.done_when, vec![s("d")]);
    assert_eq!(parsed.front.target.as_deref(), Some("t"));
    assert_eq!(parsed.front.write_scope, vec![s("a"), s("b")]);
    assert_eq!(parsed.front.depends_on, vec![s("t1")]);
}

#[test]
fn want_question_builds_front_matter() {
    let sugar = Sugar {
        question: Some(s("why")),
        depends_on: vec![s("w0")],
        ..Sugar::default()
    };
    let sugared = apply("want", "prose", &sugar).unwrap();
    let parsed = parse_letter::<WantFront>(&sugared.body).unwrap();
    assert_eq!(parsed.front.question, "why");
    assert_eq!(parsed.front.depends_on, vec![s("w0")]);
    assert_eq!(parsed.body.trim(), "prose");
}

#[test]
fn want_with_other_flags_but_no_question_is_refused() {
    let sugar = Sugar {
        target: Some(s("x")),
        ..Sugar::default()
    };
    assert_eq!(
        err(apply("want", "p", &sugar)),
        "want has no question: pass --question or add question = \"...\" to the front matter"
    );
}

#[test]
fn flags_that_do_not_fit_the_kind_are_refused() {
    let task = Sugar {
        serves: Some(s("w")),
        ..Sugar::default()
    };
    assert_eq!(
        err(apply("task", "p", &task)),
        "--serves does not apply to a task"
    );
    let want = Sugar {
        question: Some(s("q")),
        target: Some(s("x")),
        ..Sugar::default()
    };
    assert_eq!(
        err(apply("want", "p", &want)),
        "--target does not apply to a want"
    );
}

#[test]
fn whole_letter_is_taken_unchanged() {
    let body = need_body(&["done"], "prose");
    let sugared = apply("need", &body, &Sugar::default()).unwrap();
    assert_eq!(sugared.body, body);
}

#[test]
fn whole_letter_refuses_every_flag_beside_it() {
    let body = need_body(&["done"], "prose");
    let cases = [
        (
            Sugar {
                done_when: vec![s("x")],
                ..Sugar::default()
            },
            "done_when",
            "done-when",
        ),
        (
            Sugar {
                depends_on: vec![s("x")],
                ..Sugar::default()
            },
            "depends_on",
            "depends-on",
        ),
        (
            Sugar {
                write_scope: vec![s("x")],
                ..Sugar::default()
            },
            "write_scope",
            "write-scope",
        ),
    ];
    for (sugar, key, flag) in cases {
        assert_eq!(
            err(apply("need", &body, &sugar)),
            format!(
                "the body is a whole letter (it opens with +++): put {key} in its front matter instead of passing --{flag}"
            )
        );
    }
}

#[test]
fn whole_letter_need_without_done_when_is_refused_with_the_ruling_text() {
    for body in [
        "+++\ntarget = \"x\"\n+++\n\nprose",
        "+++\ndone_when = []\n+++\n\nprose",
    ] {
        assert_eq!(
            err(apply("need", body, &Sugar::default())),
            "need has no done_when: pass --done-when or add done_when = [...] to the front matter"
        );
    }
}

#[test]
fn whole_letter_want_without_question_is_refused() {
    assert_eq!(
        err(apply("want", "+++\n+++\n\nprose", &Sugar::default())),
        "want has no question: pass --question or add question = \"...\" to the front matter"
    );
}

#[test]
fn whole_letter_with_broken_front_matter_names_the_fence_problem() {
    assert!(
        err(apply(
            "need",
            "+++\ndone_when = [\"a\"]\n",
            &Sugar::default()
        ))
        .contains("not closed")
    );
}

#[test]
fn prose_with_only_depends_on_builds_the_front_matter() {
    let sugar = Sugar {
        depends_on: vec![s("n1")],
        ..Sugar::default()
    };
    let sugared = apply("task", "prose", &sugar).unwrap();
    let parsed = parse_letter::<TaskFront>(&sugared.body).unwrap();
    assert_eq!(parsed.front.depends_on, vec![s("n1")]);
    assert_eq!(parsed.body.trim(), "prose");
}

#[test]
fn a_need_with_only_depends_on_is_told_to_pass_done_when() {
    let sugar = Sugar {
        depends_on: vec![s("n1")],
        ..Sugar::default()
    };
    let err = apply("need", "prose", &sugar).unwrap_err().to_string();
    assert!(err.contains("need has no done_when"), "{err}");
}

#[test]
fn a_want_with_only_depends_on_is_told_to_pass_question() {
    let sugar = Sugar {
        depends_on: vec![s("n1")],
        ..Sugar::default()
    };
    let err = apply("want", "prose", &sugar).unwrap_err().to_string();
    assert!(err.contains("want has no question"), "{err}");
}

#[test]
fn bare_prose_task_is_wrapped_in_empty_front_matter() {
    let sugared = apply("task", "prose", &Sugar::default()).unwrap();
    assert!(opens_with_fence(&sugared.body), "{}", sugared.body);
    let parsed = parse_letter::<TaskFront>(&sugared.body).unwrap();
    assert_eq!(parsed.body.trim(), "prose");
}

#[test]
fn bare_prose_need_is_refused_with_the_ruling_text() {
    let err = err(apply("need", "prose", &Sugar::default()));
    assert!(err.contains("need has no done_when"), "{err}");
}

#[test]
fn bare_prose_want_is_refused_with_the_want_text() {
    let err = err(apply("want", "prose", &Sugar::default()));
    assert!(err.contains("want has no question"), "{err}");
}

#[test]
fn opens_with_fence_skips_blank_lines() {
    assert!(opens_with_fence("\n\n+++\nx\n+++\n"));
    assert!(!opens_with_fence("prose\n+++\n"));
}
