//! Test-only letter bodies: well-formed front matter plus prose, so tests that
//! need a stored need, want or task body do not hand-write TOML.

use crate::letter::render;
use crate::letter::work::{NeedFront, TaskFront, WantFront};

/// A need body with the given done conditions.
pub(crate) fn need_body(done_when: &[&str], prose: &str) -> String {
    let done_when = done_when.iter().map(ToString::to_string).collect();
    need_body_with(&NeedFront::new(done_when), prose)
}

/// A need body from a full front.
pub(crate) fn need_body_with(front: &NeedFront, prose: &str) -> String {
    render(front, prose).unwrap()
}

/// A want body with the given question.
pub(crate) fn want_body(question: &str, prose: &str) -> String {
    let front = WantFront {
        question: question.to_string(),
        depends_on: Vec::new(),
    };
    render(&front, prose).unwrap()
}

/// A task body with empty front matter.
pub(crate) fn task_body(prose: &str) -> String {
    task_body_with(&TaskFront::default(), prose)
}

/// A task body from a full front.
pub(crate) fn task_body_with(front: &TaskFront, prose: &str) -> String {
    render(front, prose).unwrap()
}

/// A pump step task body.
pub(crate) fn step_body(need: &str, run: u32, node: &str, prose: &str) -> String {
    task_body_with(&TaskFront::step(need, run, node), prose)
}

/// A pump run sentinel task body.
pub(crate) fn sentinel_body(need: &str, run: u32, prose: &str) -> String {
    task_body_with(&TaskFront::sentinel(need, run), prose)
}

/// A body for `kind` (need, want or task) with default front matter; any other
/// kind returns `prose` unchanged.
pub(crate) fn body_for(kind: &str, prose: &str) -> String {
    match kind {
        "need" => need_body(&["done"], prose),
        "want" => want_body("q", prose),
        "task" => task_body(prose),
        _ => prose.to_string(),
    }
}

/// As `body_for`, with `depends_on` set to `deps`.
pub(crate) fn body_with_deps(kind: &str, prose: &str, deps: &[String]) -> String {
    match kind {
        "need" => {
            let mut front = NeedFront::new(vec!["done".to_string()]);
            front.depends_on = deps.to_vec();
            need_body_with(&front, prose)
        }
        "want" => {
            let front = WantFront {
                question: "q".to_string(),
                depends_on: deps.to_vec(),
            };
            render(&front, prose).unwrap()
        }
        "task" => {
            let front = TaskFront {
                depends_on: deps.to_vec(),
                ..TaskFront::default()
            };
            task_body_with(&front, prose)
        }
        _ => prose.to_string(),
    }
}
