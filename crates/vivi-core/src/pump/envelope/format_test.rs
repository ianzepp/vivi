//! Extraction tests for shared envelope formatting and its typed examples.

use super::{repair_audit, role, slicer};
use vivi_mail::letter::review::{Admitter, Auditor, Cto, HeadCeo, Judge, Merge, Researcher};
use vivi_mail::letter::roles::{
    Hand, Planner, Repairer, RoleKind, Slicer, SlicerAnswer, SlicerMode, parse_role_reply,
};
use vivi_mail::letter::{LetterError, split};

fn examples(text: &str) -> Vec<String> {
    let mut inside = false;
    let mut current = String::new();
    let mut found = Vec::new();
    for line in text.lines() {
        if line == "+++" {
            if inside {
                found.push(format!("+++\n{}\n+++", current.trim_end()));
                current.clear();
            }
            inside = !inside;
        } else if inside {
            current.push_str(line);
            current.push('\n');
        }
    }
    found
}

fn raw_front(example: &str) -> toml::Value {
    let (front, _) = split(example).expect("example has a complete envelope");
    toml::from_str(front).expect("example front matter is TOML")
}

#[test]
fn shared_rules_describe_the_reply_boundary_and_literal_strings() {
    let text = role(RoleKind::Hand);
    for rule in [
        "actual reply starts immediately with an opening `+++` line",
        "closes its TOML front matter with a matching `+++` line",
        "`answer` is the first key",
        "Permitted prose follows the envelope",
        "No Markdown preamble or code-fence wrapper surrounds the actual reply",
        "Unknown keys are refused",
        "TOML literal strings `'...'`",
        "multiline literal strings `'''...'''`",
        "non-evidentiary",
        "Lists default to empty",
    ] {
        assert!(text.contains(rule), "missing shared rule: {rule}\n{text}");
    }
}

#[test]
fn slicer_format_examples_are_valid_for_each_mode() {
    for (mode, mode_name, supplied_id) in [
        (SlicerMode::Slice, "slice", "slice-parent"),
        (SlicerMode::Rework, "rework", "slice-existing"),
        (SlicerMode::Split, "split", "parent-7"),
    ] {
        assert_slicer_mode(mode, mode_name, supplied_id);
    }
}

fn assert_slicer_mode(mode: SlicerMode, mode_name: &str, supplied_id: &str) {
    let target = "vivi-target";
    let text = slicer(mode, target, supplied_id);
    assert!(text.contains(&format!("requested mode = `{mode_name}`")));
    if mode == SlicerMode::Split {
        assert!(text.contains("two to four replacement slices"));
    }
    let found = examples(&text);
    assert_eq!(found.len(), 2, "{text}");
    let sliced = parse_role_reply::<Slicer>(&found[0]).unwrap();
    assert_eq!(sliced.front.answer, SlicerAnswer::Sliced);
    assert_eq!(sliced.front.mode, mode);
    assert_eq!(
        sliced.front.slice.len(),
        if mode == SlicerMode::Split { 2 } else { 1 }
    );
    assert_slicer_rows(&found[0], &sliced.front, target, supplied_id, mode);
    let escalate = parse_role_reply::<Slicer>(&found[1]).unwrap();
    assert_eq!(escalate.front.answer, SlicerAnswer::Escalate);
    assert_eq!(escalate.front.mode, mode);
    assert!(escalate.front.slice.is_empty());
    assert!(
        escalate
            .front
            .reason
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty())
    );
}

