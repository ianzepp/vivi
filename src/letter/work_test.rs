use super::*;
use crate::letter::{LetterError, parse, render};

fn s(text: &str) -> String {
    text.to_string()
}

fn full_step() -> StepFront {
    StepFront {
        need: s("ab12cd34"),
        run: 3,
        node: Some(s("build")),
        sentinel: false,
    }
}

fn full_need() -> NeedFront {
    NeedFront {
        done_when: vec![s("tests pass"), s("docs updated")],
        target: Some(s("vivi")),
        depends_on: vec![s("aa11"), s("bb22")],
        write_scope: vec![s("src/a.rs"), s("src/b.rs")],
        priority: Some(-4),
        serves: Some(s("cc33")),
        explores: None,
    }
}

fn full_task() -> TaskFront {
    TaskFront {
        done_when: vec![s("it builds")],
        depends_on: vec![s("dd44")],
        parent: Some(s("ee55")),
        target: Some(s("vivi")),
        write_scope: vec![s("src/x.rs")],
        from: Some(s("main")),
        branch: Some(s("w3/x")),
        with: vec![s("one"), s("two")],
        band: Some(s("P1")),
        step: Some(full_step()),
    }
}

fn round<T>(front: &T) -> T
where
    T: serde::Serialize + serde::de::DeserializeOwned + Validate,
{
    let text = render(front, "the prose").unwrap();
    let envelope = parse::<T>(&text).unwrap();
    assert_eq!(envelope.body, "the prose");
    envelope.front
}

fn invalid<T: serde::de::DeserializeOwned + Validate>(front: &str) -> String {
    match parse::<T>(&format!("+++\n{front}+++\n\nbody")) {
        Err(LetterError::Invalid(message)) => message,
        other => panic!("expected Invalid, got {:?}", other.map(|e| e.body)),
    }
}

fn toml_error<T: serde::de::DeserializeOwned + Validate>(front: &str) -> String {
    match parse::<T>(&format!("+++\n{front}+++\n\nbody")) {
        Err(LetterError::Toml { message, .. }) => message,
        other => panic!("expected Toml, got {:?}", other.map(|e| e.body)),
    }
}

#[test]
fn need_round_trips_full_and_minimal() {
    let full = full_need();
    assert_eq!(round(&full), full);
    let minimal = NeedFront {
        done_when: vec![s("done")],
        ..NeedFront::default()
    };
    assert_eq!(round(&minimal), minimal);
    let text = render(&minimal, "p").unwrap();
    assert_eq!(text, "+++\ndone_when = [\"done\"]\n+++\n\np");
}

#[test]
fn want_round_trips_full_and_minimal() {
    let full = WantFront {
        question: s("why is it slow?"),
        depends_on: vec![s("aa11")],
    };
    assert_eq!(round(&full), full);
    let minimal = WantFront {
        question: s("why?"),
        depends_on: Vec::new(),
    };
    assert_eq!(round(&minimal), minimal);
}

#[test]
fn task_round_trips_full_and_minimal() {
    let full = full_task();
    assert_eq!(round(&full), full);
    let minimal = TaskFront::default();
    assert_eq!(round(&minimal), minimal);
    let sentinel = TaskFront {
        step: Some(StepFront {
            need: s("ab12"),
            run: 1,
            node: None,
            sentinel: true,
        }),
        ..TaskFront::default()
    };
    assert_eq!(round(&sentinel), sentinel);
}

#[test]
fn render_puts_tables_last() {
    let text = render(&full_task(), "p").unwrap();
    let band = text.find("band = ").unwrap();
    let step = text.find("[step]").unwrap();
    assert!(band < step);
}

#[test]
fn unknown_keys_are_refused() {
    assert!(toml_error::<NeedFront>("done_when = [\"x\"]\nbogus = 1\n").contains("bogus"));
    assert!(toml_error::<WantFront>("question = \"q\"\nbogus = 1\n").contains("bogus"));
    assert!(toml_error::<TaskFront>("bogus = 1\n").contains("bogus"));
    assert!(
        toml_error::<TaskFront>("[step]\nneed = \"a\"\nrun = 1\nnode = \"n\"\nbogus = 1\n")
            .contains("bogus")
    );
}

#[test]
fn missing_required_keys_are_refused() {
    assert!(toml_error::<NeedFront>("target = \"vivi\"\n").contains("done_when"));
    assert!(toml_error::<WantFront>("depends_on = [\"a\"]\n").contains("question"));
    assert!(toml_error::<TaskFront>("[step]\nrun = 1\nnode = \"n\"\n").contains("need"));
    assert!(toml_error::<TaskFront>("[step]\nneed = \"a\"\nnode = \"n\"\n").contains("run"));
}

