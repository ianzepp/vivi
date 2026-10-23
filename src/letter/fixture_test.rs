//! Test-only letter bodies: well-formed front matter plus prose, so tests that
//! need a stored need, want or task body do not hand-write TOML.

use crate::letter::records::{RecordFront, RecordKind};
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

/// A pump record body for `need`; `kind` is "" (none), build, research,
/// release or repair, and anything else panics. The prose is empty.
pub(crate) fn record_body(need: &str, kind: &str) -> String {
    let kind = match kind {
        "" => None,
        "build" => Some(RecordKind::Build),
        "research" => Some(RecordKind::Research),
        "release" => Some(RecordKind::Release),
        "repair" => Some(RecordKind::Repair),
        other => panic!("unknown record kind {other:?}"),
    };
    let front = RecordFront {
        need: need.to_string(),
        kind,
        target: None,
        tested: None,
    };
    render(&front, "").unwrap()
}

/// A pump record body of kind repair.
pub(crate) fn repair_record_body(
    need: &str,
    target: &str,
    tested: Option<&str>,
    prose: &str,
) -> String {
    let front = RecordFront {
        need: need.to_string(),
        kind: Some(RecordKind::Repair),
        target: Some(target.to_string()),
        tested: tested.map(ToString::to_string),
    };
    render(&front, prose).unwrap()
}

/// `body` with the line `need = "<need>"` right after its opening `+++` line,
/// for rigs that build the need themselves; a body that does not open with
/// front matter is returned unchanged.
pub(crate) fn record_with_need(body: &str, need: &str) -> String {
    let Some((first, rest)) = body.split_once('\n') else {
        return body.to_string();
    };
    if first.trim() != "+++" {
        return body.to_string();
    }
    let line = toml::Value::String(need.to_string());
    format!("{first}\nneed = {line}\n{rest}")
}