fn assert_slicer_rows(
    raw: &str,
    reply: &Slicer,
    target: &str,
    supplied_id: &str,
    mode: SlicerMode,
) {
    let front = raw_front(raw);
    let rows = front["slice"].as_array().expect("slice array");
    let expected = ["id", "title", "brief", "target", "write_scope", "deps"];
    for (index, row) in rows.iter().enumerate() {
        let mut keys = row
            .as_table()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        keys.sort_unstable();
        let mut expected_keys = expected.to_vec();
        expected_keys.sort_unstable();
        assert_eq!(keys, expected_keys);
        assert_eq!(row["target"].as_str(), Some(target));
        assert!(
            row["write_scope"]
                .as_array()
                .is_some_and(|scope| !scope.is_empty())
        );
        let id = if mode == SlicerMode::Split {
            format!("{supplied_id}.{}", index + 1)
        } else {
            supplied_id.to_owned()
        };
        assert_eq!(row["id"].as_str(), Some(id.as_str()));
        assert!(row["title"].as_str().is_some_and(|s| !s.trim().is_empty()));
        assert!(row["brief"].as_str().is_some_and(|s| !s.trim().is_empty()));
        assert!(
            reply.slice[index]
                .deps
                .iter()
                .all(|dep| reply.slice.iter().any(|s| &s.id == dep))
        );
    }
    let front_raw = split(raw).unwrap().0;
    assert!(front_raw.contains("title = '") && front_raw.contains("brief = '"));
    if mode == SlicerMode::Split {
        assert_eq!(reply.slice[0].id, format!("{supplied_id}.1"));
        assert_eq!(reply.slice[1].id, format!("{supplied_id}.2"));
    }
}

#[test]
fn role_examples_parse_to_the_declared_typed_fields() {
    for kind in RoleKind::ALL {
        let rendered = role(kind);
        let found = examples(&rendered);
        assert_eq!(found.len(), 1, "{kind:?}: {rendered}");
        let example = &found[0];
        match kind {
            RoleKind::Researcher => {
                let parsed = parse_role_reply::<Researcher>(example).unwrap();
                assert_eq!(
                    parsed.front.answer,
                    vivi_mail::letter::review::ResearcherAnswer::Ready
                );
                assert!(parsed.front.open_questions.is_empty());
                assert!(rendered.contains("`READY` | `NOT READY` | `NEEDS FURTHER REVIEW`"));
            }
            RoleKind::Planner => {
                let parsed = parse_role_reply::<Planner>(example).unwrap();
                assert_eq!(
                    parsed.front.reason.as_deref(),
                    Some("The requested work is complete.")
                );
                assert!(rendered.contains("unless answer is `planned`"));
            }
            RoleKind::Admitter => {
                let parsed = parse_role_reply::<Admitter>(example).unwrap();
                assert_eq!(parsed.front.required_changes.len(), 1);
                assert!(rendered.contains("nonempty for `revise`"));
            }
            RoleKind::Slicer => {
                let parsed = parse_role_reply::<Slicer>(example).unwrap();
                assert_eq!(parsed.front.mode, SlicerMode::Slice);
                assert_eq!(parsed.front.slice[0].write_scope.len(), 1);
                assert!(rendered.contains("do not change either"));
            }
            RoleKind::Auditor => {
                let parsed = parse_role_reply::<Auditor>(example).unwrap();
                assert_eq!(
                    parsed.front.raise_rung,
                    Some(vivi_mail::letter::review::RaiseRung::Need)
                );
                assert_eq!(
                    parsed.front.requirement[0].status,
                    vivi_mail::letter::review::RequirementStatus::Partial
                );
                assert_eq!(
                    parsed.front.finding[0].recheck_trigger,
                    "Re-run the focused check."
                );
                for answer in ["clean_pass", "residual", "block_range", "block_ship"] {
                    assert!(rendered.contains(&format!("`{answer}`")));
                }
            }
            RoleKind::Judge => {
                let parsed = parse_role_reply::<Judge>(example).unwrap();
                assert!(
                    parsed
                        .front
                        .reason
                        .as_deref()
                        .is_some_and(|s| !s.is_empty())
                );
                assert!(!rendered.contains("[[requirement]]"));
                assert!(!rendered.contains("[[finding]]"));
            }
            RoleKind::Cto => {
                let parsed = parse_role_reply::<Cto>(example).unwrap();
                assert!(parsed.front.reason.is_some() && parsed.front.new_fact.is_some());
                for answer in ["research", "plan", "slice", "rework", "lock"] {
                    assert!(rendered.contains(&format!("`{answer}`")));
                }
            }
            RoleKind::Merge => {
                let parsed = parse_role_reply::<Merge>(example).unwrap();
                assert_eq!(
                    parsed.front.check[0].result,
                    vivi_mail::letter::roles::CheckResult::Skipped
                );
                assert!(rendered.contains("publication evidence"));
                assert!(!rendered.contains("published SHA"));
            }
            RoleKind::Hand => {
                parse_role_reply::<Hand>(example).unwrap();
                assert!(rendered.contains("scope_paths` must be nonempty"));
                assert!(rendered.contains("[[follow_up]]"));
            }
            RoleKind::Repairer => {
                let parsed = parse_role_reply::<Repairer>(example).unwrap();
                assert_eq!(
                    parsed.front.answer,
                    vivi_mail::letter::roles::RepairerAnswer::Built
                );
                let front = raw_front(example);
                assert!(!front.as_table().unwrap().contains_key("scope_paths"));
                assert!(!front.as_table().unwrap().contains_key("follow_up"));
                assert!(!front.as_table().unwrap().contains_key("remaining"));
            }
            RoleKind::HeadCeo => {
                let parsed = parse_role_reply::<HeadCeo>(example).unwrap();
                assert!(parsed.front.served.is_empty() && parsed.front.research.is_empty());
                assert!(rendered.contains("`reviewed` | `no-op`"));
            }
        }
    }
}