#[test]
fn need_rules_name_the_key() {
    assert!(invalid::<NeedFront>("done_when = []\n").contains("done_when"));
    assert!(invalid::<NeedFront>("done_when = [\"a\", \" \"]\n").contains("done_when"));
    assert!(invalid::<NeedFront>("done_when = [\"a\"]\ntarget = \" \"\n").contains("target"));
    assert!(invalid::<NeedFront>("done_when = [\"a\"]\nserves = \"\"\n").contains("serves"));
    assert!(
        invalid::<NeedFront>("done_when = [\"a\"]\ndepends_on = [\"\"]\n").contains("depends_on")
    );
    assert!(
        invalid::<NeedFront>("done_when = [\"a\"]\nwrite_scope = [\"\"]\n").contains("write_scope")
    );
}

#[test]
fn want_rules_name_the_key() {
    assert!(invalid::<WantFront>("question = \"  \"\n").contains("question"));
    assert!(invalid::<WantFront>("question = \"q\"\ndepends_on = [\"\"]\n").contains("depends_on"));
}

#[test]
fn task_rules_name_the_key() {
    assert!(invalid::<TaskFront>("done_when = [\"\"]\n").contains("done_when"));
    assert!(invalid::<TaskFront>("parent = \" \"\n").contains("parent"));
    assert!(invalid::<TaskFront>("band = \"\"\n").contains("band"));
    assert!(invalid::<TaskFront>("with = [\"\"]\n").contains("with"));
}

#[test]
fn step_rules_name_the_key() {
    let no_node = invalid::<TaskFront>("[step]\nneed = \"a\"\nrun = 1\n");
    assert!(no_node.contains("step.node"));
    let blank_node = invalid::<TaskFront>("[step]\nneed = \"a\"\nrun = 1\nnode = \" \"\n");
    assert!(blank_node.contains("step.node"));
    let sentinel_node =
        invalid::<TaskFront>("[step]\nneed = \"a\"\nrun = 1\nnode = \"n\"\nsentinel = true\n");
    assert!(sentinel_node.contains("step.node"));
    let blank_need = invalid::<TaskFront>("[step]\nneed = \"\"\nrun = 1\nnode = \"n\"\n");
    assert!(blank_need.contains("step.need"));
}

#[test]
fn explores_round_trips_and_is_refused_with_serves() {
    let mut need = NeedFront::new(vec![s("answered")]);
    need.explores = Some(s("ff66"));
    assert_eq!(round(&need), need);
    let both =
        invalid::<NeedFront>("done_when = [\"a\"]\nserves = \"cc33\"\nexplores = \"ff66\"\n");
    assert!(
        both.contains("`serves`") && both.contains("`explores`"),
        "{both}"
    );
    assert!(invalid::<NeedFront>("done_when = [\"a\"]\nexplores = \" \"\n").contains("explores"));
}

#[test]
fn constructors_set_only_their_fields() {
    let need = NeedFront::new(vec![s("x")]);
    assert_eq!(
        need,
        NeedFront {
            done_when: vec![s("x")],
            ..NeedFront::default()
        }
    );
    assert_eq!(
        TaskFront::step("ab12", 2, "build"),
        TaskFront {
            step: Some(StepFront {
                need: s("ab12"),
                run: 2,
                node: Some(s("build")),
                sentinel: false,
            }),
            ..TaskFront::default()
        }
    );
    let sentinel = TaskFront::sentinel("ab12", 2);
    assert_eq!(
        sentinel.step,
        Some(StepFront {
            need: s("ab12"),
            run: 2,
            node: None,
            sentinel: true,
        })
    );
    assert!(sentinel.validate().is_ok());
}

#[test]
fn fixture_bodies_parse_back() {
    use crate::letter::fixture;
    use crate::letter::parse_letter;
    let need = parse_letter::<NeedFront>(&fixture::body_for("need", "x")).unwrap();
    assert_eq!(need.front.done_when, ["done"]);
    assert_eq!(need.body, "x");
    let deps = [s("aa11")];
    let want = parse_letter::<WantFront>(&fixture::body_with_deps("want", "x", &deps)).unwrap();
    assert_eq!(want.front.depends_on, deps);
    assert_eq!(fixture::body_for("memo", "plain"), "plain");
    let step = parse_letter::<TaskFront>(&fixture::step_body("ab12", 1, "n", "p")).unwrap();
    assert_eq!(step.front, TaskFront::step("ab12", 1, "n"));
    let sentinel = parse_letter::<TaskFront>(&fixture::sentinel_body("ab12", 1, "p")).unwrap();
    assert_eq!(sentinel.front, TaskFront::sentinel("ab12", 1));
}