#[test]
fn repair_audit_examples_parse_as_the_narrow_auditor_subset() {
    let text = repair_audit();
    let found = examples(&text);
    assert_eq!(found.len(), 2);
    let clean = parse_role_reply::<Auditor>(&found[0]).unwrap();
    assert_eq!(
        clean.front.answer,
        vivi_mail::letter::review::AuditorAnswer::CleanPass
    );
    assert!(clean.front.requirement.is_empty());
    let residual = parse_role_reply::<Auditor>(&found[1]).unwrap();
    assert_eq!(
        residual.front.answer,
        vivi_mail::letter::review::AuditorAnswer::Residual
    );
    assert!(residual.front.requirement.is_empty());
    assert!(residual.front.finding[0].id.starts_with("weakened-test"));
    assert!(text.contains("Do not provide or invent a requirement inventory"));
    assert!(text.contains("`clean_pass` | `residual`"));
}

#[test]
fn complete_footers_are_below_the_four_kibibyte_bound() {
    for kind in RoleKind::ALL {
        assert!(role(kind).len() < 4096, "{kind:?} footer too large");
    }
    for mode in [SlicerMode::Slice, SlicerMode::Rework, SlicerMode::Split] {
        assert!(slicer(mode, "vivi", "parent-1").len() < 4096);
    }
    assert!(repair_audit().len() < 4096);
}

#[test]
fn malformed_fields_and_conditional_slicer_answers_are_refused() {
    let invalid = [
        "+++\nanswer = \"READY\"\nunknown = true\n+++",
        "+++\nanswer = \"bad\"\n+++",
    ];
    assert!(parse_role_reply::<Researcher>(invalid[0]).is_err());
    assert!(parse_role_reply::<Researcher>(invalid[1]).is_err());
    let no_reason = "+++\nanswer = \"escalate\"\nmode = \"slice\"\n+++";
    let has_slice = "+++\nanswer = \"escalate\"\nmode = \"slice\"\nreason = 'x'\n[[slice]]\nid = 's1'\ntitle = 't'\nbrief = 'b'\ntarget = 'vivi'\nwrite_scope = ['x']\ndeps = []\n+++";
    assert!(parse_role_reply::<Slicer>(no_reason).is_err());
    assert!(parse_role_reply::<Slicer>(has_slice).is_err());
    assert_eq!(
        parse_role_reply::<Hand>("bare Markdown only"),
        Err(LetterError::NoOpening)
    );
}

#[test]
fn rendered_slicer_examples_keep_literal_text_and_exact_scope_contract() {
    let text = slicer(SlicerMode::Split, "vivi", "parent");
    for clause in [
        "exactly these six keys",
        "`write_scope` is nonempty",
        "every dependency is expressed as a slice ID",
        "title = '",
        "brief = '",
        "reason = '",
        "target = \"vivi\"",
    ] {
        assert!(text.contains(clause), "missing `{clause}` in {text}");
    }
    let sliced = examples(&text).remove(0);
    let reply = parse_role_reply::<Slicer>(&sliced).unwrap();
    assert_eq!(reply.front.slice.len(), 2);
    assert!(
        reply
            .front
            .slice
            .iter()
            .all(|row| row.target == "vivi" && !row.write_scope.is_empty())
    );
}

#[test]
fn bare_markdown_still_returns_no_opening() {
    assert_eq!(split("just Markdown"), Err(LetterError::NoOpening));
}
